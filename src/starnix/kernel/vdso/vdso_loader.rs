// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::AsHandleRef;
use fuchsia_zircon::{self as zx, HandleBased};
use std::sync::Arc;

use crate::{
    arch::vdso::{get_sigreturn_offset, set_vvar_data, HAS_VDSO},
    mm::PAGE_SIZE,
    types::{errno, from_status_like_fdio, Errno},
};

#[derive(Default)]
pub struct Vdso {
    pub vmo: Option<Arc<zx::Vmo>>,
    pub sigreturn_offset: Option<u64>,
    pub vvar_writeable: Option<Arc<zx::Vmo>>,
    pub vvar_readonly: Option<Arc<zx::Vmo>>,
}

impl Vdso {
    pub fn new() -> Self {
        let vdso_vmo = load_vdso_from_file().expect("Couldn't read vDSO from disk");
        let sigreturn = match vdso_vmo.as_ref() {
            Some(vdso) => get_sigreturn_offset(vdso),
            None => Ok(None),
        }
        .expect("Couldn't find signal trampoline code in vDSO");

        let (vvar_vmo_writeable, vvar_vmo_readonly) = if HAS_VDSO {
            let (writeable_vvar, readonly_vvar) = create_vvar_and_handles();
            (Some(writeable_vvar), Some(readonly_vvar))
        } else {
            (None, None)
        };

        Self {
            vmo: vdso_vmo,
            sigreturn_offset: sigreturn,
            vvar_writeable: vvar_vmo_writeable,
            vvar_readonly: vvar_vmo_readonly,
        }
    }
}

fn create_vvar_and_handles() -> (Arc<zx::Vmo>, Arc<zx::Vmo>) {
    // Creating a vvar vmo which has a handle which is writeable.
    // Starnix will use this handle to write to vvar
    let vvar_vmo_writeable =
        Arc::new(zx::Vmo::create(*PAGE_SIZE as u64).expect("Couldn't create vvar vvmo"));
    let _ = set_vvar_data(&vvar_vmo_writeable).expect("Couldn't write data in vvar");
    let vvar_writeable_rights = vvar_vmo_writeable
        .basic_info()
        .expect("Couldn't get rights of writeable vvar handle")
        .rights;
    // Create a duplicate handle to this vvar vmo which doesn't have write permission
    // This handle is used to map vvar into linux userspace
    let vvar_readable_rights = vvar_writeable_rights.difference(zx::Rights::WRITE);
    let vvar_vmo_readonly = Arc::new(
        vvar_vmo_writeable
            .as_ref()
            .duplicate_handle(vvar_readable_rights)
            .expect("couldn't duplicate vvar handle"),
    );
    (vvar_vmo_writeable, vvar_vmo_readonly)
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

    let dir_proxy = sync_open_in_namespace(VDSO_LOCATION, fuchsia_fs::OpenFlags::RIGHT_READABLE)?;
    let vdso_vmo = syncio::directory_open_vmo(
        &dir_proxy,
        VDSO_FILENAME,
        fidl_fuchsia_io::VmoFlags::READ,
        zx::Time::INFINITE,
    )
    .map_err(|status| from_status_like_fdio!(status))?;

    Ok(Some(Arc::new(vdso_vmo)))
}
