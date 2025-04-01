// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::fs_node::fs_node_init_with_dentry;
use super::{
    check_permission, fs_node_effective_sid_and_class, todo_check_permission, FileSystemLabel,
    FileSystemLabelState, FileSystemState, FsNodeSidAndClass,
};

use crate::task::CurrentTask;
use crate::vfs::fs_args::MountParams;
use crate::vfs::{FileSystem, FileSystemHandle, FsStr, Mount, NamespaceNode, OutputBuffer};
use crate::TODO_DENY;
use selinux::permission_check::PermissionCheck;
use selinux::{
    CommonFilePermission, FileSystemMountOptions, FileSystemPermission, FsNodeClass, SecurityId,
    SecurityServer,
};
use starnix_logging::{log_debug, track_stub};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked};
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

/// Returns security state to associate with a filesystem based on the supplied mount options.
pub(in crate::security) fn file_system_init_security(
    name: &'static FsStr,
    mount_options: &FileSystemMountOptions,
) -> Result<FileSystemState, Errno> {
    Ok(FileSystemState::new(name, mount_options.clone()))
}

/// Resolves the labeling scheme and arguments for the `file_system`, based on the loaded policy.
pub(in crate::security) fn file_system_resolve_security<L>(
    locked: &mut Locked<'_, L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file_system: &FileSystemHandle,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    // TODO: https://fxbug.dev/334094811 - Determine how failures, e.g. mount options containing
    // Security Context values that are not valid in the loaded policy.
    let pending_entries = {
        let mut label_state = file_system.security_state.state.0.lock();
        let (resolved_label_state, pending_entries) = match &mut *label_state {
            FileSystemLabelState::Labeled { .. } => return Ok(()),
            FileSystemLabelState::Unlabeled { name, mount_options, pending_entries } => (
                {
                    FileSystemLabelState::Labeled {
                        label: label_from_mount_options_and_name(
                            security_server,
                            mount_options,
                            name,
                        ),
                    }
                },
                std::mem::take(pending_entries),
            ),
        };
        *label_state = resolved_label_state;
        pending_entries
    };

    if let Some(root_dir_entry) = file_system.maybe_root() {
        fs_node_init_with_dentry(
            Some(&mut locked.cast_locked()),
            security_server,
            current_task,
            root_dir_entry,
        )?;
    }

    // Label the `FsNode`s for any `pending_entries`.
    let labeled_entries = pending_entries.len();
    for dir_entry in pending_entries {
        if let Some(dir_entry) = dir_entry.0.upgrade() {
            fs_node_init_with_dentry(
                Some(&mut locked.cast_locked()),
                security_server,
                current_task,
                &dir_entry,
            )
            .unwrap_or_else(|_| panic!("Failed to resolve FsNode label"));
        }
    }
    log_debug!("Labeled {} entries in {} FileSystem", labeled_entries, file_system.name());

    Ok(())
}

/// Returns the security label to be applied to a file system with the name `fs_name`
/// that is to be mounted with `mount_options`.
pub(super) fn label_from_mount_options_and_name(
    security_server: &SecurityServer,
    mount_options: &FileSystemMountOptions,
    fs_name: &'static FsStr,
) -> FileSystemLabel {
    // TODO: https://fxbug.dev/361297862 - Replace this workaround with more
    // general handling of these special Fuchsia filesystems.
    let effective_name: &FsStr =
        if *fs_name == "remotefs" || *fs_name == "remote_bundle" || *fs_name == "remotevol" {
            track_stub!(
                TODO("https://fxbug.dev/361297862"),
                "Applying ext4 labeling configuration to remote filesystems"
            );
            "ext4".into()
        } else {
            fs_name
        };
    security_server.resolve_fs_label(effective_name.into(), mount_options)
}

/// Consumes the SELinux mount options from the supplied `MountParams` and returns the security
/// mount options for the given `MountParams`.
pub(in crate::security) fn sb_eat_lsm_opts(
    mount_params: &mut MountParams,
) -> Result<FileSystemMountOptions, Errno> {
    let context = mount_params.remove(FsStr::new(b"context"));
    let def_context = mount_params.remove(FsStr::new(b"defcontext"));
    let fs_context = mount_params.remove(FsStr::new(b"fscontext"));
    let root_context = mount_params.remove(FsStr::new(b"rootcontext"));

    // If a "context" is specified then it is used for all nodes in the filesystem, so the other
    // security context options would not be meaningful to combine with it, except "fscontext".
    if context.is_some() && (def_context.is_some() || root_context.is_some()) {
        return error!(EINVAL);
    }
    Ok(FileSystemMountOptions {
        context: context.map(Into::into),
        def_context: def_context.map(Into::into),
        fs_context: fs_context.map(Into::into),
        root_context: root_context.map(Into::into),
    })
}

/// Checks if `current_task` has the permission to mount `fs`.
pub(in crate::security) fn sb_kern_mount(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    fs: &FileSystem,
) -> Result<(), Errno> {
    let audit_context = [current_task.into(), fs.into()];
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = fs_sid(fs)?;
    check_permission(
        permission_check,
        source_sid,
        target_sid,
        FileSystemPermission::Mount,
        (&audit_context).into(),
    )
}

/// Checks if `current_task` has the permission to mount at `path` with the mounting flags `flags`.
pub(in crate::security) fn sb_mount(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    path: &NamespaceNode,
    flags: MountFlags,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    if flags.contains(MountFlags::REMOUNT) {
        let mount = path.mount_if_root()?;
        let fs = mount.root().entry.node.fs();
        let target_sid = fs_sid(&fs)?;
        let audit_context = [current_task.into(), fs.as_ref().into()];
        check_permission(
            permission_check,
            source_sid,
            target_sid,
            FileSystemPermission::Remount,
            (&audit_context).into(),
        )
    } else {
        let node = path.entry.node.as_ref().as_ref();
        let FsNodeSidAndClass { sid: target_sid, class: target_class } =
            fs_node_effective_sid_and_class(node);
        let FsNodeClass::File(target_class) = target_class else {
            panic!("sb_mount on non-file-like class")
        };
        let audit_context = [current_task.into(), node.into()];
        todo_check_permission(
            TODO_DENY!("https://fxbug.dev/380230897", "Check mounton permission."),
            &current_task.kernel(),
            permission_check,
            source_sid,
            target_sid,
            CommonFilePermission::MountOn.for_class(target_class),
            (&audit_context).into(),
        )
    }
}

/// Checks that `mount` is getting remounted with the same security state as before.
pub(in crate::security) fn sb_remount(
    security_server: &SecurityServer,
    mount: &Mount,
    new_mount_options: FileSystemMountOptions,
) -> Result<(), Errno> {
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

/// Writes the LSM mount options of `mount` to `buf`.
pub(in crate::security) fn sb_show_options(
    security_server: &SecurityServer,
    buf: &mut impl OutputBuffer,
    mount: &Mount,
) -> Result<(), Errno> {
    mount.security_state().state.write_mount_options(security_server, buf)
}

/// Checks if `current_task` has the permission to get information on `fs`.
pub(in crate::security) fn sb_statfs(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    fs: &FileSystem,
) -> Result<(), Errno> {
    let audit_context = [current_task.into(), fs.into()];
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = fs_sid(fs)?;
    check_permission(
        permission_check,
        source_sid,
        target_sid,
        FileSystemPermission::GetAttr,
        (&audit_context).into(),
    )
}

/// Checks if `current_task` has the permission to unmount the filesystem mounted on
/// `node` using the unmount flags `_flags`.
pub(in crate::security) fn sb_umount(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    node: &NamespaceNode,
    _flags: UnmountFlags,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let mount = node.mount_if_root()?;
    let fs = mount.root().entry.node.fs();
    let target_sid = fs_sid(&fs)?;
    let audit_context = [current_task.into(), fs.as_ref().into()];
    check_permission(
        permission_check,
        source_sid,
        target_sid,
        FileSystemPermission::Unmount,
        (&audit_context).into(),
    )
}
