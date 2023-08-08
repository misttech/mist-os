// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::errno::Errno;

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct SuspendStats {
    pub success_count: u64,
    pub fail_count: u64,
    pub last_failed_device: Option<String>,
    pub last_failed_errno: Option<Errno>,
}
