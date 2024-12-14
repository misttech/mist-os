// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security::selinux_hooks::{
    check_permission, fs_node_effective_sid, todo_check_permission, FileSystemLabelState,
};
use crate::task::CurrentTask;
use crate::vfs::{FileSystem, NamespaceNode};
use crate::TODO_DENY;
use selinux::permission_check::PermissionCheck;
use selinux::{CommonFilePermission, FileClass, FileSystemPermission, SecurityId};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::unmount_flags::UnmountFlags;

/// Returns the [`SecurityId`] of `fs`.
/// If the filesystem is not labeled, returns EPERM.
fn fs_sid(fs: &FileSystem) -> Result<SecurityId, Errno> {
    let filesystem_sid = match &*fs.security_state.state.0.lock() {
        FileSystemLabelState::Labeled { label } => Ok(label.sid),
        FileSystemLabelState::Unlabeled { .. } => {
            error!(EPERM)
        }
    }?;
    Ok(filesystem_sid)
}

/// Checks if `current_task` has the permission to mount at `path` with the mounting flags `flags`.
pub fn sb_mount(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    path: &NamespaceNode,
    flags: MountFlags,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    if flags.contains(MountFlags::REMOUNT) {
        let mount = path.mount_if_root()?;
        let target_sid = fs_sid(&mount.root().entry.node.fs())?;
        check_permission(permission_check, source_sid, target_sid, FileSystemPermission::Remount)
    } else {
        let target_sid = fs_node_effective_sid(&path.entry.node);
        todo_check_permission(
            TODO_DENY!("https://fxbug.dev/380230897", "Check mounton permission."),
            permission_check,
            source_sid,
            target_sid,
            CommonFilePermission::MountOn.for_class(FileClass::Dir),
        )
    }
}

/// Checks if `current_task` has the permission to get information on `fs`.
pub fn sb_statfs(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    fs: &FileSystem,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = fs_sid(fs)?;
    check_permission(permission_check, source_sid, target_sid, FileSystemPermission::GetAttr)
}

/// Checks if `current_task` has the permission to unmount the filesystem mounted on
/// `node` using the unmount flags `_flags`.
pub fn sb_umount(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    node: &NamespaceNode,
    _flags: UnmountFlags,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let mount = node.mount_if_root()?;
    let target_sid = fs_sid(&mount.root().entry.node.fs())?;
    check_permission(permission_check, source_sid, target_sid, FileSystemPermission::Unmount)
}
