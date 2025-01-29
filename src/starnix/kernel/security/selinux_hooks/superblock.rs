// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security::selinux_hooks;
use crate::security::selinux_hooks::{
    check_permission, fs_node_effective_sid_and_class, todo_check_permission, FileSystemLabelState,
    FsNodeSidAndClass,
};
use crate::task::CurrentTask;
use crate::vfs::fs_args::MountParams;
use crate::vfs::{FileSystem, Mount, NamespaceNode};
use crate::TODO_DENY;
use selinux::permission_check::PermissionCheck;
use selinux::{CommonFilePermission, FileSystemPermission, SecurityId, SecurityServer};
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

/// Checks if `current_task` has the permission to mount `fs`.
pub fn sb_kern_mount(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    fs: &FileSystem,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = fs_sid(fs)?;
    check_permission(permission_check, source_sid, target_sid, FileSystemPermission::Mount)
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
        let FsNodeSidAndClass { sid: target_sid, class: target_class } =
            fs_node_effective_sid_and_class(&path.entry.node);
        todo_check_permission(
            TODO_DENY!("https://fxbug.dev/380230897", "Check mounton permission."),
            permission_check,
            source_sid,
            target_sid,
            CommonFilePermission::MountOn.for_class(target_class),
        )
    }
}

/// Checks that `mount` is getting remounted with the same security state as before.
pub fn sb_remount(
    security_server: &SecurityServer,
    mount: &Mount,
    new_mount_params: &MountParams,
) -> Result<(), Errno> {
    let new_mount_options = selinux_hooks::sb_eat_lsm_opts(new_mount_params)?;
    let fs_name = mount.fs_name();
    if !mount.security_state().state.equivalent_to_options(
        security_server,
        &new_mount_options,
        fs_name,
    ) {
        return error!(EACCES);
    }
    Ok(())
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
