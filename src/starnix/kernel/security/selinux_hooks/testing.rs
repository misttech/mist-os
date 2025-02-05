// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use super::scoped_fs_create;
use crate::security::SecurityServer;
use crate::task::CurrentTask;
use crate::testing::spawn_kernel_with_selinux_and_run;
use crate::vfs::{FsStr, NamespaceNode};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::file_mode::FileMode;
use std::sync::Arc;

// The default name used files used in testing.
pub const TEST_FILE_NAME: &str = "file";

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
    let security_server = &current_task.kernel().security_state.state.as_ref().unwrap().server;
    let fscreate_sid = security_server.security_context_to_sid(security_context.into()).unwrap();
    let scoped_fs_create = scoped_fs_create(current_task, fscreate_sid);
    let namespace_node = current_task
        .fs()
        .root()
        .create_node(locked, &current_task, "executable".into(), FileMode::IFREG, DeviceType::NONE)
        .expect("create_node(file)");
    std::mem::drop(scoped_fs_create);
    namespace_node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::selinux_hooks::fs_node_effective_sid_and_class;

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";

    #[fuchsia::test]
    async fn create_test_executable_sets_context() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let dir_entry =
                    &create_test_executable(locked, current_task, VALID_SECURITY_CONTEXT).entry;

                let effective_sid = fs_node_effective_sid_and_class(&dir_entry.node).sid;
                let effective_context =
                    security_server.sid_to_security_context(effective_sid).unwrap();
                assert_eq!(effective_context, VALID_SECURITY_CONTEXT);
            },
        );
    }
}
