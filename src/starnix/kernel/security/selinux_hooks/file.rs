// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use super::{
    fs_node_effective_sid_and_class, has_file_permissions, permissions_from_flags,
    todo_has_fs_node_permissions, FileObjectState, FsNodeSidAndClass, PermissionFlags,
};
use crate::security::selinux_hooks::todo_has_file_permissions;
use crate::task::CurrentTask;
use crate::vfs::FileObject;
use crate::TODO_DENY;
use selinux::{CommonFsNodePermission, Permission, SecurityServer};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{
    FIGETBSZ, FIOASYNC, FIONBIO, FIONREAD, FS_IOC_GETFLAGS, FS_IOC_GETVERSION, FS_IOC_SETFLAGS,
    FS_IOC_SETVERSION, F_GETLK, F_SETFL, F_SETLK, F_SETLKW,
};

/// Returns the security state for a new file object created by `current_task`.
pub(in crate::security) fn file_alloc_security(current_task: &CurrentTask) -> FileObjectState {
    FileObjectState { sid: current_task.security_state.lock().current_sid }
}

/// Checks whether the `current_task`` has the permissions specified by `mask` to the `file`.
pub(in crate::security) fn file_permission(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
    mut permission_flags: PermissionFlags,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { class: file_class, .. } =
        fs_node_effective_sid_and_class(&file.name.entry.node);

    if file.flags().contains(OpenFlags::APPEND) {
        permission_flags |= PermissionFlags::APPEND;
    }

    has_file_permissions(
        &security_server.as_permission_check(),
        current_sid,
        file,
        &[],
        current_task.into(),
    )?;

    todo_has_fs_node_permissions(
        TODO_DENY!("https://fxbug.dev/385121365", "Enforce file_permission checks"),
        &security_server.as_permission_check(),
        current_sid,
        &file.name.entry.node,
        &permissions_from_flags(permission_flags, file_class),
        current_task.into(),
    )
}

/// Returns whether the `current_task` can receive `file` via a socket IPC.
pub(in crate::security) fn file_receive(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let subject_sid = current_task.security_state.lock().current_sid;
    let fs_node_class = file.node().security_state.lock().class;
    let permission_flags = file.flags().into();
    todo_has_file_permissions(
        TODO_DENY!("https://fxbug.dev/399894966", "Check file receive permission."),
        &permission_check,
        subject_sid,
        file,
        &permissions_from_flags(permission_flags, fs_node_class),
        current_task.into(),
    )
}

/// Returns whether `current_task` can issue an ioctl to `file`.
pub(in crate::security) fn check_file_ioctl_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
    request: u32,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let subject_sid = current_task.security_state.lock().current_sid;
    has_file_permissions(&permission_check, subject_sid, file, &[], current_task.into())?;

    let file_class = file.node().security_state.lock().class;
    let permissions: &[Permission] = match request {
        // The NSA report also has `FIBMAP` follow this branch.
        FIONREAD | FIGETBSZ | FS_IOC_GETFLAGS | FS_IOC_GETVERSION => {
            &[CommonFsNodePermission::GetAttr.for_class(file_class)]
        }
        FS_IOC_SETFLAGS | FS_IOC_SETVERSION => {
            &[CommonFsNodePermission::SetAttr.for_class(file_class)]
        }
        FIONBIO | FIOASYNC => &[],
        _ => &[CommonFsNodePermission::Ioctl.for_class(file_class)],
    };

    let audit_context = [current_task.into(), file.into()];
    todo_has_fs_node_permissions(
        TODO_DENY!("https://fxbug.dev/385077129", "Enforce file_ioctl() fs-node checks"),
        &permission_check,
        subject_sid,
        file.node(),
        permissions,
        (&audit_context).into(),
    )
}

/// Returns whether `current_task` can perform a lock operation on the given `file`.
pub(in crate::security) fn check_file_lock_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let subject_sid = current_task.security_state.lock().current_sid;
    let fs_node_class = file.node().security_state.lock().class;
    has_file_permissions(
        &permission_check,
        subject_sid,
        file,
        &[CommonFsNodePermission::Lock.for_class(fs_node_class)],
        current_task.into(),
    )
}

/// This hook is called by the `fcntl` syscall. Returns whether `current_task` can perform
/// `fcntl_cmd` on the given file.
pub(in crate::security) fn check_file_fcntl_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
    fcntl_cmd: u32,
    fcntl_arg: u64,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let subject_sid = current_task.security_state.lock().current_sid;
    let fs_node_class = file.node().security_state.lock().class;

    match fcntl_cmd {
        F_GETLK | F_SETLK | F_SETLKW => {
            // Checks both the Lock and Use permissions.
            has_file_permissions(
                &permission_check,
                subject_sid,
                file,
                &[CommonFsNodePermission::Lock.for_class(fs_node_class)],
                current_task.into(),
            )?;
        }
        _ => {
            // Only checks the Use permission.
            has_file_permissions(&permission_check, subject_sid, file, &[], current_task.into())?;
        }
    }

    if fcntl_cmd != F_SETFL {
        return Ok(());
    }

    // Based on documentation additional checks are necessary for F_SETFL, since it updates the file
    // permissions.
    let new_flags = OpenFlags::from_bits_truncate(fcntl_arg as u32);
    let old_flags = file.flags();
    let changed_flags = old_flags.symmetric_difference(new_flags);
    if !changed_flags.contains(OpenFlags::APPEND) {
        // The append value wasn't updated: no further checks are necessary.
        return Ok(());
    }
    if new_flags.contains(OpenFlags::APPEND) {
        if !old_flags.can_write() {
            // The file was previously opened with read-only access. Since append is now requested,
            // we need to check for permission.
            todo_has_fs_node_permissions(
                TODO_DENY!("https://fxbug.dev/385121365", "Enforce file_permission() checks"),
                &security_server.as_permission_check(),
                subject_sid,
                file.node(),
                &permissions_from_flags(PermissionFlags::APPEND, fs_node_class),
                current_task.into(),
            )?;
        }
    } else if old_flags.can_write() {
        // If a file is opened with the WRITE and APPEND permissions, only the APPEND permission is
        // checked. Now that the append flag was cleared we need to check the WRITE permission.
        todo_has_fs_node_permissions(
            TODO_DENY!("https://fxbug.dev/385121365", "Enforce file_permission() checks"),
            &security_server.as_permission_check(),
            subject_sid,
            file.node(),
            &permissions_from_flags(PermissionFlags::WRITE, fs_node_class),
            current_task.into(),
        )?;
    }
    Ok(())
}
