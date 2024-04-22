// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::Error,
    anyhow::format_err,
    std::{
        ffi::c_void,
        mem::{self, ManuallyDrop},
        ops::{Deref, DerefMut},
        ptr::NonNull,
        slice,
        sync::atomic::AtomicPtr,
    },
};

#[repr(C)]
pub struct CBufferProvider {
    /// Allocate and take ownership of a buffer allocated by the C++ portion of wlansoftmac
    /// with at least `min_capacity` bytes of capacity.
    ///
    /// The returned `CBuffer` contains a pointer whose pointee the caller now owns, unless that
    /// pointer is the null pointer. If the pointer is non-null, then the `Drop` implementation of
    /// `CBuffer` ensures its `free` will be called if its dropped. If the pointer is null, the
    /// allocation failed, and the caller must discard the `CBuffer` without calling its `Drop`
    /// implementation.
    ///
    /// # Safety
    ///
    /// This function is unsafe because the returned `CBuffer` could contain null pointers,
    /// indicating the allocation failed.
    ///
    /// By calling this function, the caller promises to call `mem::forget` on the returned
    /// `CBuffer` if either the `raw` or `data` fields in the `CBuffer` are null.
    get_buffer: unsafe extern "C" fn(min_capacity: usize) -> CBuffer,
}

impl CBufferProvider {
    pub fn get_buffer(&self, min_capacity: usize) -> Result<Buffer, Error> {
        // Safety: This call is safe because this function checks if `buffer` contains
        // a null pointer and calls `mem::forget(buffer)` to prevent calling the `Drop`
        // implementation if it does.
        let buffer = unsafe { (self.get_buffer)(min_capacity) };
        if buffer.raw.is_null() || buffer.data.is_null() {
            mem::forget(buffer);
            Err(Error::NoResources(min_capacity))
        } else {
            Buffer::try_from(buffer)
        }
    }
}

/// Type that wraps a pointer to a buffer allocated in the C++ portion of wlansoftmac.
#[derive(Debug)]
#[repr(C)]
pub struct CBuffer {
    /// Returns the buffer's ownership and free it.
    ///
    /// # Safety
    ///
    /// The `free` function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the `raw` field in this struct.
    ///
    /// By calling `free`, the caller promises the pointer it's called with is the `raw` field
    /// in this struct.
    free: unsafe extern "C" fn(raw: *mut c_void),
    /// Pointer to the buffer allocated in the C++ portion of wlansoftmac and owned by the Rust
    /// portion of wlansoftmac.
    raw: *mut c_void,
    /// Pointer to the start of bytes written in the buffer.
    data: *mut u8,
    /// Capacity of the buffer, starting at `data`.
    capacity: usize,
}

impl Drop for CBuffer {
    // Normally, `self.free` will be called from an `Buffer` constructed from a `CBuffer`. This `Drop`
    // implementation is provided in case such construction does not occur before `CBuffer` goes out of
    // scope.
    fn drop(&mut self) {
        // Safety: This call of `self.free` is safe because it's called on the `raw` field
        // of this struct.
        unsafe { (self.free)(self.raw) };
        self.raw = std::ptr::null_mut();
        self.data = std::ptr::null_mut();
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
    /// called with is the `raw` field in this struct.
    ///
    /// By calling `free`, the caller promises the pointer it's called with is the `raw` field
    /// in this struct.
    free: unsafe extern "C" fn(raw: *mut c_void),
    /// Pointer to the buffer allocated in the C++ portion of wlansoftmac and owned by the Rust
    /// portion of wlansoftmac.
    ///
    /// The `ManuallyDrop` type wraps the pointer to allow the drop implementation to move
    /// the `AtomicPtr` out of self in both `Buffer::release` and `drop`.
    ///
    /// The `AtomicPtr` type wraps the pointer so this field, and thus `Buffer`, implements
    /// `Send` and `Sync`. Note that `AtomicPtr` only wraps the pointer and dereferencing the pointer
    /// is still unsafe. However, Rust code cannot meaningfully dereference a `*mut c_void`
    /// and the `CBufferProvider` FFI contract supports sending the `*mut c_void` between
    /// threads. Thus, wrapping this field so that it implements `Send` and `Sync` is safe.
    raw: ManuallyDrop<AtomicPtr<c_void>>,
    /// Boxed slice containing a buffer of bytes available to read and write.
    data: ManuallyDrop<Box<[u8]>>,
}

impl TryFrom<CBuffer> for Buffer {
    type Error = Error;
    fn try_from(buffer: CBuffer) -> Result<Self, Self::Error> {
        // Prevent running the `CBuffer` drop implementation to prevent calling `buffer.free`.
        let buffer = ManuallyDrop::new(buffer);
        let (raw, data, capacity) = match (
            NonNull::new(buffer.raw as *mut c_void),
            NonNull::new(buffer.data),
            buffer.capacity,
        ) {
            (None, _, _) => return Err(Error::Internal(format_err!("CBuffer.raw is null"))),
            (_, None, _) => return Err(Error::Internal(format_err!("CBuffer.data is null"))),
            (_, _, 0) => return Err(Error::Internal(format_err!("CBuffer.capacity is zero"))),
            (Some(raw), Some(data), capacity) => (raw, data, capacity),
        };

        Ok(Self {
            free: buffer.free,
            raw: ManuallyDrop::new(AtomicPtr::new(raw.as_ptr())),
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
    /// This function is unsafe because the returned `*mut c_void` must either be sent back to the
    /// C++ portion of wlansoftmac or the returned free function must be called on it. Otherwise,
    /// its memory will be leaked.
    ///
    /// By calling this function, the caller promises to send the `*mut c_void` back to the C++
    /// portion of wlansoftmac or call the returned free function on it.
    ///
    /// The returned free function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the same pointer it was returned with.
    ///
    /// By calling the returned free function, the caller promises the pointer it's called with is
    /// the same pointer it was returned with.
    unsafe fn release(self) -> (*mut c_void, unsafe extern "C" fn(raw: *mut c_void)) {
        let mut me = ManuallyDrop::new(self);
        (
            // Safety: This call is safe because `me.raw` will not be used again.
            unsafe { ManuallyDrop::take(&mut me.raw) }.into_inner(),
            me.free,
        )
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // Safety: This call is safe because `self.raw` will not be used again.
        let raw = unsafe { ManuallyDrop::take(&mut self.raw) }.into_inner();

        // Safety: This call of `self.free` is safe because it's called on the `raw` field
        // of this struct.
        unsafe { (self.free)(raw) };
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
    /// This function is unsafe because the returned `*mut c_void` must either be sent back to the
    /// C++ portion of wlansoftmac or the returned free function must be called on it. Otherwise,
    /// its memory will be leaked.
    ///
    /// By calling this function, the caller promises to send the `*mut c_void` back to the C++
    /// portion of wlansoftmac or call the returned free function on it.
    ///
    /// The returned free function is unsafe because the function cannot guarantee the pointer it's
    /// called with is the same pointer it was returned with.
    ///
    /// By calling the returned free function, the caller promises the pointer it's called with is
    /// the same pointer it was returned with.
    pub unsafe fn release(self) -> (*mut c_void, unsafe extern "C" fn(raw: *mut c_void), usize) {
        let (raw, free) = self.inner.release();
        (raw, free, self.written)
    }
}

impl Deref for FinalizedBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.inner[..self.written]
    }
}

// Assert both `Buffer` and `FinalizedBuffer` implement `Send` and `Sync` here to
// provide an obvious compile-time error when either of them does not. When using
// a multi-threaded executor, these two types are generally required to be `Send`
// and `Sync`.
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<Buffer>;
    let _ = assert_send_sync::<FinalizedBuffer>;
};

pub struct FakeCBufferProvider;

impl FakeCBufferProvider {
    pub fn new() -> CBufferProvider {
        CBufferProvider { get_buffer: Self::get_buffer }
    }

    /// # Safety
    ///
    /// This function is unsafe because the function cannot guarantee `raw` was a pointer acquired
    /// from unboxing a `Box<Vec<u8>>`.
    ///
    /// The caller of this function promises `Box::from_raw(raw as *mut Vec<u8>)` will be valid.
    pub unsafe extern "C" fn free(raw: *mut c_void) {
        let _ = Box::from_raw(raw as *mut Vec<u8>);
    }

    pub extern "C" fn get_buffer(min_capacity: usize) -> CBuffer {
        let mut data = Box::new(vec![0u8; min_capacity]);
        let data_ptr = data.as_mut_ptr();
        let ptr = Box::into_raw(data);
        CBuffer {
            free: Self::free,
            raw: ptr as *mut c_void,
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

    unsafe extern "C" fn no_op_free(_raw: *mut c_void) {}

    fn free_test(
        test_fn: impl FnOnce(unsafe extern "C" fn(*mut c_void), Rc<RefCell<Option<*mut c_void>>>),
    ) {
        thread_local! {
            static FREED_BUFFER: Rc<RefCell<Option<*mut c_void>>> = Rc::new(RefCell::new(None));
        }
        unsafe extern "C" fn free(raw: *mut c_void) {
            FREED_BUFFER.with(|c| c.borrow_mut().replace(raw));
        }
        let freed_buffer: Rc<RefCell<Option<*mut c_void>>> = FREED_BUFFER.with(|c| c.clone());
        test_fn(free, freed_buffer);
        FREED_BUFFER.with(|c| c.borrow_mut().take());
    }

    #[test]
    fn dropping_raw_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            drop(CBuffer { free, raw: 42 as *mut c_void, data: ptr::null_mut(), capacity: 0 });
            assert_eq!(42 as *mut c_void, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn successful_conversion_into_buffer() {
        let _ = Buffer::try_from(FakeCBufferProvider::get_buffer(10)).unwrap();
    }

    #[test_case(ptr::null_mut(), 754 as *mut u8, 20)]
    #[test_case(754 as *mut c_void, ptr::null_mut(), 20)]
    #[test_case(754 as *mut c_void, 755 as *mut u8, 0)]
    fn failed_conversion_into_buffer(raw: *mut c_void, data: *mut u8, capacity: usize) {
        let buffer = CBuffer { free: no_op_free, raw, data, capacity };
        let error = Buffer::try_from(buffer).unwrap_err().to_string();
        if raw.is_null() {
            assert!(error.contains("raw is null"));
        } else if data.is_null() {
            assert!(error.contains("data is null"));
        } else if capacity == 0 {
            assert!(error.contains("capacity is zero"));
        }
    }

    #[test]
    fn correct_representation_of_immutable_buffer_data_by_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let buffer = Buffer::try_from(CBuffer {
            free: no_op_free,
            raw: 42 as *mut c_void,
            data: data.as_mut_ptr(),
            capacity: data.len(),
        })
        .unwrap();
        assert_eq!(&buffer[..], &data[..]);
    }

    #[test]
    fn correct_representation_of_mutable_buffer_data_by_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let mut buffer = Buffer::try_from(CBuffer {
            free: no_op_free,
            raw: 42 as *mut c_void,
            data: data.as_mut_ptr(),
            capacity: data.len(),
        })
        .unwrap();
        assert_eq!(&mut buffer[..], &mut data[..]);
    }

    #[test]
    fn successful_modification_of_buffer() {
        let mut buffer = Buffer::try_from(FakeCBufferProvider::get_buffer(10)).unwrap();
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
            let buffer = Buffer::try_from(CBuffer {
                free,
                raw: 42 as *mut c_void,
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
                Buffer::try_from(CBuffer {
                    free,
                    raw: 42 as *mut c_void,
                    data: 43 as *mut u8,
                    capacity: 10,
                })
                .unwrap(),
            );
            assert_eq!(42 as *mut c_void, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn freeing_released_buffer_calls_free() {
        free_test(|free, freed_buffer| {
            let buffer = Buffer::try_from(CBuffer {
                free,
                raw: 42 as *mut c_void,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap();
            unsafe {
                let (raw, free) = buffer.release();
                free(raw);
            }
            assert_eq!(42 as *mut c_void, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn successful_conversion_into_finalized_buffer() {
        let mut data = [0u8, 1, 2, 3];
        let _ = Buffer::try_from(CBuffer {
            free: no_op_free,
            raw: 42 as *mut c_void,
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
        let _ = Buffer::try_from(CBuffer {
            free: no_op_free,
            raw: 42 as *mut c_void,
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
                Buffer::try_from(CBuffer {
                    free,
                    raw: 42 as *mut c_void,
                    data: 43 as *mut u8,
                    capacity: 10,
                })
                .unwrap()
                .finalize(5),
            );
            assert_eq!(42 as *mut c_void, freed_buffer.borrow_mut().take().unwrap());
        });
    }

    #[test]
    fn releasing_finalized_buffer_does_not_call_free() {
        free_test(|free, freed_buffer| {
            let buffer = Buffer::try_from(CBuffer {
                free,
                raw: 42 as *mut c_void,
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
            let buffer = Buffer::try_from(CBuffer {
                free,
                raw: 42 as *mut c_void,
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
            let buffer = Buffer::try_from(CBuffer {
                free,
                raw: 42 as *mut c_void,
                data: 43 as *mut u8,
                capacity: 10,
            })
            .unwrap()
            .finalize(5);
            unsafe {
                let (raw, free, _) = buffer.release();
                free(raw);
            }
            assert_eq!(42 as *mut c_void, freed_buffer.borrow_mut().take().unwrap());
        });
    }
}
