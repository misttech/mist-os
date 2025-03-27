// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;

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
}
