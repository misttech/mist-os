// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct CompatibilityInfo {
    pub status: String,
    pub platform_abi: String,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ConnectionInfo {
    pub ssh_connection: String,
    pub compatibility: CompatibilityInfo,
}
