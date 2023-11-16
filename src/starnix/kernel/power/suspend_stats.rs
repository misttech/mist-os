// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::errno::Errno;

use fuchsia_zircon as zx;

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct SuspendStats {
    pub success_count: u64,
    pub fail_count: u64,
    pub last_failed_device: Option<String>,
    pub last_failed_errno: Option<Errno>,
    pub wakeup_count: u64,
    pub last_resume_reason: Option<String>,
    pub last_suspend_time: zx::Duration,
}
