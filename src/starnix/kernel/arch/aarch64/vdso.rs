// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::Errno;
use fuchsia_zircon as zx;

pub const HAS_VDSO: bool = true;

pub fn set_vdso_constants(_vdso_vmo: &zx::Vmo) -> Result<(), Errno> {
    Ok(())
}
