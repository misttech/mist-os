// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use std::sync::Arc;

use crate::types::Errno;

/// Reads the vDSO file and returns the backing VMO.
pub fn load_vdso_from_file() -> Result<Option<Arc<zx::Vmo>>, Errno> {
    Ok(None)
}
