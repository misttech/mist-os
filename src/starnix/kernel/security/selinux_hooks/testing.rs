// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use crate::security::selinux_hooks::XATTR_NAME_SELINUX;
use crate::security::{SecurityId, SecurityServer};
use crate::testing::AutoReleasableTask;
use crate::vfs::{FsNode, NamespaceNode, XattrOp};
use selinux_core::security_server::Mode;
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::file_mode::FileMode;
use std::sync::Arc;

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

pub fn create_test_executable(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &AutoReleasableTask,
    security_context: &[u8],
) -> NamespaceNode {
    let namespace_node = current_task
        .fs()
        .root()
        .create_node(locked, &current_task, "executable".into(), FileMode::IFREG, DeviceType::NONE)
        .expect("create_node(file)");
    let fs_node = &namespace_node.entry.node;
    fs_node
        .ops()
        .set_xattr(
            fs_node,
            current_task,
            XATTR_NAME_SELINUX.to_bytes().into(),
            security_context.into(),
            XattrOp::Set,
        )
        .expect("set security.selinux xattr");
    namespace_node
}
