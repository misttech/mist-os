// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::VdsoError,
    cstr::cstr,
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
    lazy_static::lazy_static,
    std::collections::HashMap,
    std::ffi::{CStr, CString},
};

fn take_vdso_vmos() -> Result<HashMap<CString, zx::Vmo>, VdsoError> {
    let mut vmos = HashMap::new();
    let mut i = 0;
    while let Some(handle) = take_startup_handle(HandleInfo::new(HandleType::VdsoVmo, i)) {
        let vmo = zx::Vmo::from(handle);
        let name = vmo.get_name().map_err(VdsoError::GetName)?;
        vmos.insert(name, vmo);
        i += 1;
    }
    Ok(vmos)
}

pub fn get_vdso_vmo(name: &CStr) -> Result<zx::Vmo, VdsoError> {
    lazy_static! {
        static ref VMOS: HashMap<CString, zx::Vmo> =
            take_vdso_vmos().expect("Failed to take vDSO VMOs");
    }
    if let Some(vmo) = VMOS.get(name) {
        vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(|status| VdsoError::CouldNotDuplicate { name: name.to_owned(), status })
    } else {
        Err(VdsoError::NotFound(name.to_owned()))
    }
}

/// Returns an owned VMO handle to the stable vDSO, duplicated from the handle
/// provided to this process through its processargs bootstrap message.
pub fn get_stable_vdso_vmo() -> Result<zx::Vmo, VdsoError> {
    get_vdso_vmo(cstr!("vdso/stable"))
}

/// Returns an owned VMO handle to the next vDSO, duplicated from the handle
/// provided to this process through its processargs bootstrap message.
pub fn get_next_vdso_vmo() -> Result<zx::Vmo, VdsoError> {
    get_vdso_vmo(cstr!("vdso/next"))
}

/// Returns an owned VMO handle to the direct vDSO, duplicated from the handle
/// provided to this process through its processargs bootstrap message.
pub fn get_direct_vdso_vmo() -> Result<zx::Vmo, VdsoError> {
    get_vdso_vmo(cstr!("vdso/direct"))
}
