// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use std::ffi::{c_char, c_void};
use zx::sys::{zx_handle_t, zx_status_t, zx_vaddr_t};
use zx::{self as zx, AsHandleRef};

#[cfg(target_arch = "aarch64")]
fn untag_ptr(ptr: u64) -> u64 {
    ptr & !(0xFFu64 << 56)
}

#[cfg(not(target_arch = "aarch64"))]
fn untag_ptr(ptr: u64) -> u64 {
    ptr
}

/// Reads the contents of the given process' memory range.
pub fn read_process_memory(
    process: &zx::Process,
    address: u64,
    num_bytes: u64,
) -> Result<Vec<u8>, zx::Status> {
    let mut buf = vec![0; num_bytes.try_into().or(Err(zx::Status::FILE_BIG))?];
    process.read_memory(untag_ptr(address) as zx_vaddr_t, &mut buf)?;
    Ok(buf)
}

/// Returns the list of the executable regions in a given process' address space.
pub fn find_executable_regions(
    process: &zx::Process,
) -> Result<Vec<fheapdump_client::ExecutableRegion>, zx::Status> {
    let mut output_vec = Vec::new();
    let status = unsafe {
        ElfSearchExecutableRegions(
            process.raw_handle(),
            callback,
            &mut output_vec as *mut Vec<fheapdump_client::ExecutableRegion> as *mut c_void,
        )
    };
    zx::Status::ok(status)?;
    Ok(output_vec)
}

extern "C" {
    // Implemented in elf-search.cc
    fn ElfSearchExecutableRegions(
        process_handle: zx_handle_t,
        callback: extern "C" fn(
            u64,
            u64,
            u64,
            *const u8,
            usize,
            u64,
            *const c_char,
            usize,
            *mut c_void,
        ),
        callback_arg: *mut c_void,
    ) -> zx_status_t;
}

extern "C" fn callback(
    address: u64,
    size: u64,
    file_offset: u64,
    build_id: *const u8,
    build_id_len: usize,
    vaddr: u64,
    name: *const c_char,
    name_len: usize,
    callback_arg: *mut c_void,
) {
    // SAFETY: this is the `callback_arg` value passed by `find_executable_regions`, which sets it
    // to the address of the destination vector.
    let output_vec: &mut Vec<fheapdump_client::ExecutableRegion> =
        unsafe { &mut *(callback_arg as *mut _) };

    // SAFETY: the address and the size of the `build_id` array come from the elf-search library.
    let mut build_id = unsafe { core::slice::from_raw_parts(build_id, build_id_len) }.to_vec();
    build_id.truncate(fheapdump_client::MAX_BUILD_ID_LENGTH as usize);

    // SAFETY: the address and the length of the `name` come from the elf-search library.
    let name = unsafe { core::slice::from_raw_parts(name, name_len) };
    let name = String::from_utf8(name.iter().map(|ch| *ch as u8).collect());

    #[cfg(not(fuchsia_api_level_at_least = "NEXT"))]
    let _unused = (vaddr, name); // avoid "unused variable" warnings

    output_vec.push(fheapdump_client::ExecutableRegion {
        address: Some(address),
        size: Some(size),
        file_offset: Some(file_offset),
        build_id: Some(fheapdump_client::BuildId { value: build_id }),
        #[cfg(fuchsia_api_level_at_least = "NEXT")]
        vaddr: Some(vaddr),
        #[cfg(fuchsia_api_level_at_least = "NEXT")]
        name: name.ok(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify that this function's address belongs to exactly one executable region.
    #[test]
    fn test_find_executable_regions() {
        let executable_regions = find_executable_regions(&fuchsia_runtime::process_self()).unwrap();

        let test_address = test_find_executable_regions as u64;
        let region_count = executable_regions
            .iter()
            .filter(|region| {
                let start = region.address.unwrap();
                let end = start + region.size.unwrap();
                (start..end).contains(&test_address)
            })
            .count();

        assert_eq!(
            region_count, 1,
            "test address {:x} not covered exactly once by {:x?}",
            test_address, executable_regions
        );
    }
}
