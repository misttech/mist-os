// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;
use std::ops::Range;
use zerocopy::{FromBytes, IntoBytes};

#[cfg(target_arch = "aarch64")]
mod arm64;

#[cfg(target_arch = "aarch64")]
use arm64 as arch;

#[cfg(target_arch = "x86_64")]
mod x64;

#[cfg(target_arch = "x86_64")]
use x64 as arch;

#[cfg(target_arch = "riscv64")]
mod riscv64;

#[cfg(target_arch = "riscv64")]
use riscv64 as arch;

/// Pointer to a buffer that may be shared between eBPF programs. It allows to
/// safely pass around pointers to the data stored in eBPF maps and access the
/// data referenced by the pointer.
pub struct EbpfPtr<'a, T> {
    ptr: *mut T,
    phantom: PhantomData<&'a T>,
}

unsafe impl<'a, T> Send for EbpfPtr<'a, T> {}
unsafe impl<'a, T> Sync for EbpfPtr<'a, T> {}

impl<'a, T> EbpfPtr<'a, T> {
    /// Creates a new `EbpfPtr` from the specified pointer.
    ///
    /// # Safety
    /// Caller must ensure that the buffer referenced by `ptr` is valid for
    /// lifetime `'a`.
    pub unsafe fn new(ptr: *mut T) -> Self {
        Self { ptr, phantom: PhantomData }
    }

    /// # Safety
    /// Caller must ensure that the value cannot be updated by other threads
    /// while the returned reference is live.
    pub unsafe fn deref(&self) -> &'a T {
        &*self.ptr
    }

    /// # Safety
    /// Caller must ensure that the value is not being used by other threads.
    pub unsafe fn deref_mut(&self) -> &'a mut T {
        &mut *self.ptr
    }
}

impl EbpfPtr<'_, u64> {
    /// Loads the value referenced by the pointer. Atomicity is guaranteed
    /// if and only if the pointer is 8-byte aligned.
    pub fn load_relaxed(&self) -> u64 {
        unsafe { arch::load_u64(self.ptr) }
    }

    /// Stores the `value` at the memory referenced by the pointer. Atomicity
    /// is guaranteed if and only if the pointer is 8-byte aligned.
    pub fn store_relaxed(&self, value: u64) {
        unsafe { arch::store_u64(self.ptr, value) }
    }
}

/// Wraps a pointer to buffer used in eBPF runtime, such as an eBPF maps
/// entry. The referenced data may be access from multiple threads in parallel,
/// which makes it unsafe to access it using standard Rust types.
/// `EbpfBufferPtr` allows to access these buffers safely. It may be used to
/// reference either a whole VMO allocated for and eBPF map or individual
/// elements of that VMO (see `slice()`). The address and the size of the
/// buffer are always 8-byte aligned.
#[derive(Clone)]
pub struct EbpfBufferPtr<'a> {
    ptr: *mut u8,
    size: usize,
    phantom: PhantomData<&'a u8>,
}

impl<'a> EbpfBufferPtr<'a> {
    pub const ALIGNMENT: usize = size_of::<u64>();

    /// Creates a new `EbpfBufferPtr` from the specified pointer. `ptr` must be
    /// 8-byte aligned. `size` must be multiple of 8.
    ///
    /// # Safety
    /// Caller must ensure that the buffer referenced by `ptr` is valid for
    /// lifetime `'a`.
    pub unsafe fn new(ptr: *mut u8, size: usize) -> Self {
        assert!((ptr as usize) % Self::ALIGNMENT == 0);
        assert!(size % Self::ALIGNMENT == 0);
        assert!(size < isize::MAX as usize);
        Self { ptr, size, phantom: PhantomData }
    }

    /// Size of the buffer in bytes.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Raw pointer to the start of the buffer.
    pub fn raw_ptr(&self) -> *mut u8 {
        self.ptr
    }

    // SAFETY: caller must ensure that the value at the specified offset fits
    // the buffer.
    unsafe fn get_ptr_internal<T>(&self, offset: usize) -> EbpfPtr<'a, T> {
        EbpfPtr::new(self.ptr.byte_offset(offset as isize) as *mut T)
    }

    /// Returns a pointer to a value of type `T` at the specified `offset`.
    pub fn get_ptr<T>(&self, offset: usize) -> Option<EbpfPtr<'a, T>> {
        if offset + std::mem::size_of::<T>() <= self.size {
            // SAFETY: Buffer bounds are verified above.
            Some(unsafe { self.get_ptr_internal(offset) })
        } else {
            None
        }
    }

    /// Returns pointer to the specified range in the buffer.
    /// Range bounds must be multiple of 8.
    pub fn slice(&self, range: Range<usize>) -> Option<Self> {
        assert!(range.start <= range.end);
        (range.end <= self.size).then(|| {
            // SAFETY: Returned buffer has the same lifetime as `self`, which
            // ensures that the `ptr` stays valid for the lifetime of the
            // result.
            unsafe {
                Self {
                    ptr: self.ptr.byte_offset(range.start as isize),
                    size: range.end - range.start,
                    phantom: PhantomData,
                }
            }
        })
    }

    /// Loads contents of the buffer to a `Vec<u8>`.
    pub fn load(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.size);

        for pos in (0..self.size).step_by(Self::ALIGNMENT) {
            // SAFETY: the offset is guaranteed to be within the buffer bounds.
            let value: u64 = unsafe { self.get_ptr_internal(pos).load_relaxed() };
            result.extend_from_slice(value.as_bytes());
        }

        result
    }

    /// Stores `data` in the buffer. `data` must be the same size as the
    /// buffer.
    pub fn store(&self, data: &[u8]) {
        assert!(data.len() == self.size);
        self.store_padded(data);
    }

    /// Stores `data` at the head of the buffer. If `data` is not multiple of 8
    /// then it's padded at the end with zeros.
    pub fn store_padded(&self, data: &[u8]) {
        assert!(data.len() <= self.size);

        let tail = data.len() % 8;
        let end = data.len() - tail;
        for pos in (0..end).step_by(Self::ALIGNMENT) {
            let value = u64::read_from_bytes(&data[pos..(pos + 8)]).unwrap();
            // SAFETY: pos is guaranteed to be within the buffer bounds.
            unsafe { self.get_ptr_internal(pos).store_relaxed(value) };
        }

        if tail > 0 {
            let mut value: u64 = 0;
            value.as_mut_bytes()[..tail].copy_from_slice(&data[(data.len() - tail)..]);
            // SAFETY: pos is guaranteed to be within the buffer bounds.
            unsafe { self.get_ptr_internal(data.len() - tail).store_relaxed(value) };
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fuchsia_runtime::vmar_root_self;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn test_u64_atomicity() {
        let vmo_size = zx::system_get_page_size() as usize;
        let vmo = zx::Vmo::create(vmo_size as u64).unwrap();
        let addr = vmar_root_self()
            .map(0, &vmo, 0, vmo_size, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
            .unwrap();
        let shared_ptr = unsafe { EbpfPtr::new(addr as *mut u64) };

        const NUM_THREADS: usize = 10;

        // Barrier used to synchronize start of the threads.
        let barrier = Barrier::new(NUM_THREADS * 2);

        let finished_writers = AtomicU32::new(0);

        thread::scope(|scope| {
            let mut threads = Vec::new();

            for _ in 0..10 {
                threads.push(scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..1000 {
                        for i in 0..255 {
                            // Store a value with the same value repeated in every byte.
                            let v = i << 8 | i;
                            let v = v << 16 | v;
                            let v = v << 32 | v;
                            shared_ptr.store_relaxed(v);
                        }
                    }
                    finished_writers.fetch_add(1, Ordering::Relaxed);
                }));

                threads.push(scope.spawn(|| {
                    barrier.wait();
                    loop {
                        for _ in 0..1000 {
                            let v = shared_ptr.load_relaxed();
                            // Verify that all bytes in `v` are set to the same.
                            assert!(v >> 32 == v & 0xffff_ffff);
                            assert!((v >> 16) & 0xffff == v & 0xffff);
                            assert!((v >> 8) & 0xff == v & 0xff);
                        }
                        if finished_writers.load(Ordering::Relaxed) == NUM_THREADS as u32 {
                            break;
                        }
                    }
                }));
            }

            for t in threads.into_iter() {
                t.join().expect("failed to join a test thread");
            }
        });

        unsafe { vmar_root_self().unmap(addr, vmo_size).unwrap() };
    }

    #[test]
    fn test_buffer_slice() {
        const SIZE: usize = 32;

        let mut buf = [0; SIZE];
        let buf_ptr = unsafe { EbpfBufferPtr::new(buf.as_mut_ptr(), SIZE) };

        buf_ptr.slice(8..16).unwrap().store(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            buf_ptr.slice(0..24).unwrap().load(),
            [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0]
        );

        assert!(buf_ptr.slice(8..40).is_none());
    }

    #[test]
    fn test_buffer_load() {
        const SIZE: usize = 32;

        let mut buf = (0..(SIZE as u8)).map(|v| v as u8).collect::<Vec<_>>();
        let buf_ptr = unsafe { EbpfBufferPtr::new(buf.as_mut_ptr(), SIZE) };
        let v = buf_ptr.load();
        assert_eq!(v, (0..SIZE).map(|v| v as u8).collect::<Vec<_>>());
    }

    #[test]
    fn test_buffer_store() {
        const SIZE: usize = 32;

        let mut buf = [0u8; SIZE];
        let buf_ptr = unsafe { EbpfBufferPtr::new(buf.as_mut_ptr(), SIZE) };

        // Write values from `s` to `e` to range `s..e`.
        buf_ptr.store(&(0..SIZE).map(|v| v as u8).collect::<Vec<_>>());

        // Read the content and verify that it matches the expectation.
        let data = buf_ptr.load();
        assert_eq!(&data, &(0..SIZE).map(|v| v as u8).collect::<Vec<_>>());
    }
}
