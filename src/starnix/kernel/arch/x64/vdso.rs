// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::x86_64::_rdtsc;
use fuchsia_zircon as zx;
use process_builder::elf_parse;
use std::sync::Arc;
use zerocopy::AsBytes;

use crate::types::{errno, from_status_like_fdio, uapi, Errno};
use crate::vmex_resource::VMEX_RESOURCE;

fn sync_open_in_namespace(
    path: &str,
    flags: fidl_fuchsia_io::OpenFlags,
) -> Result<fidl_fuchsia_io::DirectorySynchronousProxy, Errno> {
    let (client, server) = fidl::Channel::create();
    let dir_proxy = fidl_fuchsia_io::DirectorySynchronousProxy::new(client);

    let namespace = fdio::Namespace::installed().map_err(|_| errno!(EINVAL))?;
    namespace.open(path, flags, server).map_err(|_| errno!(ENOENT))?;
    Ok(dir_proxy)
}

/// Overwrite the constants in the vDSO with the values obtained from Zircon.
fn set_vdso_constants(vdso_vmo: &zx::Vmo) -> Result<(), Errno> {
    let headers = elf_parse::Elf64Headers::from_vmo(vdso_vmo).map_err(|_| errno!(EINVAL))?;
    // The entry point in the vDSO stores the offset at which the constants are located. See vdso/vdso.ld for details.
    let constants_offset = headers.file_header().entry;

    let clock = zx::Clock::create(zx::ClockOpts::MONOTONIC | zx::ClockOpts::AUTO_START, None)
        .expect("failed to create clock");
    let details = clock.get_details().expect("Failed to get clock details");
    let zx_ticks = zx::ticks_get();
    let raw_ticks;
    unsafe {
        raw_ticks = _rdtsc();
    }
    // Compute the offset as a difference between the value returned by zx_get_ticks() and the value
    // in the register. This is not precise as the reads are not synchronised.
    // TODO(fxb/124586): Support updates to this value.
    let ticks_offset = zx_ticks - (raw_ticks as i64);

    let vdso_consts: uapi::vdso_constants = uapi::vdso_constants {
        raw_ticks_to_ticks_offset: ticks_offset,
        ticks_to_mono_numerator: details.ticks_to_synthetic.rate.synthetic_ticks,
        ticks_to_mono_denominator: details.ticks_to_synthetic.rate.reference_ticks,
    };
    vdso_vmo
        .write(vdso_consts.as_bytes(), constants_offset as u64)
        .map_err(|status| from_status_like_fdio!(status))?;
    Ok(())
}

/// Reads the vDSO file and returns the backing VMO.
pub fn load_vdso_from_file() -> Result<Option<Arc<zx::Vmo>>, Errno> {
    const VDSO_FILENAME: &str = "libvdso.so";
    const VDSO_LOCATION: &str = "/pkg/data";

    let dir_proxy = sync_open_in_namespace(
        VDSO_LOCATION,
        fuchsia_fs::OpenFlags::RIGHT_READABLE | fuchsia_fs::OpenFlags::RIGHT_EXECUTABLE,
    )?;
    let vdso_vmo = syncio::directory_open_vmo(
        &dir_proxy,
        VDSO_FILENAME,
        fidl_fuchsia_io::VmoFlags::READ | fidl_fuchsia_io::VmoFlags::EXECUTE,
        zx::Time::INFINITE,
    )
    .map_err(|status| from_status_like_fdio!(status))?;

    let vdso_size = vdso_vmo.get_size().map_err(|status| from_status_like_fdio!(status))?;
    let vdso_clone = vdso_vmo
        .create_child(zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE, 0, vdso_size)
        .map_err(|status| from_status_like_fdio!(status))?;
    set_vdso_constants(&vdso_clone)?;
    let vdso_executable = vdso_clone
        .replace_as_executable(&VMEX_RESOURCE)
        .map_err(|status| from_status_like_fdio!(status))?;

    Ok(Some(Arc::new(vdso_executable)))
}
