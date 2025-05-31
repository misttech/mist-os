// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use super::bpf::{check_bpf_map_access, check_bpf_prog_access};
use super::{
    fs_node_effective_sid_and_class, has_file_ioctl_permission, has_file_permissions,
    permissions_from_flags, task_effective_sid, todo_has_fs_node_permissions, FileObjectState,
    FsNodeSidAndClass, PermissionFlags,
};
use crate::bpf::fs::BpfHandle;
use crate::mm::{Mapping, MappingName, MappingOptions, ProtectionFlags};
use crate::security::selinux_hooks::{
    check_self_permission, todo_check_permission, todo_has_file_permissions, track_stub,
    ProcessPermission,
};
use crate::task::CurrentTask;
use crate::vfs::{canonicalize_ioctl_request, FileHandle, FileObject, FsNodeHandle};
use crate::TODO_DENY;
use selinux::{CommonFsNodePermission, SecurityServer};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{
    FIBMAP, FIGETBSZ, FIOASYNC, FIONBIO, FIONREAD, FS_IOC_GETFLAGS, FS_IOC_GETVERSION,
    FS_IOC_SETFLAGS, FS_IOC_SETVERSION, F_GETLK, F_SETFL, F_SETLK, F_SETLKW,
};

/// Returns the security state for a new file object created by `current_task`.
pub(in crate::security) fn file_alloc_security(current_task: &CurrentTask) -> FileObjectState {
    FileObjectState { sid: task_effective_sid(current_task) }
}

/// Checks whether the `current_task`` has the permissions specified by `mask` to the `file`.
pub(in crate::security) fn file_permission(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
    mut permission_flags: PermissionFlags,
) -> Result<(), Errno> {
    let current_sid = task_effective_sid(current_task);
    let FsNodeSidAndClass { class: file_class, .. } =
        fs_node_effective_sid_and_class(&file.name.entry.node);

    if file.flags().contains(OpenFlags::APPEND) {
        permission_flags |= PermissionFlags::APPEND;
    }

    has_file_permissions(
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        file,
        &[],
        current_task.into(),
    )?;

    todo_has_fs_node_permissions(
        TODO_DENY!("https://fxbug.dev/385121365", "Enforce file_permission checks"),
        &current_task.kernel(),
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
    let subject_sid = task_effective_sid(current_task);
    let fs_node_class = file.node().security_state.lock().class;
    let permission_flags = file.flags().into();

    // BPF resources are wrapped into file descriptors for interaction with userspace,
    // but have a distinct set of permissions associated with the underlying objects rather
    // than on the `FsNode`.
    if let Some(bpf_handle) = file.downcast_file::<BpfHandle>() {
        has_file_permissions(
            &permission_check,
            current_task.kernel(),
            subject_sid,
            file,
            &[],
            current_task.into(),
        )?;
        match *bpf_handle {
            BpfHandle::Map(ref map) => {
                check_bpf_map_access(security_server, current_task, map, permission_flags)?
            }
            BpfHandle::Program(ref prog) => {
                check_bpf_prog_access(security_server, current_task, prog)?
            }
            _ => {}
        }
        return Ok(());
    }

    todo_has_file_permissions(
        TODO_DENY!("https://fxbug.dev/399894966", "Check file receive permission."),
        &current_task.kernel(),
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
    let subject_sid = task_effective_sid(current_task);

    let file_class = file.node().security_state.lock().class;
    match canonicalize_ioctl_request(current_task, request) {
        FIBMAP | FIONREAD | FIGETBSZ | FS_IOC_GETFLAGS | FS_IOC_GETVERSION => has_file_permissions(
            &permission_check,
            current_task.kernel(),
            subject_sid,
            file,
            &[CommonFsNodePermission::GetAttr.for_class(file_class)],
            current_task.into(),
        ),
        FS_IOC_SETFLAGS | FS_IOC_SETVERSION => has_file_permissions(
            &permission_check,
            current_task.kernel(),
            subject_sid,
            file,
            &[CommonFsNodePermission::SetAttr.for_class(file_class)],
            current_task.into(),
        ),
        FIONBIO | FIOASYNC => has_file_permissions(
            &permission_check,
            current_task.kernel(),
            subject_sid,
            file,
            &[],
            current_task.into(),
        ),
        _ => {
            // The ioctl command is the 2 least-significant bytes of `request`.
            let ioctl = request as u16;
            has_file_ioctl_permission(
                &permission_check,
                current_task.kernel(),
                subject_sid,
                file,
                ioctl,
                current_task.into(),
            )
        }
    }
}

/// Returns whether `current_task` can perform a lock operation on the given `file`.
pub(in crate::security) fn check_file_lock_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let subject_sid = task_effective_sid(current_task);
    let fs_node_class = file.node().security_state.lock().class;
    has_file_permissions(
        &permission_check,
        current_task.kernel(),
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
    let subject_sid = task_effective_sid(current_task);
    let fs_node_class = file.node().security_state.lock().class;

    match fcntl_cmd {
        F_GETLK | F_SETLK | F_SETLKW => {
            // Checks both the Lock and Use permissions.
            has_file_permissions(
                &permission_check,
                current_task.kernel(),
                subject_sid,
                file,
                &[CommonFsNodePermission::Lock.for_class(fs_node_class)],
                current_task.into(),
            )?;
        }
        _ => {
            // Only checks the Use permission.
            has_file_permissions(
                &permission_check,
                current_task.kernel(),
                subject_sid,
                file,
                &[],
                current_task.into(),
            )?;
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
                &current_task.kernel(),
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
            &current_task.kernel(),
            &security_server.as_permission_check(),
            subject_sid,
            file.node(),
            &permissions_from_flags(PermissionFlags::WRITE, fs_node_class),
            current_task.into(),
        )?;
    }
    Ok(())
}

/// Checks if the requested protection changes `prot` can be applied to `mapping`.
pub fn file_mprotect(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    mapping: &Mapping,
    prot: ProtectionFlags,
) -> Result<(), Errno> {
    if !mapping.can_exec() && prot.contains(ProtectionFlags::EXEC) {
        match mapping.name() {
            MappingName::Heap => {
                let current_sid = task_effective_sid(current_task);
                check_self_permission(
                    &security_server.as_permission_check(),
                    current_task.kernel(),
                    current_sid,
                    ProcessPermission::ExecHeap,
                    current_task.into(),
                )?;
            }
            MappingName::Stack => {
                track_stub!(TODO("https://fxbug.dev/415257144"), "Check `execstack`");
            }
            _ => {
                // TODO(b/409256444): Check `execmod`
            }
        };
    }
    let fs_node = if let MappingName::File(active_namespace_node) = mapping.name() {
        Some((&(*active_namespace_node).entry.node).clone())
    } else {
        None
    };
    let mapping_options = mapping.flags().options();
    file_map_prot_check(security_server, current_task, fs_node.as_ref(), prot, mapping_options)?;
    Ok(())
}

/// Checks if `current_task` can mmap `file` or anonymous memory with the given `protection_flags`
/// and `mapping_options`.
pub fn mmap_file(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: Option<&FileHandle>,
    protection_flags: ProtectionFlags,
    mapping_options: MappingOptions,
) -> Result<(), Errno> {
    if let Some(file) = file {
        let file_class = file.node().security_state.lock().class;
        let current_sid = task_effective_sid(current_task);
        todo_has_file_permissions(
            TODO_DENY!("https://fxbug.dev/405381460", "Check permissions when mapping."),
            &current_task.kernel(),
            &security_server.as_permission_check(),
            current_sid,
            file,
            &[CommonFsNodePermission::Map.for_class(file_class)],
            current_task.into(),
        )?;
    }
    let fs_node: Option<&FsNodeHandle> = file.map(|f| f.node());
    file_map_prot_check(security_server, current_task, fs_node, protection_flags, mapping_options)
}

/// Checks if `current_task` has the permission to set `prot` on a mapping
/// described by `mapping_options` potentially associated with `fs_node`.
fn file_map_prot_check(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: Option<&FsNodeHandle>,
    prot: ProtectionFlags,
    mapping_options: MappingOptions,
) -> Result<(), Errno> {
    // This function checks:
    // * `execmem` when mapping with `PROT_EXEC` an anonymous mapping.
    // * `execmem` when mapping with `PROT_EXEC` a writable private mapping.
    // * `read` when mapping a file.
    // * `write` when mapping a shared file with `PROT_WRITE`.
    // * `execute` when mapping a file with `PROT_EXEC`.
    if prot.contains(ProtectionFlags::EXEC) {
        let anonymous_mapping = mapping_options.contains(MappingOptions::ANONYMOUS);
        let private_writable_mapping = !mapping_options.contains(MappingOptions::SHARED)
            && prot.contains(ProtectionFlags::WRITE);
        if anonymous_mapping || private_writable_mapping {
            let current_sid = task_effective_sid(current_task);
            todo_check_permission(
                TODO_DENY!("https://fxbug.dev/405381460", "Check permissions when mapping."),
                &current_task.kernel(),
                &security_server.as_permission_check(),
                current_sid,
                current_sid,
                ProcessPermission::ExecMem,
                current_task.into(),
            )?;
        }
    }

    if let Some(fs_node) = fs_node {
        let node_class = fs_node.security_state.lock().class;
        let flags = {
            let mut flags: PermissionFlags = prot.into();
            // After mapping a file into memory you can read its content, so
            // the read permission needs to be checked.
            flags |= PermissionFlags::READ;
            if !mapping_options.contains(MappingOptions::SHARED) {
                // When mapping a file privately, the writes to the mapping
                // aren't propagated to the file, so there's no need to
                // check for the write permission.
                flags.remove(PermissionFlags::WRITE);
            }
            flags
        };
        let permissions = permissions_from_flags(flags, node_class);
        let current_sid = task_effective_sid(current_task);
        todo_has_fs_node_permissions(
            TODO_DENY!("https://fxbug.dev/405381460", "Check permissions when mapping."),
            &current_task.kernel(),
            &security_server.as_permission_check(),
            current_sid,
            fs_node,
            &permissions,
            current_task.into(),
        )?;
    }
    Ok(())
}
