// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_runtime as fruntime;

/// The handle type that is used to pass configuration handles to the starnix_kernel.
pub const HANDLE_TYPE: fruntime::HandleType = fruntime::HandleType::User0;

/// The handle info that is used to pass the /pkg directory of a galaxy component.
pub const PKG_HANDLE_INFO: fruntime::HandleInfo = fruntime::HandleInfo::new(HANDLE_TYPE, 0);

/// The handle info that is used to pass the outgoing directory for a galaxy.
pub const GALAXY_OUTGOING_DIR_HANDLE_INFO: fruntime::HandleInfo =
    fruntime::HandleInfo::new(HANDLE_TYPE, 1);
