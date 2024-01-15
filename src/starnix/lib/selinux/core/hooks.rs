// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{security_context::SecurityContext, security_server::SecurityServer, SecurityId};
use std::sync::Arc;

/// The SELinux security structure for `ThreadGroup`.
#[derive(Default)]
pub struct SeLinuxThreadGroupState {
    /// Current SID for the task.
    pub current_sid: SecurityId,

    /// SID for the task upon the next execve call.
    pub exec_sid: Option<SecurityId>,

    /// SID for files created by the task.
    pub fscreate_sid: Option<SecurityId>,

    /// SID for kernel-managed keys created by the task.
    pub keycreate_sid: Option<SecurityId>,

    /// SID prior to the last execve.
    pub previous_sid: SecurityId,

    /// SID for sockets created by the task.
    pub sockcreate_sid: Option<SecurityId>,
}

impl SeLinuxThreadGroupState {
    // TODO(http://b/316181721): initialize with correct values; use hard coded value for fake mode.
    pub fn new_default(security_server: &Arc<SecurityServer>) -> Self {
        let sid = security_server.security_context_to_sid(
            &SecurityContext::try_from("unconfined_u:unconfined_r:unconfined_t:s0-s0:c0-c1023")
                .unwrap(),
        );
        SeLinuxThreadGroupState { current_sid: sid, previous_sid: sid, ..Default::default() }
    }
}
