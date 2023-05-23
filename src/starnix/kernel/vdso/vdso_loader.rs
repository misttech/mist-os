// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use std::sync::Arc;

use crate::arch::vdso::{get_sigreturn_offset, set_vdso_constants, HAS_VDSO};
use crate::types::{errno, from_status_like_fdio, Errno};
use crate::vmex_resource::VMEX_RESOURCE;

#[derive(Default)]
pub struct Vdso {
    pub vmo: Option<Arc<zx::Vmo>>,
    pub sigreturn_offset: Option<u64>,
}

impl Vdso {
    pub fn new() -> Self {
        let vdso_vmo = load_vdso_from_file().expect("Couldn't read vDSO from disk");
        let sigreturn = match vdso_vmo.as_ref() {
            Some(vdso) => get_sigreturn_offset(vdso),
            None => Ok(None),
        }
        .expect("Couldn't find signal trampoline code in vDSO");
        Self { vmo: vdso_vmo, sigreturn_offset: sigreturn }
    }
}

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

/// Reads the vDSO file and returns the backing VMO.
pub fn load_vdso_from_file() -> Result<Option<Arc<zx::Vmo>>, Errno> {
    if !HAS_VDSO {
        return Ok(None);
    }
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
