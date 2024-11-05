// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use crate::security;
use crate::security::selinux_hooks::XATTR_NAME_SELINUX;
use crate::security::SecurityServer;
use crate::task::CurrentTask;
use crate::testing::spawn_kernel_with_selinux_and_run;
use crate::vfs::{FsStr, NamespaceNode, XattrOp};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::file_mode::FileMode;
use std::sync::Arc;

// The default name used files used in testing.
pub const TEST_FILE_NAME: &str = "file";

/// Creates a new file named [`TEST_FILE_NAME`] under the root of the filesystem.
/// As currently implemented this will exercise the file-labeling scheme
/// specified for the root filesystem by the current policy and then
/// clear both the file's cached `SecurityId` and its extended attribute.
pub fn create_unlabeled_test_file(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> NamespaceNode {
    let namespace_node = create_test_file(locked, current_task);
    assert!(security::fs_node_setsecurity(
        locked,
        current_task,
        &namespace_node.entry.node,
        XATTR_NAME_SELINUX.to_bytes().into(),
        "".into(),
        XattrOp::Set
    )
    .is_ok());
    namespace_node
}

/// Creates a new file named [`TEST_FILE_NAME`] under the root of the filesystem.
/// Note that this will exercise the file-labeling scheme specified for the root
/// filesystem by the current policy.
pub fn create_test_file(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> NamespaceNode {
    current_task
        .fs()
        .root()
        .create_node(
            locked,
            &current_task,
            TEST_FILE_NAME.into(),
            FileMode::IFREG,
            DeviceType::NONE,
        )
        .expect("create_node(file)")
}

/// Creates a new path of directories with the given names under the root of the
/// filesystem. Note that this will exercise the file-labeling scheme specified for the
/// root filesystem by the current policy.
pub fn create_directory_with_parents(
    dir_names: Vec<&FsStr>,
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> NamespaceNode {
    let mut current_dir = current_task.fs().root();
    for dir_name in dir_names {
        current_dir = current_dir
            .create_node(locked, &current_task, dir_name, FileMode::IFDIR, DeviceType::NONE)
            .expect("create_node(file)");
    }
    current_dir
}

/// `hooks_tests_policy.pp` is a compiled policy module.
/// The path is relative to this rust source file.
const HOOKS_TESTS_BINARY_POLICY: &[u8] =
    include_bytes!("../../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

/// Instantiates a kernel with SELinux enabled, switches from permissive to enforcing mode, and
/// loads the hooks test policy, before delegating to the supplied test `callback`.
// TODO: https://fxbug.dev/335397745 - Only provide an admin/test API to the test, so that tests
// must generally exercise hooks via public entrypoints.
pub fn spawn_kernel_with_selinux_hooks_test_policy_and_run<F>(callback: F)
where
    F: FnOnce(&mut Locked<'_, Unlocked>, &mut CurrentTask, &Arc<SecurityServer>)
        + Send
        + Sync
        + 'static,
{
    spawn_kernel_with_selinux_and_run(|locked, current_task, security_server| {
        let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        super::selinuxfs_policy_loaded(locked, security_server, current_task);
        callback(locked, current_task, security_server)
    })
}

pub fn create_test_executable(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
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
            &mut locked.cast_locked::<FileOpsCore>(),
            fs_node,
            current_task,
            XATTR_NAME_SELINUX.to_bytes().into(),
            security_context.into(),
            XattrOp::Set,
        )
        .expect("set security.selinux xattr");
    namespace_node
}
