// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub fn malloc(size: usize, _hint: u32) -> *mut ::std::os::raw::c_void {
    // The 'hint' parameter allows requesting memory that is zeroed and that's
    // not shared with other TAs. We always zero allocations and don't share
    // memory with other TAs so we ignore the hint.
    unsafe { libc::malloc(size) }
}

/// # Safety
///
/// This wraps libc::realloc and is only safe to call with a pointer value that is NULL or allocated
/// through malloc / realloc.
pub unsafe fn realloc(
    buffer: *mut ::std::os::raw::c_void,
    new_size: usize,
) -> *mut ::std::os::raw::c_void {
    libc::realloc(buffer, new_size)
}

/// # Safety
///
/// This wraps libc::free and is only safe to call with a pointer value that is NULL or allocated
/// through malloc / realloc.
pub unsafe fn free(buffer: *mut ::std::os::raw::c_void) {
    libc::free(buffer)
}

pub fn mem_move(dest: *mut ::std::os::raw::c_void, src: *mut ::std::os::raw::c_void, size: usize) {
    // This is semantically equivalent to libc::memmove() with the order of operands reversed.
    // This uses the Rust library routine instead so that it can be optimized directly if the
    // toolchain decides, calling libc::memmove() would require an external library call here.
    unsafe { std::ptr::copy(src as *const u8, dest as *mut u8, size) }
}

pub fn mem_compare(
    buffer1: *mut ::std::os::raw::c_void,
    buffer2: *mut ::std::os::raw::c_void,
    size: usize,
) -> i32 {
    unsafe {
        let buffer1 = std::slice::from_raw_parts::<u8>(buffer1 as *const u8, size);
        let buffer2 = std::slice::from_raw_parts::<u8>(buffer2 as *const u8, size);
        match buffer1.cmp(buffer2) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

pub fn mem_fill(buffer: *mut ::std::os::raw::c_void, x: u8, size: usize) {
    // This is semantically equivalent to libc::memset() as it's called with a byte sized type.
    // This uses the Rust library routine instead so that it can be optimized directly if the
    // toolchain decides, calling libc::memmove() would require an external library call here.
    unsafe { std::ptr::write_bytes(buffer as *mut u8, x, size) }
}

#[no_mangle]
pub extern "C" fn __scudo_default_options() -> *const std::ffi::c_char {
    b"zero_contents=true\0" as *const u8 as *const std::ffi::c_char
}

#[cfg(test)]
mod test {
    use super::*;
    use std::ffi::c_void;

    fn check_memory_contains(addr: *const c_void, fill: u8, len: usize) {
        for i in 0..len as isize {
            let val = unsafe { std::ptr::read_volatile(addr.byte_offset(i) as *const u8) };
            assert_eq!(val, fill, "offset {i}");
        }
    }

    fn fill_memory(addr: *mut c_void, fill: u8, len: usize) {
        for i in 0..len as isize {
            unsafe { std::ptr::write_volatile(addr.byte_offset(i) as *mut u8, fill) }
        }
    }

    #[fuchsia::test]
    fn malloc_free() {
        const ALLOC_SIZE: usize = 25;
        let buf = malloc(ALLOC_SIZE, 0);
        assert_ne!(buf, std::ptr::null_mut());

        unsafe { free(buf) };
    }

    #[fuchsia::test]
    fn small_malloc_zeroed() {
        const ALLOC_SIZE: usize = 25;
        let buf = malloc(ALLOC_SIZE, 0);
        assert_ne!(buf, std::ptr::null_mut());

        check_memory_contains(buf, 0, ALLOC_SIZE);

        // Fill memory and then free/malloc to check that if the memory is reused for the
        // new allocation that it's still zeroed out.
        fill_memory(buf, 7u8, ALLOC_SIZE);
        unsafe { free(buf) };
        let buf = malloc(ALLOC_SIZE, 0);
        assert_ne!(buf, std::ptr::null_mut());
        check_memory_contains(buf, 0, ALLOC_SIZE);
        unsafe { free(buf) };
    }

    #[fuchsia::test]
    fn large_malloc_zeroed() {
        const ALLOC_SIZE: usize = 1024 * 1024 + 1;
        let buf = malloc(ALLOC_SIZE, 0);
        assert_ne!(buf, std::ptr::null_mut());
        check_memory_contains(buf, 0, ALLOC_SIZE);
        fill_memory(buf, 7u8, ALLOC_SIZE);
        unsafe { free(buf) };
        let buf = malloc(ALLOC_SIZE, 0);
        check_memory_contains(buf, 0, ALLOC_SIZE);
        unsafe { free(buf) };
    }

    #[fuchsia::test]
    fn realloc_grow() {
        let buf = malloc(5, 0);
        assert_ne!(buf, std::ptr::null_mut());
        fill_memory(buf, 7u8, 5);
        let realloced_buf = unsafe { realloc(buf, 5 * 1024) };
        assert_ne!(realloced_buf as usize, 0);
        check_memory_contains(realloced_buf, 7u8, 5);
        check_memory_contains(unsafe { realloced_buf.byte_offset(5) }, 0, 5 * 1024 - 5);

        unsafe { free(realloced_buf) };
    }

    #[fuchsia::test]
    fn realloc_shrink() {
        let buf = malloc(5 * 1024, 0);
        assert_ne!(buf, std::ptr::null_mut());
        fill_memory(buf, 7u8, 5 * 1024);
        let realloced_buf = unsafe { realloc(buf, 5) };
        assert_ne!(realloced_buf, std::ptr::null_mut());
        check_memory_contains(realloced_buf, 7, 5);
        unsafe {
            free(realloced_buf);
        }
    }

    #[fuchsia::test]
    fn mem_move_overlap() {
        // Allocate a buffer 12 bytes long and initialize the first 8 elements, then move 8 bytes from
        // the start of the buffer to an offset of 4 from the start.
        let buf = malloc(12, 0);
        assert_ne!(buf, std::ptr::null_mut());
        fill_memory(buf, 1, 4);
        fill_memory(unsafe { buf.byte_offset(4) }, 2, 4);
        let dest = unsafe { buf.byte_offset(4) };
        mem_move(dest, buf, 8);
        check_memory_contains(buf, 1, 4);
        check_memory_contains(unsafe { buf.byte_offset(4) }, 1, 4);
        check_memory_contains(unsafe { buf.byte_offset(8) }, 2, 4);
        unsafe { free(buf) };
    }

    #[fuchsia::test]
    fn mem_fill_nonzero() {
        let buf = malloc(5, 0);
        assert_ne!(buf, std::ptr::null_mut());
        mem_fill(buf, 0x42, 5);
        check_memory_contains(buf, 0x42, 5);
        unsafe { free(buf) };
    }

    #[fuchsia::test]
    fn mem_compare_tests() {
        let a = &mut [1u8, 2, 3];
        let b = &mut [2u8, 2, 3];
        let c = &mut [2u8, 2, 4];
        let a_ptr = a.as_mut_ptr() as *mut c_void;
        let b_ptr = b.as_mut_ptr() as *mut c_void;
        let c_ptr = c.as_mut_ptr() as *mut c_void;

        assert_eq!(mem_compare(a_ptr, a_ptr, 1), 0);
        assert_eq!(mem_compare(a_ptr, a_ptr, 3), 0);
        assert_eq!(mem_compare(a_ptr, b_ptr, 1), -1);
        assert_eq!(mem_compare(b_ptr, a_ptr, 1), 1);
        assert_eq!(mem_compare(b_ptr, c_ptr, 2), 0);
        assert_eq!(mem_compare(b_ptr, c_ptr, 3), -1);
    }
}
