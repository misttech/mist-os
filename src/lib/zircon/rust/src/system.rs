// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Retrieve the system memory page size in bytes.
///
/// Wraps the
/// [zx_system_get_page_size](https://fuchsia.dev/fuchsia-src/reference/syscalls/system_get_page_size.md)
/// syscall.
pub fn system_get_page_size() -> u32 {
    unsafe { crate::sys::zx_system_get_page_size() }
}

/// Get the amount of physical memory on the system, in bytes.
///
/// Wraps the
/// [zx_system_get_physmem](https://fuchsia.dev/fuchsia-src/reference/syscalls/system_get_physmem)
/// syscall.
pub fn system_get_physmem() -> u64 {
    unsafe { crate::sys::zx_system_get_physmem() }
}

/// Get number of logical processors on the system.
///
/// Wraps the
/// [zx_system_get_num_cpus](https://fuchsia.dev/fuchsia-src/reference/syscalls/system_get_num_cpus)
/// syscall.
pub fn system_get_num_cpus() -> u32 {
    unsafe { crate::sys::zx_system_get_num_cpus() }
}
