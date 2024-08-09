// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use selinux_core::security_server::{Mode, SecurityServer};
use std::sync::Arc;

use crate::vfs::FsNode;
use selinux_core::SecurityId;

/// Returns the security id currently stored in `fs_node`, if any. This API should only be used
/// by code that is responsible for controlling the cached security id; e.g., to check its
/// current value before engaging logic that may compute a new value. Access control enforcement
/// code should use `get_effective_fs_node_security_id()`, *not* this function.
pub fn get_cached_sid(fs_node: &FsNode) -> Option<SecurityId> {
    fs_node.info().security_state.sid
}

/// `hooks_tests_policy.pp` is a compiled policy module.
/// The path is relative to this rust source file.
const HOOKS_TESTS_BINARY_POLICY: &[u8] =
    include_bytes!("../../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

pub fn security_server_with_policy() -> Arc<SecurityServer> {
    let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
    let security_server = SecurityServer::new(Mode::Enable);
    security_server.set_enforcing(true);
    security_server.load_policy(policy_bytes).expect("policy load failed");
    security_server
}
