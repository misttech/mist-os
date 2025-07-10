// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::fs_node::fs_node_init_with_dentry;
use super::{
    check_permission, fs_node_effective_sid_and_class, task_effective_sid, FileSystemLabel,
    FileSystemState, FsNodeSidAndClass,
};

use crate::task::CurrentTask;
use crate::vfs::fs_args::MountParams;
use crate::vfs::{FileSystem, FileSystemHandle, FsStr, Mount, NamespaceNode, OutputBuffer};
use selinux::permission_check::PermissionCheck;
use selinux::{
    CommonFilePermission, FileSystemMountOptions, FileSystemPermission, ForClass, FsNodeClass,
    SecurityId, SecurityServer,
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
    let Some(fs_label) = fs.security_state.state.label() else {
        return error!(EPERM);
    };
    Ok(fs_label.sid)
}

/// Returns security state to associate with a filesystem based on the supplied mount options.
pub(in crate::security) fn file_system_init_security(
    mount_options: &FileSystemMountOptions,
) -> Result<FileSystemState, Errno> {
    Ok(FileSystemState::new(mount_options.clone()))
}

/// Resolves the labeling scheme and arguments for the `file_system`, based on the loaded policy.
pub(in crate::security) fn file_system_resolve_security<L>(
    locked: &mut Locked<L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file_system: &FileSystemHandle,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    // TODO: https://fxbug.dev/334094811 - Determine how failures, e.g. mount options containing
    // Security Context values that are not valid in the loaded policy.
    let fs_state = &file_system.security_state.state;

    let mut root_node_to_init = Option::default();

    fs_state.label.get_or_init(|| {
        // This caller is initializing the file system, so note the root node to be initialized.
        root_node_to_init = file_system.maybe_root();

        label_from_mount_options_and_name(
            security_server,
            &fs_state.mount_options,
            file_system.name(),
        )
    });

    let pending_entries = {
        let pending = &mut *file_system.security_state.state.pending_entries.lock();
        std::mem::take(pending)
    };

    // This step will be performed only when the file system label is first resolved.
    if let Some(root_dir_entry) = root_node_to_init {
        fs_node_init_with_dentry(
            Some(locked.cast_locked()),
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
                Some(locked.cast_locked()),
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
    let source_sid = task_effective_sid(current_task);
    let target_sid = fs_sid(fs)?;
    check_permission(
        permission_check,
        current_task.kernel(),
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
    let source_sid = task_effective_sid(current_task);
    if flags.contains(MountFlags::REMOUNT) {
        let mount = path.mount_if_root()?;
        let fs = mount.root().entry.node.fs();
        let target_sid = fs_sid(&fs)?;
        let audit_context = [current_task.into(), fs.as_ref().into()];
        check_permission(
            permission_check,
            current_task.kernel(),
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
        check_permission(
            permission_check,
            current_task.kernel(),
            source_sid,
            target_sid,
            CommonFilePermission::MountOn.for_class(target_class),
            (&audit_context).into(),
        )
    }
}

/// Checks that `mount` is getting remounted with the same security state as before.
pub(in crate::security) fn sb_remount(
    _security_server: &SecurityServer,
    mount: &Mount,
    new_mount_options: FileSystemMountOptions,
) -> Result<(), Errno> {
    if mount.security_state().state.mount_options != new_mount_options {
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
    let source_sid = task_effective_sid(current_task);
    let target_sid = fs_sid(fs)?;
    check_permission(
        permission_check,
        current_task.kernel(),
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
    let source_sid = task_effective_sid(current_task);
    let mount = node.mount_if_root()?;
    let fs = mount.root().entry.node.fs();
    let target_sid = fs_sid(&fs)?;
    let audit_context = [current_task.into(), fs.as_ref().into()];
    check_permission(
        permission_check,
        current_task.kernel(),
        source_sid,
        target_sid,
        FileSystemPermission::Unmount,
        (&audit_context).into(),
    )
}
