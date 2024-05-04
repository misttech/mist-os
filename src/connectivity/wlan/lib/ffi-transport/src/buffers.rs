// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::errors::{Error, InvalidFfiBuffer},
    std::{
        marker::{PhantomData, PhantomPinned},
        mem::ManuallyDrop,
        ops::{Deref, DerefMut},
        ptr::{self, NonNull},
        slice,
        sync::atomic::AtomicPtr,
    },
};

#[repr(C)]
pub struct FfiBufferProvider {
    /// Allocate and take ownership of a buffer allocated by the C++ portion of wlansoftmac
    /// with at least `min_capacity` bytes of capacity.
    ///
    /// If the requested allocation is zero bytes, this function should return an `FfiBuffer`
    /// with a null `ctx` pointer indicating the allocation failed.
    ///
    /// The returned `FfiBuffer` contains a pointer whose pointee the caller now owns, unless that
    /// pointer is the null pointer. If the pointer is non-null, then the `Drop` implementation of
    /// `FfiBuffer` ensures its `free` will be called if its dropped. Otherwise, `free` will not,
    /// and should not, be called.
    get_buffer: extern "C" fn(min_capacity: usize) -> FfiBuffer,
}

pub struct BufferProvider {
    ffi: FfiBufferProvider,
}

impl BufferProvider {
    pub fn new(ffi: FfiBufferProvider) -> Self {
        Self { ffi }
    }

    pub fn get_buffer(&self, min_capacity: usize) -> Result<Buffer, Error> {
        let buffer = (self.ffi.get_buffer)(min_capacity);
        if buffer.ctx.is_null() || buffer.data.is_null() {
            Err(Error::NoResources { requested_capacity: min_capacity })
        } else {
            Buffer::try_from(buffer)
        }
    }
}

#[repr(C)]
pub struct FfiBufferCtx {
    _data: [u8; 0],
    _marker: PhantomData<(*const u8, PhantomPinned)>,
}

/// Type that wraps a pointer to a buffer allocated in the C++ portion of wlansoftmac.
#[derive(Debug)]
#[repr(C)]
pub struct FfiBuffer {
    /// Returns the buffer's ownership and free it.
    ///
    /// # Safety
    ///
    /// The `free` function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the `ctx` field in this struct.
    ///
    /// By calling `free`, the caller promises the pointer it's called with is the `ctx` field
    /// in this struct.
    free: unsafe extern "C" fn(ctx: *mut FfiBufferCtx),
    /// Pointer to the buffer allocated in the C++ portion of wlansoftmac and owned by the Rust
    /// portion of wlansoftmac. An `FfiBufferProvider` sets this pointer to null when the allocation
    /// failed.
    ctx: *mut FfiBufferCtx,
    /// Pointer to the start of bytes written in the buffer.
    data: *mut u8,
    /// Capacity of the buffer, starting at `data`.
    capacity: usize,
}

impl Drop for FfiBuffer {
    // Normally, `self.free` will be called from an `Buffer` constructed from a `FfiBuffer`. This `Drop`
    // implementation is provided in case such construction does not occur before `FfiBuffer` goes out of
    // scope.
    fn drop(&mut self) {
        // Safety: This call of `self.free` is safe because it's called on the `ctx` field
        // of this struct only when `ctx` is not null.
        if !self.ctx.is_null() {
            unsafe { (self.free)(self.ctx) };
        }
        self.ctx = ptr::null_mut();
        self.data = ptr::null_mut();
        self.capacity = 0;
    }
}

/// Type that wraps a pointer to a buffer allocated in the C++ portion of wlansoftmac.
#[derive(Debug)]
pub struct Buffer {
    /// Returns the buffer's ownership and free it.
    ///
    /// # Safety
    ///
    /// The `free` function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the `ctx` field in this struct.
    ///
    /// By calling `free`, the caller promises the pointer it's called with is the `ctx` field
    /// in this struct.
    free: unsafe extern "C" fn(ctx: *mut FfiBufferCtx),
    /// Pointer to the buffer allocated in the C++ portion of wlansoftmac and owned by the Rust
    /// portion of wlansoftmac.
    ///
    /// The `ManuallyDrop` type wraps the pointer to allow the drop implementation to move
    /// the `AtomicPtr` out of self in both `Buffer::release` and `drop`.
    ///
    /// The `AtomicPtr` type wraps the pointer so this field, and thus `Buffer`, implements
    /// `Send` and `Sync`. Note that `AtomicPtr` only wraps the pointer and dereferencing the pointer
    /// is still unsafe. However, there is nothing to modify in a deferenced `*mut FfiBufferCtx`
    /// and the `FfiBufferProvider` FFI contract supports sending the `*mut FfiBufferCtx` between
    /// threads. Thus, wrapping this field so that it implements `Send` and `Sync` is safe.
    ctx: ManuallyDrop<AtomicPtr<FfiBufferCtx>>,
    /// Boxed slice containing a buffer of bytes available to read and write.
    data: ManuallyDrop<Box<[u8]>>,
}

impl TryFrom<FfiBuffer> for Buffer {
    type Error = Error;
    fn try_from(buffer: FfiBuffer) -> Result<Self, Self::Error> {
        // Prevent running the `FfiBuffer` drop implementation to prevent calling `buffer.free`.
        let buffer = ManuallyDrop::new(buffer);
        let (ctx, data, capacity) = match (
            NonNull::new(buffer.ctx as *mut FfiBufferCtx),
            NonNull::new(buffer.data),
            buffer.capacity,
        ) {
            (None, _, _) => return Err(Error::InvalidFfiBuffer(InvalidFfiBuffer::NullCtx)),
            (_, None, _) => return Err(Error::InvalidFfiBuffer(InvalidFfiBuffer::NullData)),
            (_, _, 0) => return Err(Error::InvalidFfiBuffer(InvalidFfiBuffer::ZeroCapacity)),
            (Some(ctx), Some(data), capacity) => (ctx, data, capacity),
        };

        Ok(Self {
            free: buffer.free,
            ctx: ManuallyDrop::new(AtomicPtr::new(ctx.as_ptr())),
            // Safety: Forming a boxed slice from `data` and `capacity` is safe because
            // `data` is non-null and `capacity` defines the number of bytes beyond the
            // `data` pointer that are allocated for reading and writing.
            data: ManuallyDrop::new(unsafe {
                Box::from_raw(slice::from_raw_parts_mut(data.as_ptr(), capacity))
            }),
        })
    }
}

impl Buffer {
    /// Transforms `self` into a read-only `FinalizedBuffer` with `written` number of bytes starting
    /// from the beginning of `self.data`.
    ///
    /// # Panics
    ///
    /// This function will panic if `written` is greater than `self.len()`.
    pub fn finalize(self, written: usize) -> FinalizedBuffer {
        if written > self.len() {
            // A panic is appropriate here because otherwise, its likely corruption has already
            // taken place and could manifest in undefined behavior at some unknown point later.
            panic!("Buffer capacity exceeded: written {}, capacity {}", written, self.len());
        }
        FinalizedBuffer { inner: self, written }
    }

    /// Returns the pointer for the buffer allocation obtained from C++ and its corresponding free
    /// function.
    ///
    /// # Safety
    ///
    /// This function is unsafe because the returned `*mut FfiBufferCtx` must either be sent back to the
    /// C++ portion of wlansoftmac or the returned free function must be called on it. Otherwise,
    /// its memory will be leaked.
    ///
    /// By calling this function, the caller promises to send the `*mut FfiBufferCtx` back to the C++
    /// portion of wlansoftmac or call the returned free function on it.
    ///
    /// The returned free function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the same pointer it was returned with.
    ///
    /// By calling the returned free function, the caller promises the pointer it's called with is
    /// the same pointer it was returned with.
    unsafe fn release(self) -> (*mut FfiBufferCtx, unsafe extern "C" fn(ctx: *mut FfiBufferCtx)) {
        let mut me = ManuallyDrop::new(self);
        (
            // Safety: This call is safe because `me.ctx` will not be used again.
            unsafe { ManuallyDrop::take(&mut me.ctx) }.into_inner(),
            me.free,
        )
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // Safety: This call is safe because `self.ctx` will not be used again.
        let ctx = unsafe { ManuallyDrop::take(&mut self.ctx) }.into_inner();

        // Safety: This call of `self.free` is safe because it's called on the `ctx` field
        // of this struct which was verified to be non-null upon construction.
        unsafe { (self.free)(ctx) };
    }
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

/// Type that wraps a pointer to a buffer allocated in the C++ portion of wlansoftmac.
#[derive(Debug)]
pub struct FinalizedBuffer {
    /// Private `Buffer` of which only the first `written` bytes can be read.
    inner: Buffer,
    /// Number of contiguous bytes written to the buffer starting from the beginning.
    written: usize,
}

impl FinalizedBuffer {
    /// Returns a tuple of the following:
    ///
    ///   - Pointer for the buffer allocation obtained from C++.
    ///   - The pointer's corresponding free function
    ///   - Number of bytes written to the buffer.
    ///
    /// # Safety
    ///
    /// This function is unsafe because the returned `*mut FfiBufferCtx` must either be sent back to the
    /// C++ portion of wlansoftmac or the returned free function must be called on it. Otherwise,
    /// its memory will be leaked.
    ///
    /// By calling this function, the caller promises to send the `*mut FfiBufferCtx` back to the C++
    /// portion of wlansoftmac or call the returned free function on it.
    ///
    /// The returned free function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the same pointer it was returned with.
    ///
    /// By calling the returned free function, the caller promises the pointer it's called with is
    /// the same pointer it was returned with.
    pub unsafe fn release(
        self,
    ) -> (*mut FfiBufferCtx, unsafe extern "C" fn(ctx: *mut FfiBufferCtx), usize) {
        let (ctx, free) = self.inner.release();
        (ctx, free, self.written)
    }
}

impl Deref for FinalizedBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.inner[..self.written]
    }
}

// Assert both `Buffer` and `FinalizedBuffer` implement `Send` and `Sync` here to
// provide an obvious compile-time error when either of them does not. This is helpful
// when trying to integrate these types into futures running on a multi-threaded
// executor.
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<Buffer>;
    let _ = assert_send_sync::<FinalizedBuffer>;
};

pub struct FakeFfiBufferProvider;

impl FakeFfiBufferProvider {
    pub fn new() -> FfiBufferProvider {
        FfiBufferProvider { get_buffer: Self::get_buffer }
    }

    /// # Safety
    ///
    /// This function is unsafe because the function cannot guarantee `ctx` was a pointer acquired
    /// from unboxing a `Box<Vec<u8>>`.
    ///
    /// The caller of this function promises `Box::from_raw(ctx as *mut Vec<u8>)` will be valid.
    pub unsafe extern "C" fn free(ctx: *mut FfiBufferCtx) {
        let _ = Box::from_raw(ctx as *mut Vec<u8>);
    }

    pub extern "C" fn get_buffer(min_capacity: usize) -> FfiBuffer {
        if min_capacity == 0 {
            return FfiBuffer {
                free: Self::free,
                ctx: ptr::null_mut(),
                data: ptr::null_mut(),
                capacity: 0,
            };
        }

        let mut data = Box::new(vec![0u8; min_capacity]);
        let data_ptr = data.as_mut_ptr();
        let ptr = Box::into_raw(data);
        FfiBuffer {
            free: Self::free,
            ctx: ptr as *mut FfiBufferCtx,
            data: data_ptr,
            capacity: min_capacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::{cell::RefCell, ptr, rc::Rc},
        test_case::test_case,
    };

    unsafe extern "C" fn no_op_free(_ctx: *mut FfiBufferCtx) {}

    fn free_test(
        test_fn: impl FnOnce(
            unsafe extern "C" fn(*mut FfiBufferCtx),
            Rc<RefCell<Option<*mut FfiBufferCtx>>>,
        ),
    ) {
        thread_local! {
            static FREED_BUFFER: Rc<RefCell<Option<*mut FfiBufferCtx>>> = Rc::new(RefCell::new(None));
        }
        unsafe extern "C" fn free(ctx: *mut FfiBufferCtx) {
            FREED_BUFFER.with(|c| c.borrow_mut().replace(ctx));
        }
        let freed_buffer: Rc<RefCell<Option<*mut FfiBufferCtx>>> = FREED_BUFFER.with(|c| c.clone());
        test_fn(free, freed_buffer);
        FREED_BUFFER.with(|c| c.borrow_mut().take());
    }

    #[test]
    fn dropping_ffi_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            drop(FfiBuffer {
                free,
                ctx: 42 as *mut FfiBufferCtx,
                data: ptr::null_mut(),
                capacity: 0,
            });
            assert_eq!(42 as *mut FfiBufferCtx, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn successful_conversion_into_buffer() {
        let ffi_buffer_provider = FakeFfiBufferProvider::new();
        let _ = Buffer::try_from((ffi_buffer_provider.get_buffer)(10)).unwrap();
    }

    #[test_case(ptr::null_mut(), 754 as *mut u8, 20)]
    #[test_case(754 as *mut FfiBufferCtx, ptr::null_mut(), 20)]
    #[test_case(754 as *mut FfiBufferCtx, 755 as *mut u8, 0)]
    fn failed_conversion_into_buffer(ctx: *mut FfiBufferCtx, data: *mut u8, capacity: usize) {
        let buffer = FfiBuffer { free: no_op_free, ctx, data, capacity };
        let error = Buffer::try_from(buffer).unwrap_err();
        if ctx.is_null() {
            assert!(matches!(error, Error::InvalidFfiBuffer(InvalidFfiBuffer::NullCtx)));
        } else if data.is_null() {
            assert!(matches!(error, Error::InvalidFfiBuffer(InvalidFfiBuffer::NullData)));
        } else if capacity == 0 {
            assert!(matches!(error, Error::InvalidFfiBuffer(InvalidFfiBuffer::ZeroCapacity)));
        }
    }

    #[test]
    fn correct_representation_of_immutable_buffer_data_by_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let buffer = Buffer::try_from(FfiBuffer {
            free: no_op_free,
            ctx: 42 as *mut FfiBufferCtx,
            data: data.as_mut_ptr(),
            capacity: data.len(),
        })
        .unwrap();
        assert_eq!(&buffer[..], &data[..]);
    }

    #[test]
    fn correct_representation_of_mutable_buffer_data_by_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let mut buffer = Buffer::try_from(FfiBuffer {
            free: no_op_free,
            ctx: 42 as *mut FfiBufferCtx,
            data: data.as_mut_ptr(),
            capacity: data.len(),
        })
        .unwrap();
        assert_eq!(&mut buffer[..], &mut data[..]);
    }

    #[test]
    fn successful_modification_of_buffer() {
        let ffi_buffer_provider = FakeFfiBufferProvider::new();
        let mut buffer = Buffer::try_from((ffi_buffer_provider.get_buffer)(10)).unwrap();
        assert_eq!(buffer.len(), 10);
        let buffer_copy = buffer.to_vec();
        assert_eq!(&buffer[..], &buffer_copy[..]);
        buffer[2] = 3;
        assert_eq!(buffer[2], 3);
        assert_ne!(&buffer[..], &buffer_copy[..]);

        buffer.clone_from_slice(&buffer_copy[..]);
        assert_eq!(&buffer[..], &buffer_copy[..]);
    }

    #[test]
    fn releasing_buffer_does_not_call_free() {
        free_test(|free, freed_buffer| {
            let buffer = Buffer::try_from(FfiBuffer {
                free,
                ctx: 42 as *mut FfiBufferCtx,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap();
            let _ = unsafe { buffer.release() };
            assert!(freed_buffer.borrow().is_none());
        });
    }

    #[test]
    fn dropping_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            drop(
                Buffer::try_from(FfiBuffer {
                    free,
                    ctx: 42 as *mut FfiBufferCtx,
                    data: 43 as *mut u8,
                    capacity: 10,
                })
                .unwrap(),
            );
            assert_eq!(42 as *mut FfiBufferCtx, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn freeing_released_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            let buffer = Buffer::try_from(FfiBuffer {
                free,
                ctx: 42 as *mut FfiBufferCtx,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap();
            unsafe {
                let (ctx, free) = buffer.release();
                free(ctx);
            }
            assert_eq!(42 as *mut FfiBufferCtx, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn successful_conversion_into_finalized_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let _ = Buffer::try_from(FfiBuffer {
            free: no_op_free,
            ctx: 42 as *mut FfiBufferCtx,
            data: data.as_mut_ptr(),
            capacity: data.len(),
        })
        .unwrap()
        .finalize(4);
    }

    #[test]
    #[should_panic(expected = "Buffer capacity exceeded: written 5, capacity 4")]
    fn failed_conversion_into_finalized_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let _ = Buffer::try_from(FfiBuffer {
            free: no_op_free,
            ctx: 42 as *mut FfiBufferCtx,
            data: data.as_mut_ptr(),
            capacity: data.len(),
        })
        .unwrap()
        .finalize(5);
    }

    #[test]
    fn dropping_finalized_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            drop(
                Buffer::try_from(FfiBuffer {
                    free,
                    ctx: 42 as *mut FfiBufferCtx,
                    data: 43 as *mut u8,
                    capacity: 10,
                })
                .unwrap()
                .finalize(5),
            );
            assert_eq!(42 as *mut FfiBufferCtx, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn releasing_finalized_buffer_does_not_call_free() {
        free_test(|free, freed_buffer| {
            let buffer = Buffer::try_from(FfiBuffer {
                free,
                ctx: 42 as *mut FfiBufferCtx,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap()
            .finalize(5);
            let _ = unsafe { buffer.release() };
            assert!(freed_buffer.borrow().is_none());
        });
    }

    #[test]
    fn released_finalized_buffer_returns_correct_written_bytes() {
        free_test(|free, _freed_buffer| {
            let buffer = Buffer::try_from(FfiBuffer {
                free,
                ctx: 42 as *mut FfiBufferCtx,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap()
            .finalize(5);
            let (_, _, written) = unsafe { buffer.release() };
            assert_eq!(written, 5);
        });
    }

    #[test]
    fn freeing_released_finalized_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            let buffer = Buffer::try_from(FfiBuffer {
                free,
                ctx: 42 as *mut FfiBufferCtx,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap()
            .finalize(5);
            unsafe {
                let (ctx, free, _) = buffer.release();
                free(ctx);
            }
            assert_eq!(42 as *mut FfiBufferCtx, freed_buffer.borrow_mut().take().unwrap());
        });
    }
}
