// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_runtime as fruntime;
use rand::Rng;

/// The handle type that is used to pass configuration handles to the starnix_kernel.
pub const HANDLE_TYPE: fruntime::HandleType = fruntime::HandleType::User0;

/// The handle info that is used to pass the /pkg directory of a container component.
pub const PKG_HANDLE_INFO: fruntime::HandleInfo = fruntime::HandleInfo::new(HANDLE_TYPE, 0);

/// The handle info that is used to pass the outgoing directory for a container.
pub const CONTAINER_OUTGOING_DIR_HANDLE_INFO: fruntime::HandleInfo =
    fruntime::HandleInfo::new(HANDLE_TYPE, 1);

/// The handle info that is used to pass the svc directory for a container.
pub const CONTAINER_SVC_HANDLE_INFO: fruntime::HandleInfo =
    fruntime::HandleInfo::new(HANDLE_TYPE, 2);

/// Takes a `name` and generates a `String` suitable to use as the name of a component in a
/// collection.
///
/// Used to avoid creating children with the same name.
pub fn generate_kernel_name(name: &str) -> String {
    let random_id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    name.to_owned() + "_" + &random_id
}
