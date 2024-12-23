// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod audit;
pub(super) mod superblock;
pub(super) mod task;
pub(super) mod testing;

use super::{FsNodeSecurityXattr, PermissionFlags};
use crate::task::{CurrentTask, Task};
use crate::vfs::fs_args::MountParams;
use crate::vfs::{
    DirEntry, DirEntryHandle, FileHandle, FileObject, FileSystem, FileSystemHandle, FsNode, FsStr,
    FsString, PathBuilder, UnlinkKind, ValueOrSize, XattrOp,
};
use crate::TODO_DENY;
use audit::{audit_log, AuditContext};
use bstr::BStr;
use linux_uapi::XATTR_NAME_SELINUX;
use selinux::permission_check::PermissionCheck;
use selinux::policy::FsUseType;
use selinux::{
    ClassPermission, CommonFilePermission, DirPermission, FdPermission, FileClass, FileSystemLabel,
    FileSystemLabelingScheme, FileSystemMountOptions, FileSystemPermission, InitialSid,
    ObjectClass, Permission, ProcessPermission, SecurityId, SecurityPermission, SecurityServer,
};
use starnix_logging::{log_debug, log_warn, track_stub, BugRef, __track_stub_inner};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Mutex};
use starnix_types::ownership::WeakRef;
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{Errno, ENODATA};
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{
    errno, error, FIGETBSZ, FIOASYNC, FIONBIO, FIONREAD, FS_IOC_GETFLAGS, FS_IOC_GETVERSION,
    FS_IOC_SETFLAGS, FS_IOC_SETVERSION,
};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

/// Maximum supported size for the extended attribute value used to store SELinux security
/// contexts in a filesystem node extended attributes.
const SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE: usize = 4096;

/// Returns the set of `Permissions` on `class`, corresponding to the specified `flags`.
fn permissions_from_flags(flags: PermissionFlags, class: FileClass) -> Vec<Permission> {
    let mut result = Vec::new();
    if flags.contains(PermissionFlags::READ) {
        result.push(CommonFilePermission::Read.for_class(class));
    }
    // SELinux uses the `APPEND` bit to distinguish which of the "append" or the more general
    // "write" permission to check for.
    if flags.contains(PermissionFlags::APPEND) {
        result.push(CommonFilePermission::Append.for_class(class));
    } else if flags.contains(PermissionFlags::WRITE) {
        result.push(CommonFilePermission::Write.for_class(class));
    }
    if flags.contains(PermissionFlags::EXEC) {
        if class == FileClass::Dir {
            result.push(DirPermission::Search.into());
        } else {
            result.push(CommonFilePermission::Execute.for_class(class));
        }
    }
    result
}

/// Checks that `current_task` has permission to "use" the specified `file`, and the specified
/// `permissions` to the underlying [`crate::vfs::FsNode`].
fn has_file_permissions(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    file: &FileObject,
    permissions: &[Permission],
) -> Result<(), Errno> {
    // Validate that the `subject` has the "fd { use }" permission to the `file`.
    // If the file and task security domains are identical then `fd { use }` is implicitly granted.
    let file_sid = file.security_state.state.sid;
    // TODO: https://fxbug.dev/385121365 - Should the kernel be implicitly allowed to "use" all FDs?
    if subject_sid != SecurityId::initial(InitialSid::Kernel) && subject_sid != file_sid {
        check_permission(permission_check, subject_sid, file_sid, FdPermission::Use)?;
    }

    // Validate that the `subject` has the desired `permissions`, if any, to the underlying node.
    if !permissions.is_empty() {
        has_fs_node_permissions(permission_check, subject_sid, file.node(), permissions)?;
    }

    Ok(())
}

/// Checks that `current_task` has the specified `permissions` to the `node`.
fn has_fs_node_permissions(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    node: &FsNode,
    permissions: &[Permission],
) -> Result<(), Errno> {
    // TODO: https://fxbug.dev/364568735 - Anon nodes and pipes are not yet labeled.
    // TODO: https://fxbug.dev/364568517 - Sockets are not yet labeled.
    if let Some(target) = get_cached_sid_and_class(node) {
        for permission in permissions {
            check_permission(permission_check, subject_sid, target.sid, permission.clone())?;
        }
    }

    Ok(())
}

/// Checks that `current_task` has `permissions` to `node`, with "todo_deny" on denial.
fn todo_has_fs_node_permissions(
    todo_deny: TodoDeny,
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    node: &FsNode,
    permissions: &[Permission],
) -> Result<(), Errno> {
    // TODO: https://fxbug.dev/364568735 - Anon nodes and pipes are not yet labeled.
    // TODO: https://fxbug.dev/364568517 - Sockets are not yet labeled.
    if let Some(target) = get_cached_sid_and_class(node) {
        for permission in permissions {
            todo_check_permission(
                todo_deny.clone(),
                permission_check,
                subject_sid,
                target.sid,
                permission.clone(),
            )?;
        }
    }

    Ok(())
}

/// Checks whether the `current_task`` has the permissions specified by `mask` to the `file`.
pub fn file_permission(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
    mut permission_flags: PermissionFlags,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let file_mode = file.name.entry.node.info().mode;
    let file_class = file_class_from_file_mode(file_mode)?;

    if file.flags().contains(OpenFlags::APPEND) {
        permission_flags |= PermissionFlags::APPEND;
    }

    has_file_permissions(&security_server.as_permission_check(), current_sid, file, &[])?;
    todo_has_fs_node_permissions(
        TODO_DENY!("https://fxbug.dev/385121365", "Enforce file_permission() checks"),
        &security_server.as_permission_check(),
        current_sid,
        file.node(),
        &permissions_from_flags(permission_flags, file_class),
    )
}

/// Returns the relative path from the root of the file system containing this `DirEntry`.
fn get_fs_relative_path(dir_entry: &DirEntryHandle) -> FsString {
    let mut path_builder = PathBuilder::new();
    let mut current_dir = dir_entry.clone();

    while let Some(parent) = current_dir.parent() {
        path_builder.prepend_element(&BStr::new(&current_dir.local_name()));
        current_dir = parent;
    }
    path_builder.build_absolute()
}

/// Called by the VFS to initialize the security state for an `FsNode` that is being linked at
/// `dir_entry`.
pub(super) fn fs_node_init_with_dentry<L>(
    locked: &mut Locked<'_, L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    dir_entry: &DirEntryHandle,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    // This hook is called every time an `FsNode` is linked to a `DirEntry`, so it is expected that
    // the `FsNode` may already have been labeled.
    let fs_node = &dir_entry.node;
    if fs_node.security_state.lock().label.is_initialized() {
        return Ok(());
    }

    // Attempt to derive a specific security class for the `FsNode`, based on its file mode.
    fs_node_ensure_class(&dir_entry.node)?;

    // If the parent has a from-task label then propagate it to the new node,  rather than applying
    // the filesystem's labeling scheme. This allows nodes in per-process and per-task directories
    // in "proc" to inherit the task's label.
    let parent = dir_entry.parent();
    if let Some(parent) = parent {
        let parent_node = &parent.node;
        if let FsNodeLabel::FromTask { weak_task } = parent_node.security_state.lock().label.clone()
        {
            fs_node_set_label_with_task(fs_node, weak_task);
            return Ok(());
        }
    }

    // Obtain labeling information for the `FileSystem`. If none has been resolved yet then queue the
    // `dir_entry` to be labeled later.
    let fs = fs_node.fs();
    let label = match &mut *fs.security_state.state.0.lock() {
        FileSystemLabelState::Unlabeled { pending_entries, .. } => {
            log_debug!("Queuing FsNode for {:?} for labeling", dir_entry);
            pending_entries.insert(WeakKey::from(dir_entry));
            return Ok(());
        }
        FileSystemLabelState::Labeled { label } => label.clone(),
    };

    let sid = match label.scheme {
        // mountpoint-labelling labels every node from the "context=" mount option.
        FileSystemLabelingScheme::Mountpoint { sid } => sid,
        // fs_use_xattr-labelling defers to the security attribute on the file node, with fall-back
        // behaviours for missing and invalid labels.
        FileSystemLabelingScheme::FsUse { fs_use_type, def_sid, root_sid, .. } => {
            match fs_use_type {
                FsUseType::Xattr => {
                    // Determine the SID from the "security.selinux" attribute.
                    let attr = fs_node.ops().get_xattr(
                        &mut locked.cast_locked::<FileOpsCore>(),
                        fs_node,
                        current_task,
                        XATTR_NAME_SELINUX.to_bytes().into(),
                        SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
                    );
                    let maybe_sid = match attr {
                        Ok(ValueOrSize::Value(security_context)) => Some(
                            security_server
                                .security_context_to_sid((&security_context).into())
                                .unwrap_or_else(|_| SecurityId::initial(InitialSid::Unlabeled)),
                        ),
                        Ok(ValueOrSize::Size(_)) => None,
                        Err(err) => {
                            if err.code == ENODATA && dir_entry.parent().is_none() {
                                // The root node of xattr-labeled filesystems should be labeled at
                                // creation in principle. Distinguishing creation of the root of the
                                // filesystem from re-instantiation of the `FsNode` representing an
                                // existing root is tricky, so we work-around the issue by writing
                                // the `root_sid` label here.
                                let root_context =
                                    security_server.sid_to_security_context(root_sid).unwrap();
                                fs_node.ops().set_xattr(
                                    &mut locked.cast_locked::<FileOpsCore>(),
                                    fs_node,
                                    current_task,
                                    XATTR_NAME_SELINUX.to_bytes().into(),
                                    root_context.as_slice().into(),
                                    XattrOp::Create,
                                )?;
                                Some(root_sid)
                            } else {
                                // TODO: https://fxbug.dev/334094811 - Determine how to handle errors besides
                                // `ENODATA` (no such xattr).
                                None
                            }
                        }
                    };
                    maybe_sid.unwrap_or_else(||{
                        // The node does not have a label, so apply the filesystem's default SID.
                        if fs.name() == "remotefs" {
                            track_stub!(TODO("https://fxbug.dev/378688761"), "RemoteFS node missing security label. Perhaps your device needs re-flashing?");
                        } else {
                            log_warn!(
                                "Unlabeled node {:?} in {} ({:?}-labeled) filesystem",
                                dir_entry,
                                fs.name(),
                                fs_use_type
                            );
                        };
                        def_sid
                    })
                }
                _ => {
                    if let Some(parent) = dir_entry.parent() {
                        // Ephemeral nodes are then labeled by applying SID computation between their
                        // SID of the task that created them, and their parent file node's label.
                        // TODO: https://fxbug.dev/381275592 - Use the SID from the creating task, rather than current_task!
                        return fs_node_init_on_create(
                            security_server,
                            current_task,
                            fs_node,
                            &parent.node,
                        )
                        .map(|_| ());
                    } else {
                        // Ephemeral filesystems' root nodes use the root SID, which will typically
                        // be the same as that of the filesystem itself.
                        root_sid
                    }
                }
            }
        }
        FileSystemLabelingScheme::GenFsCon => {
            let fs_type = fs_node.fs().name();
            let fs_node_class = fs_node.security_state.lock().class;

            // This will give us the path of the node from the root node of the filesystem,
            // excluding the path of the filesystem's mount point. For example, assuming that
            // filesystem "proc" is mounted in "/proc" and if the actual full path to the
            // fs_node is "/proc/bootconfig" then, get_fs_relative_path will return
            // "/bootconfig". This matches the path definitions in the genfscon statements.
            let sub_path = if fs_node_class == FileClass::Link {
                // Investigation for https://fxbug.dev/378863048 suggests that symlinks' paths are
                // ignored, so that they use the filesystem's root label.
                "/".into()
            } else {
                get_fs_relative_path(dir_entry)
            };

            let class_id = security_server
                .class_id_by_name(ObjectClass::from(fs_node_class).name())
                .map_err(|_| errno!(EINVAL))?;

            security_server
                .genfscon_label_for_fs_and_path(
                    fs_type.into(),
                    sub_path.as_slice().into(),
                    Some(class_id),
                )
                .unwrap_or_else(|| SecurityId::initial(InitialSid::Unlabeled))
        }
    };

    set_cached_sid(&fs_node, sid);

    Ok(())
}

/// Returns an [`FsNodeSecurityXattr`] for the security context of `sid`.
fn make_fs_node_security_xattr(
    security_server: &SecurityServer,
    sid: SecurityId,
) -> Result<FsNodeSecurityXattr, Errno> {
    security_server
        .sid_to_security_context(sid)
        .map(|value| FsNodeSecurityXattr {
            name: XATTR_NAME_SELINUX.to_bytes().into(),
            value: value.into(),
        })
        .ok_or_else(|| errno!(EINVAL))
}

fn file_class_from_file_mode(mode: FileMode) -> Result<FileClass, Errno> {
    let file_type = mode.bits() & starnix_uapi::S_IFMT;
    match file_type {
        starnix_uapi::S_IFLNK => Ok(FileClass::Link),
        starnix_uapi::S_IFREG => Ok(FileClass::File),
        starnix_uapi::S_IFDIR => Ok(FileClass::Dir),
        starnix_uapi::S_IFCHR => Ok(FileClass::Character),
        starnix_uapi::S_IFBLK => Ok(FileClass::Block),
        starnix_uapi::S_IFIFO => Ok(FileClass::Fifo),
        starnix_uapi::S_IFSOCK => Ok(FileClass::Socket),
        0 => {
            track_stub!(TODO("https://fxbug.dev/378864191"), "File with zero IFMT?");
            Ok(FileClass::File)
        }
        _ => error!(EINVAL, format!("mode: {:?}", mode)),
    }
}

#[derive(Clone, Debug)]
pub struct TodoDeny {
    bug: BugRef,
    message: &'static str,
    location: &'static std::panic::Location<'static>,
}

#[macro_export]
macro_rules! TODO_DENY {
    ($bug_url:literal, $message:literal) => {{
        use crate::security::selinux_hooks::TodoDeny;
        use starnix_logging::bug_ref;
        TodoDeny {
            bug: bug_ref!($bug_url),
            message: $message,
            location: std::panic::Location::caller(),
        }
    }};
}

/// Perform the specified check as would `check_permission()`, but report denials as "todo_deny" in
/// the audit output, without actually denying access.fx build
pub(super) fn todo_check_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    todo_deny: TodoDeny,
    permission_check: &PermissionCheck<'_>,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
) -> Result<(), Errno> {
    let _ = check_permission_internal(
        permission_check,
        source_sid,
        target_sid,
        permission,
        |mut context| {
            if !context.result.permit {
                // Audit-log the first few denials, but skip further denials to avoid logspamming.
                const MAX_TODO_AUDIT_DENIALS: u64 = 5;

                // Re-using the `track_stub!()` internals to track the denial, and determine whether
                // too many denial audit logs have already been emit for this case.
                if __track_stub_inner(todo_deny.bug, todo_deny.message, None, todo_deny.location)
                    > MAX_TODO_AUDIT_DENIALS
                {
                    return;
                }
                context.set_decision("todo_deny");
            }

            audit_log(context);
        },
    );
    Ok(())
}

/// Returns the SID with which an `FsNode` of `new_node_class` would be labeled, if created by
/// `current_task` under the specified `parent` node.
/// Policy-defined labeling rules, including transitions, are taken into account.
///
/// If no policy has yet been applied to the `parent`s [`create:vfs::FileSystem`] then no SID
/// is returned.
fn compute_new_fs_node_sid(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    new_node_class: FileClass,
) -> Result<Option<(SecurityId, FileSystemLabel)>, Errno> {
    let fs = parent.fs();
    let label = match &*fs.security_state.state.0.lock() {
        FileSystemLabelState::Unlabeled { .. } => {
            return Ok(None);
        }
        FileSystemLabelState::Labeled { label } => label.clone(),
    };

    // Determine the SID with which the new `FsNode` would be labeled.
    match label.scheme {
        // TODO: https://fxbug.dev/377915469 - How should creation of new files under "genfscon" be handled?
        FileSystemLabelingScheme::GenFsCon => {
            track_stub!(TODO("https://fxbug.dev/377915469"), "New file in genfscon fs");
            Ok(Some((label.sid, label)))
        }
        FileSystemLabelingScheme::Mountpoint { sid } => Ok(Some((sid, label))),
        FileSystemLabelingScheme::FsUse { fs_use_type, .. } => {
            if let Some(fscreate_sid) = current_task.security_state.lock().fscreate_sid {
                return Ok(Some((fscreate_sid, label)));
            }

            let current_task_sid = current_task.security_state.lock().current_sid;
            if fs_use_type == FsUseType::Task {
                // TODO: https://fxbug.dev/377912777 - verify that this is how fs_use_task is
                // supposed to work (https://selinuxproject.org/page/NB_ComputingSecurityContexts).
                Ok(Some((current_task_sid, label)))
            } else {
                let parent_sid = fs_node_effective_sid_and_class(parent).sid;
                let sid = security_server
                    .as_permission_check()
                    .compute_new_file_sid(current_task_sid, parent_sid, new_node_class)
                    // TODO: https://fxbug.dev/377915452 - is EPERM right here? What does it mean
                    // for compute_new_file_sid to have failed?
                    .map_err(|_| errno!(EPERM))?;
                Ok(Some((sid, label)))
            }
        }
    }
}

/// Called by file-system implementations when creating the `FsNode` for a new file.
pub(super) fn fs_node_init_on_create(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    new_node: &FsNode,
    parent: &FsNode,
) -> Result<Option<FsNodeSecurityXattr>, Errno> {
    // By definition this is a new `FsNode` so should not have already been labeled
    // (unless we're working in the context of overlayfs and affected by
    // https://fxbug.dev/369067922).
    if new_node.security_state.lock().label.is_initialized() {
        track_stub!(TODO("https://fxbug.dev/369067922"), "new FsNode already labeled");
    }

    // If the `new_node` does not already have a specific security class selected then choose one
    // based on its file mode.
    fs_node_ensure_class(new_node)?;

    // Determine the SID with which to label the `new_node` with, dependent on the file
    // class, etc. This will only fail if the filesystem containing the nodes does not yet
    // have labeling information resolved.
    let new_node_class = new_node.security_state.lock().class;
    if let Some((sid, label)) =
        compute_new_fs_node_sid(security_server, current_task, parent, new_node_class)?
    {
        // If the labeling scheme is "fs_use_xattr" then also construct an xattr value
        // for the caller to apply to `new_node`.
        let (sid, xattr) = match label.scheme {
            FileSystemLabelingScheme::FsUse { fs_use_type, .. } => {
                let xattr = (fs_use_type == FsUseType::Xattr)
                    .then(|| make_fs_node_security_xattr(security_server, sid))
                    .transpose()?;
                (sid, xattr)
            }
            FileSystemLabelingScheme::Mountpoint { .. } => (sid, None),
            FileSystemLabelingScheme::GenFsCon => {
                // Defer labeling to `fs_node_init_with_dentry()`, so that the path of the new
                // node can be taken into account.
                return Ok(None);
            }
        };

        set_cached_sid(new_node, sid);

        Ok(xattr)
    } else {
        Ok(None)
    }
}

/// Called to label file nodes not linked in any filesystem's directory structure, e.g. sockets,
/// usereventfds, etc.
pub(super) fn fs_node_init_anon(
    _security_server: &SecurityServer,
    current_task: &CurrentTask,
    new_node: &FsNode,
    _node_type: &str,
) {
    track_stub!(TODO("https://fxbug.dev/364568735"), "Apply labeling rules to anon_inodes");
    let sid = current_task.security_state.lock().current_sid;
    let mut state = new_node.security_state.lock();
    state.class = FileClass::AnonFsNode;
    state.label = FsNodeLabel::SecurityId { sid };
}

/// Helper used by filesystem node creation checks to validate that `current_task` has necessary
/// permissions to create a new node under the specified `parent`.
fn may_create(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    new_file_mode: FileMode, // Only used to determine the file class.
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();

    // Verify that the caller has permissions required to add new entries to the target
    // directory node.
    let current_sid = current_task.security_state.lock().current_sid;
    let parent_sid = fs_node_effective_sid_and_class(parent).sid;

    todo_check_permission(
        TODO_DENY!("https://fxbug.dev/374910392", "Check search permission."),
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::Search,
    )?;
    todo_check_permission(
        TODO_DENY!("https://fxbug.dev/374910392", "Check add_name permission."),
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::AddName,
    )?;

    // Verify that the caller has permission to create new nodes of the desired type.
    let new_file_class = file_class_from_file_mode(new_file_mode)?;
    let new_file_sid =
        compute_new_fs_node_sid(security_server, current_task, parent, new_file_class)?
            .map(|(sid, _)| sid)
            .unwrap_or_else(|| SecurityId::initial(InitialSid::File));
    todo_check_permission(
        TODO_DENY!("https://fxbug.dev/375381156", "Check create permission."),
        &permission_check,
        current_sid,
        new_file_sid,
        CommonFilePermission::Create.for_class(new_file_class),
    )?;

    // Verify that the new node's label is permitted to be created in the target filesystem.
    let filesystem_sid = match &*parent.fs().security_state.state.0.lock() {
        FileSystemLabelState::Labeled { label } => Ok(label.sid),
        FileSystemLabelState::Unlabeled { .. } => {
            track_stub!(
                TODO("https://fxbug.dev/367585803"),
                "may_create() should not be called until policy load has completed"
            );
            error!(EPERM)
        }
    }?;
    todo_check_permission(
        TODO_DENY!("https://fxbug.dev/375381156", "Check associate permission."),
        &permission_check,
        new_file_sid,
        filesystem_sid,
        FileSystemPermission::Associate,
    )?;

    Ok(())
}

/// Helper that checks whether the `current_task` can create a new link to the `existing` file or
/// directory in the `parent` directory. Called by [`check_fs_node_link_access`].
fn may_link(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    existing_node: &FsNode,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let current_sid = current_task.security_state.lock().current_sid;
    let parent_sid = fs_node_effective_sid_and_class(parent).sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(existing_node);

    check_permission(&permission_check, current_sid, parent_sid, DirPermission::Search)?;
    check_permission(&permission_check, current_sid, parent_sid, DirPermission::AddName)?;
    check_permission(
        &permission_check,
        current_sid,
        file_sid,
        CommonFilePermission::Link.for_class(file_class),
    )?;
    Ok(())
}

/// Helper that checks whether the `current_task` can unlink or rmdir an `fs_node` from its
/// `parent` directory.
/// If [`operation`] is [`UnlinkKind::Directory`] this will check permissions for rmdir;
/// otherwise for unlink.
/// Called by [`check_fs_node_unlink_access`] and [`check_fs_node_rmdir_access`] .
fn may_unlink_or_rmdir(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    fs_node: &FsNode,
    operation: UnlinkKind,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let current_sid = current_task.security_state.lock().current_sid;
    let parent_sid = fs_node_effective_sid_and_class(parent).sid;

    check_permission(&permission_check, current_sid, parent_sid, DirPermission::Search)?;
    check_permission(&permission_check, current_sid, parent_sid, DirPermission::RemoveName)?;

    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    match operation {
        UnlinkKind::NonDirectory => check_permission(
            &permission_check,
            current_sid,
            file_sid,
            CommonFilePermission::Unlink.for_class(file_class),
        )?,
        UnlinkKind::Directory => {
            check_permission(&permission_check, current_sid, file_sid, DirPermission::RemoveDir)?
        }
    }
    Ok(())
}

/// Validate that `current_task` has permission to create a regular file in the `parent` directory,
/// with the specified file `mode`.
pub(super) fn check_fs_node_create_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, mode)
}

/// Validate that `current_task` has permission to create a symlink to `old_path` in the `parent`
/// directory.
pub(super) fn check_fs_node_symlink_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    _old_path: &FsStr,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, FileMode::IFLNK)
}

/// Validate that `current_task` has permission to create a new directory in the `parent` directory,
/// with the specified file `mode`.
pub(super) fn check_fs_node_mkdir_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, mode)
}

/// Validate that `current_task` has permission to create a new special file, socket or pipe, in the
/// `parent` directory, and with the specified file `mode` and `device_id`.
pub(super) fn check_fs_node_mknod_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
    _device_id: DeviceType,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, mode)
}

/// Validate that `current_task` has the permission to create a new hard link to a file.
pub(super) fn check_fs_node_link_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    target_directory: &FsNode,
    existing_node: &FsNode,
) -> Result<(), Errno> {
    may_link(security_server, current_task, target_directory, existing_node)
}

/// Validate that `current_task` has the permission to remove a hard link to a file.
pub(super) fn check_fs_node_unlink_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    assert!(!child.is_dir());

    may_unlink_or_rmdir(security_server, current_task, parent, child, UnlinkKind::NonDirectory)
}

/// Validate that `current_task` has the permission to remove a directory.
pub(super) fn check_fs_node_rmdir_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    assert!(child.is_dir());

    may_unlink_or_rmdir(security_server, current_task, parent, child, UnlinkKind::Directory)
}

/// Validates that `current_task` has the permissions to move `moving_node`.
pub(super) fn check_fs_node_rename_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    old_parent: &FsNode,
    moving_node: &FsNode,
    new_parent: &FsNode,
    replaced_node: Option<&FsNode>,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let current_sid = current_task.security_state.lock().current_sid;
    let old_parent_sid = fs_node_effective_sid_and_class(old_parent).sid;

    check_permission(&permission_check, current_sid, old_parent_sid, DirPermission::Search)?;
    check_permission(&permission_check, current_sid, old_parent_sid, DirPermission::RemoveName)?;

    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(moving_node);
    check_permission(
        &permission_check,
        current_sid,
        file_sid,
        CommonFilePermission::Rename.for_class(file_class),
    )?;

    let new_parent_sid = fs_node_effective_sid_and_class(new_parent).sid;
    check_permission(&permission_check, current_sid, new_parent_sid, DirPermission::AddName)?;

    // If a file already exists with the new name, then verify that the existing file can be
    // removed.
    if let Some(replaced_node) = replaced_node {
        let replaced_node_class = replaced_node.security_state.lock().class;
        may_unlink_or_rmdir(
            security_server,
            current_task,
            new_parent,
            replaced_node,
            if replaced_node_class == FileClass::Dir {
                UnlinkKind::Directory
            } else {
                UnlinkKind::NonDirectory
            },
        )?;
    }

    if !std::ptr::eq(old_parent, new_parent) {
        // If the parent nodes are the same directory, we have already verified the search
        // permission during the `old_parent_sid` verification.
        check_permission(&permission_check, current_sid, new_parent_sid, DirPermission::Search)?;

        // If the file is a directory and its parent directory is being changed by the rename,
        // we additionally check for the reparent permission. Note that the `reparent` permission is
        // only defined for directories.
        if file_class == FileClass::Dir {
            check_permission(&permission_check, current_sid, file_sid, DirPermission::Reparent)?;
        }
    }

    Ok(())
}

/// Validates that `current_task` has the permissions to read the symbolic link `fs_node`.
pub(super) fn check_fs_node_read_link_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    check_permission(
        &security_server.as_permission_check(),
        current_sid,
        file_sid,
        CommonFilePermission::Read.for_class(file_class),
    )
}

/// Validates that the `current_task` has the permissions to access `fs_node`.
pub fn fs_node_permission(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    permission_flags: PermissionFlags,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let file_class = fs_node.security_state.lock().class;
    todo_has_fs_node_permissions(
        TODO_DENY!("https://fxbug.dev/380855359", "Enforce fs_node_permission checks."),
        &security_server.as_permission_check(),
        current_sid,
        fs_node,
        &permissions_from_flags(permission_flags, file_class),
    )
}

pub(super) fn check_fs_node_getattr_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    todo_check_permission(
        TODO_DENY!("https://fxbug.dev/383284672", "Enable permission checks in getattr."),
        &security_server.as_permission_check(),
        current_sid,
        file_sid,
        CommonFilePermission::GetAttr.for_class(file_class),
    )
}

pub(super) fn check_fs_node_setxattr_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    _name: &FsStr,
    _value: &FsStr,
    _op: XattrOp,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    check_permission(
        &security_server.as_permission_check(),
        current_sid,
        file_sid,
        CommonFilePermission::SetAttr.for_class(file_class),
    )
}

pub(super) fn check_fs_node_getxattr_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    _name: &FsStr,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    check_permission(
        &security_server.as_permission_check(),
        current_sid,
        file_sid,
        CommonFilePermission::GetAttr.for_class(file_class),
    )
}

pub(super) fn check_fs_node_listxattr_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    check_permission(
        &security_server.as_permission_check(),
        current_sid,
        file_sid,
        CommonFilePermission::GetAttr.for_class(file_class),
    )
}

pub(super) fn check_fs_node_removexattr_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    _name: &FsStr,
) -> Result<(), Errno> {
    // TODO: https://fxbug.dev/364568818 - Verify the correct permission check here; is removing a
    // security.* attribute even allowed?
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    check_permission(
        &security_server.as_permission_check(),
        current_sid,
        file_sid,
        CommonFilePermission::SetAttr.for_class(file_class),
    )
}

/// Returns whether `current_task` can issue an ioctl to `file`.
pub(super) fn check_file_ioctl_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    file: &FileObject,
    request: u32,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();
    let subject_sid = current_task.security_state.lock().current_sid;
    has_file_permissions(&permission_check, subject_sid, file, &[])?;

    let file_class = file.node().security_state.lock().class;
    let permissions: &[Permission] = match request {
        // The NSA report also has `FIBMAP` follow this branch.
        FIONREAD | FIGETBSZ | FS_IOC_GETFLAGS | FS_IOC_GETVERSION => {
            &[CommonFilePermission::GetAttr.for_class(file_class)]
        }
        FS_IOC_SETFLAGS | FS_IOC_SETVERSION => {
            &[CommonFilePermission::SetAttr.for_class(file_class)]
        }
        FIONBIO | FIOASYNC => &[],
        _ => &[CommonFilePermission::Ioctl.for_class(file_class)],
    };
    todo_has_fs_node_permissions(
        TODO_DENY!("https://fxbug.dev/385077129", "Enforce file_ioctl() fs-node checks"),
        &permission_check,
        subject_sid,
        file.node(),
        permissions,
    )
}

/// If `fs_node` is in a filesystem without xattr support, returns the xattr name for the security
/// label (i.e. "security.selinux"). Otherwise returns None.
pub(super) fn fs_node_listsecurity(fs_node: &FsNode) -> Option<FsString> {
    if fs_node.fs().security_state.state.supports_xattr() {
        None
    } else {
        Some(XATTR_NAME_SELINUX.to_bytes().into())
    }
}

/// Returns the Security Context corresponding to the SID with which `FsNode`
/// is labelled, otherwise delegates to the node's [`crate::vfs::FsNodeOps`].
pub(super) fn fs_node_getsecurity<L>(
    locked: &mut Locked<'_, L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    max_size: usize,
) -> Result<ValueOrSize<FsString>, Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    if name == FsStr::new(XATTR_NAME_SELINUX.to_bytes()) {
        let sid = fs_node_effective_sid_and_class(&fs_node).sid;
        if sid != SecurityId::initial(InitialSid::Unlabeled) {
            if let Some(context) = security_server.sid_to_security_context(sid) {
                return Ok(ValueOrSize::Value(context.into()));
            }
        }

        // If the node is still unlabelled at this point then it most likely does have a value set for
        // "security.selinux", but the value is not a valid Security Context, so we defer to the
        // attribute value stored in the file system for this node.
    }

    fs_node.ops().get_xattr(
        &mut locked.cast_locked::<FileOpsCore>(),
        fs_node,
        current_task,
        name,
        max_size,
    )
}

/// Sets the `name`d security attribute on `fs_node` and updates internal
/// kernel state.
pub(super) fn fs_node_setsecurity<L>(
    locked: &mut Locked<'_, L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    value: &FsStr,
    op: XattrOp,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    if name != FsStr::new(XATTR_NAME_SELINUX.to_bytes()) {
        return fs_node.ops().set_xattr(
            &mut locked.cast_locked::<FileOpsCore>(),
            fs_node,
            current_task,
            name,
            value,
            op,
        );
    }

    // If the "security.selinux" attribute is being modified then the result depends on the
    // `FileSystem`'s labeling scheme.
    let fs = fs_node.fs();
    let fs_label = match &mut *fs.security_state.state.0.lock() {
        FileSystemLabelState::Unlabeled { .. } => {
            // If the `FileSystem` has not yet been labeled then store the xattr but leave the
            // label on the in-memory `fs_node` to be set up when the `FileSystem`'s labeling state
            // has been initialized, during load of the initial policy.
            return fs_node.ops().set_xattr(
                &mut locked.cast_locked::<FileOpsCore>(),
                fs_node,
                current_task,
                name,
                value,
                op,
            );
        }
        FileSystemLabelState::Labeled { label } => label.clone(),
    };

    // If the "mountpoint"-labeling is used by this filesystem then setting labels is not supported.
    // TODO: https://fxbug.dev/377915469 - Is re-labeling of "genfscon" nodes allowed?
    if let FileSystemLabelingScheme::Mountpoint { .. } = fs_label.scheme {
        return error!(ENOTSUP);
    }

    // TODO: https://fxbug.dev/367585803 - Lock the `fs_node` security label here, to ensure consistency.

    // Verify that the requested modification is permitted by the loaded policy.
    let new_sid = security_server.security_context_to_sid(value.into()).ok();
    if security_server.is_enforcing() {
        let new_sid = new_sid.ok_or_else(|| errno!(EINVAL))?;
        let task_sid = current_task.security_state.lock().current_sid;
        let FsNodeSidAndClass { sid: old_sid, class: file_class } =
            fs_node_effective_sid_and_class(fs_node);
        let permission_check = security_server.as_permission_check();
        if old_sid == SecurityId::initial(InitialSid::File) {
            check_permission(
                &permission_check,
                task_sid,
                old_sid,
                CommonFilePermission::RelabelFrom.for_class(file_class),
            )?;
        } else {
            check_permission(
                &permission_check,
                task_sid,
                old_sid,
                CommonFilePermission::RelabelFrom.for_class(file_class),
            )?;
        }
        check_permission(
            &permission_check,
            task_sid,
            new_sid,
            CommonFilePermission::RelabelTo.for_class(file_class),
        )?;
        check_permission(
            &permission_check,
            new_sid,
            fs_label.sid,
            FileSystemPermission::Associate,
        )?;
    }

    // Apply the change to the file node.
    let result = fs_node.ops().set_xattr(
        &mut locked.cast_locked::<FileOpsCore>(),
        fs_node,
        current_task,
        name,
        value,
        op,
    );

    // If the operation succeeded then update the label cached on the file node.
    if result.is_ok() {
        let effective_new_sid =
            new_sid.unwrap_or_else(|| SecurityId::initial(InitialSid::Unlabeled));
        set_cached_sid(fs_node, effective_new_sid);
    }

    result
}

/// Returns the `SecurityId` that should be used for SELinux access control checks against `fs_node`.
fn fs_node_effective_sid_and_class(fs_node: &FsNode) -> FsNodeSidAndClass {
    let maybe_sid_and_class = get_cached_sid_and_class(&fs_node);
    if let Some(sid_and_class) = maybe_sid_and_class {
        return sid_and_class;
    }

    // We should never reach here, but for now enforce it (see above) in debug builds.
    let fs_name = fs_node.fs().name();
    if fs_name == "anon" {
        track_stub!(TODO("https://fxbug.dev/376237171"), "Label anon nodes properly");
    } else if fs_name == "sockfs" {
        track_stub!(TODO("https://fxbug.dev/364568517"), "Label socket nodes properly");
    } else if fs_name == "pipefs" {
        track_stub!(TODO("https://fxbug.dev/380448690"), "Label fifo nodes properly");
    } else {
        #[cfg(is_debug)]
        panic!(
            "Unlabeled FsNode@{} of class {:?} in {}",
            fs_node.info().ino,
            file_class_from_file_mode(fs_node.info().mode),
            fs_node.fs().name()
        );
        #[cfg(not(is_debug))]
        track_stub!(TODO("https://fxbug.dev/381210513"), "SID requested for unlabeled FsNode");
    }

    FsNodeSidAndClass { sid: SecurityId::initial(InitialSid::Unlabeled), class: FileClass::File }
}

/// Checks whether `source_sid` is allowed the specified `permission` on `target_sid`.
fn check_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    permission_check: &PermissionCheck<'_>,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
) -> Result<(), Errno> {
    check_permission_internal(permission_check, source_sid, target_sid, permission, audit_log)
}

/// Checks that `subject_sid` has the specified process `permission` on `self`.
fn check_self_permission(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    permission: ProcessPermission,
) -> Result<(), Errno> {
    check_permission(permission_check, subject_sid, subject_sid, permission)
}

fn check_permission_internal<P: ClassPermission + Into<Permission> + Clone + 'static>(
    permission_check: &PermissionCheck<'_>,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
    audit_hook: impl FnOnce(AuditContext<'_>),
) -> Result<(), Errno> {
    let result = permission_check.has_permission(source_sid, target_sid, permission.clone());

    if result.audit {
        let context = AuditContext::new(
            permission_check,
            result.clone(),
            source_sid,
            target_sid,
            permission.into(),
        );
        audit_hook(context);
    }

    if result.permit {
        Ok(())
    } else {
        error!(EACCES)
    }
}

/// Returns the security state structure for the kernel.
pub(super) fn kernel_init_security() -> KernelState {
    KernelState {
        server: SecurityServer::new(),
        pending_file_systems: Mutex::default(),
        selinuxfs_null: OnceLock::default(),
    }
}

/// Return security state to associate with a filesystem based on the supplied mount options.
pub(super) fn file_system_init_security(
    name: &'static FsStr,
    mount_params: &MountParams,
) -> Result<FileSystemState, Errno> {
    let context = mount_params.get(FsStr::new(b"context")).cloned();
    let def_context = mount_params.get(FsStr::new(b"defcontext")).cloned();
    let fs_context = mount_params.get(FsStr::new(b"fscontext")).cloned();
    let root_context = mount_params.get(FsStr::new(b"rootcontext")).cloned();

    // If a "context" is specified then it is used for all nodes in the filesystem, so the other
    // security context options would not be meaningful to combine with it, except "fscontext".
    if context.is_some() && (def_context.is_some() || root_context.is_some()) {
        return error!(EINVAL);
    }

    let mount_options = FileSystemMountOptions {
        context: context.map(Into::into),
        def_context: def_context.map(Into::into),
        fs_context: fs_context.map(Into::into),
        root_context: root_context.map(Into::into),
    };

    Ok(FileSystemState::new(name, mount_options))
}

/// Resolves the labeling scheme and arguments for the `file_system`, based on the loaded policy.
pub(super) fn file_system_resolve_security<L>(
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
        let (resolved_label, pending_entries) = match &mut *label_state {
            FileSystemLabelState::Labeled { .. } => return Ok(()),
            FileSystemLabelState::Unlabeled { name, mount_options, pending_entries } => (
                {
                    // TODO: https://fxbug.dev/361297862 - Replace this workaround with more
                    // general handling of these special Fuchsia filesystems.
                    let effective_name = if *name == "remotefs" || *name == "remote_bundle" {
                        track_stub!(
                            TODO("https://fxbug.dev/361297862"),
                            "Applying ext4 labeling configuration to remote filesystems"
                        );
                        "ext4".into()
                    } else {
                        *name
                    };
                    security_server.resolve_fs_label(effective_name.into(), mount_options)
                },
                std::mem::take(pending_entries),
            ),
        };
        *label_state = FileSystemLabelState::Labeled { label: resolved_label };
        pending_entries
    };

    if let Some(root_dir_entry) = file_system.maybe_root() {
        fs_node_init_with_dentry(locked, security_server, current_task, root_dir_entry)?;
    }

    // Label the `FsNode`s for any `pending_entries`.
    let labeled_entries = pending_entries.len();
    for dir_entry in pending_entries {
        if let Some(dir_entry) = dir_entry.0.upgrade() {
            fs_node_init_with_dentry(locked, security_server, current_task, &dir_entry)
                .unwrap_or_else(|_| panic!("Failed to resolve FsNode label"));
        }
    }
    log_debug!("Labeled {} entries in {} FileSystem", labeled_entries, file_system.name());

    Ok(())
}

/// Returns the security state for a new file object created by `current_task`.
pub fn file_alloc_security(current_task: &CurrentTask) -> FileObjectState {
    FileObjectState { sid: current_task.security_state.lock().current_sid }
}

pub(super) fn selinuxfs_init_null(current_task: &CurrentTask, null_file_handle: &FileHandle) {
    let kernel_state = current_task
        .kernel()
        .security_state
        .state
        .as_ref()
        .expect("selinux kernel state exists when selinux is enabled");

    kernel_state
        .selinuxfs_null
        .set(null_file_handle.clone())
        .expect("selinuxfs null file initialized at most once");
}

/// Called by the "selinuxfs" when a policy has been successfully loaded, to allow policy-dependent
/// initialization to be completed.
pub(super) fn selinuxfs_policy_loaded<L>(
    locked: &mut Locked<'_, L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
) where
    L: LockEqualOrBefore<FileOpsCore>,
{
    let kernel_state = current_task.kernel().security_state.state.as_ref().unwrap();

    // Invoke `file_system_resolve_security()` on all pre-existing `FileSystem`s.
    // No new `FileSystem`s should be added to `pending_file_systems` after policy load.
    let pending_file_systems = std::mem::take(&mut *kernel_state.pending_file_systems.lock());
    for file_system in pending_file_systems {
        if let Some(file_system) = file_system.0.upgrade() {
            file_system_resolve_security(locked, security_server, current_task, &file_system)
                .unwrap_or_else(|_| {
                    panic!("Failed to resolve {} FileSystem label", file_system.name())
                });
        }
    }
}

/// Used by the "selinuxfs" module to perform checks on SELinux API file accesses.
pub(super) fn selinuxfs_check_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    permission: SecurityPermission,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = SecurityId::initial(InitialSid::Security);
    let permission_check = security_server.as_permission_check();
    check_permission(&permission_check, source_sid, target_sid, permission)
}

/// The global SELinux security structures, held by the `Kernel`.
pub(super) struct KernelState {
    // Owning reference to the SELinux `SecurityServer`.
    pub(super) server: Arc<SecurityServer>,

    /// Set of [`create::vfs::FileSystem`]s that have been constructed, and must be labeled as soon
    /// as a policy is loaded into the `server`.
    pub(super) pending_file_systems: Mutex<HashSet<WeakKey<FileSystem>>>,

    /// Stashed reference to "/sys/fs/selinux/null" used for replacing inaccessible file descriptors
    /// with a null file.
    pub(super) selinuxfs_null: OnceLock<FileHandle>,
}

/// The SELinux security structure for `ThreadGroup`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TaskAttrs {
    /// Current SID for the task.
    pub current_sid: SecurityId,

    /// SID for the task upon the next execve call.
    pub exec_sid: Option<SecurityId>,

    /// SID for files created by the task.
    pub fscreate_sid: Option<SecurityId>,

    /// SID for kernel-managed keys created by the task.
    pub keycreate_sid: Option<SecurityId>,

    /// SID prior to the last execve.
    pub previous_sid: SecurityId,

    /// SID for sockets created by the task.
    pub sockcreate_sid: Option<SecurityId>,
}

impl TaskAttrs {
    /// Returns initial state for kernel tasks.
    pub(super) fn for_kernel() -> Self {
        Self::for_sid(SecurityId::initial(InitialSid::Kernel))
    }

    /// Returns placeholder state for use when SELinux is not enabled.
    pub(super) fn for_selinux_disabled() -> Self {
        Self::for_sid(SecurityId::initial(InitialSid::Unlabeled))
    }

    /// Used to create initial state for tasks with a specified SID.
    pub(super) fn for_sid(sid: SecurityId) -> Self {
        Self {
            current_sid: sid,
            previous_sid: sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
        }
    }
}

/// Security state for a [`crate::vfs::FileObject`] instance. This currently just holds the SID
/// that the [`crate::task::Task`] that created the file object had.
#[derive(Debug)]
pub(super) struct FileObjectState {
    sid: SecurityId,
}

/// Security state for a [`crate::vfs::FileSystem`] instance. This holds the security fields
/// parsed from the mount options and the selected labeling scheme.
#[derive(Debug)]
enum FileSystemLabelState {
    Unlabeled {
        name: &'static FsStr,
        mount_options: FileSystemMountOptions,
        pending_entries: HashSet<WeakKey<DirEntry>>,
    },
    Labeled {
        label: FileSystemLabel,
    },
}

#[derive(Debug)]
pub(super) struct FileSystemState(Mutex<FileSystemLabelState>);

impl FileSystemState {
    fn new(name: &'static FsStr, mount_options: FileSystemMountOptions) -> Self {
        Self(Mutex::new(FileSystemLabelState::Unlabeled {
            name,
            mount_options,
            pending_entries: HashSet::new(),
        }))
    }

    /// Returns true if this `FileSystemState` is labeled with `fs_use_xattr` and thus supports
    /// xattr.
    fn supports_xattr(&self) -> bool {
        if let FileSystemLabelState::Labeled { label } = &mut *self.0.lock() {
            if let FileSystemLabelingScheme::FsUse { fs_use_type, .. } = label.scheme {
                return fs_use_type == FsUseType::Xattr;
            }
        }
        return false;
    }
}

/// Holds security state associated with a [`crate::vfs::FsNode`].
#[derive(Debug, Clone)]
pub struct FsNodeState {
    label: FsNodeLabel,
    class: FileClass,
}

impl Default for FsNodeState {
    fn default() -> Self {
        Self { label: FsNodeLabel::Uninitialized, class: FileClass::File }
    }
}

/// Describes the security label for a [`crate::vfs::FsNode`].
#[derive(Debug, Clone)]
pub(super) enum FsNodeLabel {
    Uninitialized,
    SecurityId { sid: SecurityId },
    FromTask { weak_task: WeakRef<Task> },
}

impl FsNodeLabel {
    fn is_initialized(&self) -> bool {
        !matches!(self, FsNodeLabel::Uninitialized)
    }
}

/// Holds the SID and class with which an `FsNode` is labeled, for use in permissions checks.
#[derive(Debug, PartialEq)]
struct FsNodeSidAndClass {
    sid: SecurityId,
    class: FileClass,
}

/// Sets the cached security id associated with `fs_node` to `sid`. Storing the security id will
/// cause the security id to *not* be recomputed by the SELinux LSM when determining the effective
/// security id of this [`FsNode`].
pub(super) fn set_cached_sid(fs_node: &FsNode, sid: SecurityId) {
    fs_node.security_state.lock().label = FsNodeLabel::SecurityId { sid };
}

/// Sets the Task associated with `fs_node` to `task`.
/// The effective security id of the [`FsNode`] will be that of the task, even if the security id
/// of the task changes.
pub(super) fn fs_node_set_label_with_task(fs_node: &FsNode, task: WeakRef<Task>) {
    fs_node.security_state.lock().label = FsNodeLabel::FromTask { weak_task: task };
}

/// Ensures that the `fs_node`'s security state has an appropriate security class set.
/// As per the NSA report description, the security class is chosen based on the `FileMode`, unless
/// a security class more specific than "file" has already been set on the node.
pub(super) fn fs_node_ensure_class(fs_node: &FsNode) -> Result<(), Errno> {
    if fs_node.security_state.lock().class == FileClass::File {
        let file_mode = fs_node.info().mode;
        fs_node.security_state.lock().class = file_class_from_file_mode(file_mode)?;
    }
    Ok(())
}

/// Returns the security id currently stored in `fs_node`, if any. This API should only be used
/// by code that is responsible for controlling the cached security id; e.g., to check its
/// current value before engaging logic that may compute a new value. Access control enforcement
/// code should use `fs_node_effective_sid_and_class()`, *not* this function.
fn get_cached_sid_and_class(fs_node: &FsNode) -> Option<FsNodeSidAndClass> {
    let state = fs_node.security_state.lock().clone();
    match state.label {
        FsNodeLabel::SecurityId { sid } => Some(sid),
        FsNodeLabel::FromTask { weak_task } => Some(
            weak_task
                .upgrade()
                .map(|t| t.security_state.lock().current_sid)
                // If `upgrade()` fails then the `Task` has exited, so return a placeholder SID.
                .unwrap_or_else(|| SecurityId::initial(InitialSid::Unlabeled)),
        ),
        FsNodeLabel::Uninitialized => None,
    }
    .map(|sid| FsNodeSidAndClass { sid, class: state.class })
}

#[cfg(test)]
/// Returns the SID with which the node is labeled, if any, for use by `FsNode` labeling tests.
pub(super) fn get_cached_sid(fs_node: &FsNode) -> Option<SecurityId> {
    get_cached_sid_and_class(fs_node).map(|sid_and_class| sid_and_class.sid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::spawn_kernel_and_run;
    use crate::vfs::XattrOp;
    use starnix_sync::FileOpsCore;
    use starnix_uapi::errno;
    use testing::{spawn_kernel_with_selinux_hooks_test_policy_and_run, TEST_FILE_NAME};

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";

    /// Clears the cached security id on `fs_node`.
    fn clear_cached_sid(fs_node: &FsNode) {
        fs_node.security_state.lock().label = FsNodeLabel::Uninitialized;
    }

    #[fuchsia::test]
    async fn fs_node_resolved_and_effective_sids_for_missing_xattr() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let dir_entry = &testing::create_test_file(locked, current_task).entry;
                let node = &dir_entry.node;

                // Remove the "security.selinux" label, if any.
                let _ = node.ops().remove_xattr(
                    &mut locked.cast_locked::<FileOpsCore>(),
                    node,
                    &current_task,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                );
                assert_eq!(
                    node.ops()
                        .get_xattr(
                            &mut locked.cast_locked::<FileOpsCore>(),
                            node,
                            &current_task,
                            XATTR_NAME_SELINUX.to_bytes().into(),
                            4096
                        )
                        .unwrap_err(),
                    errno!(ENODATA)
                );

                // Clear the cached SID and use `fs_node_init_with_dentry()` to re-resolve the label.
                clear_cached_sid(node);
                assert_eq!(None, get_cached_sid(node));
                fs_node_init_with_dentry(locked, &security_server, &current_task, dir_entry)
                    .expect("fs_node_init_with_dentry");

                // `fs_node_getsecurity()` should now fall-back to the policy's "file" Context.
                let default_file_context = security_server
                    .sid_to_security_context(SecurityId::initial(InitialSid::File))
                    .unwrap()
                    .into();
                let result = fs_node_getsecurity(
                    locked,
                    &security_server,
                    &current_task,
                    node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
                )
                .unwrap();
                assert_eq!(result, ValueOrSize::Value(default_file_context));
                assert!(get_cached_sid(node).is_some());
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_resolved_and_effective_sids_for_invalid_xattr() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let dir_entry = &testing::create_test_file(locked, current_task).entry;
                let node = &dir_entry.node;

                const INVALID_CONTEXT: &[u8] = b"invalid context!";

                // Set the security label to a value which is not a valid Security Context.
                node.ops()
                    .set_xattr(
                        &mut locked.cast_locked::<FileOpsCore>(),
                        node,
                        &current_task,
                        XATTR_NAME_SELINUX.to_bytes().into(),
                        INVALID_CONTEXT.into(),
                        XattrOp::Set,
                    )
                    .expect("setxattr");

                // Clear the cached SID and use `fs_node_init_with_dentry()` to re-resolve the label.
                clear_cached_sid(node);
                assert_eq!(None, get_cached_sid(node));
                fs_node_init_with_dentry(locked, &security_server, &current_task, dir_entry)
                    .expect("fs_node_init_with_dentry");

                // `fs_node_getsecurity()` should report the same invalid string as is in the xattr.
                let result = fs_node_getsecurity(
                    locked,
                    &security_server,
                    &current_task,
                    node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
                )
                .unwrap();
                assert_eq!(result, ValueOrSize::Value(INVALID_CONTEXT.into()));

                // The SID cached for the `node` should be "unlabeled".
                assert_eq!(Some(SecurityId::initial(InitialSid::Unlabeled)), get_cached_sid(node));

                // The effective SID of the node should be "unlabeled".
                assert_eq!(
                    SecurityId::initial(InitialSid::Unlabeled),
                    fs_node_effective_sid_and_class(node).sid
                );
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_effective_sid_valid_xattr_stored() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let dir_entry = &testing::create_test_file(locked, current_task).entry;
                let node = &dir_entry.node;

                // Store a valid Security Context in the attribute, then clear the cached label and
                // re-resolve it. The hooks test policy defines that "tmpfs" use "fs_use_xattr"
                // labeling, which should result in the (valid) label being read from the file, and
                // the corresponding SID cached.
                node.ops()
                    .set_xattr(
                        &mut locked.cast_locked::<FileOpsCore>(),
                        node,
                        &current_task,
                        XATTR_NAME_SELINUX.to_bytes().into(),
                        VALID_SECURITY_CONTEXT.into(),
                        XattrOp::Set,
                    )
                    .expect("setxattr");
                clear_cached_sid(node);
                assert_eq!(None, get_cached_sid(node));
                fs_node_init_with_dentry(locked, &security_server, &current_task, dir_entry)
                    .expect("fs_node_init_with_dentry");

                // `fs_node_getsecurity()` should report the same valid Security Context string as the xattr holds.
                let result = fs_node_getsecurity(
                    locked,
                    &security_server,
                    &current_task,
                    node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
                )
                .unwrap();
                assert_eq!(result, ValueOrSize::Value(VALID_SECURITY_CONTEXT.into()));

                // There should be a SID cached, and it should map to the valid Security Context.
                let cached_sid = get_cached_sid(node).unwrap();
                assert_eq!(
                    security_server.sid_to_security_context(cached_sid).unwrap(),
                    VALID_SECURITY_CONTEXT
                );

                // Requesting the effective SID should simply return the cached value.
                assert_eq!(cached_sid, fs_node_effective_sid_and_class(node).sid);
            },
        )
    }

    #[fuchsia::test]
    async fn setxattr_set_sid() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let expected_sid = security_server
                    .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
                    .expect("no SID for VALID_SECURITY_CONTEXT");
                let node = &testing::create_test_file(locked, current_task).entry.node;

                node.set_xattr(
                    &mut locked.cast_locked::<FileOpsCore>(),
                    current_task,
                    &current_task.fs().root().mount,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    VALID_SECURITY_CONTEXT.into(),
                    XattrOp::Set,
                )
                .expect("setxattr");

                // Verify that the SID now cached on the node corresponds to VALID_SECURITY_CONTEXT.
                assert_eq!(Some(expected_sid), get_cached_sid(node));
            },
        )
    }

    #[fuchsia::test]
    async fn get_fs_relative_path_root() {
        // Verify the full path for the root entry.
        spawn_kernel_and_run(|_, current_task| {
            let dir_entry = current_task.fs().root().entry;

            assert_eq!(BStr::new(b"/"), get_fs_relative_path(&dir_entry));
        });
    }

    #[fuchsia::test]
    async fn get_fs_relative_path_simple_file() {
        // Verify the full path for a file directly under the root: "/" + [`TEST_FILE_NAME`].
        spawn_kernel_and_run(|locked, current_task| {
            let dir_entry = &testing::create_test_file(locked, current_task).entry;

            let expected = format!("/{}", TEST_FILE_NAME);
            assert_eq!(BStr::new(&expected), get_fs_relative_path(&dir_entry));
        });
    }

    #[fuchsia::test]
    async fn get_fs_relative_path_nested_dir() {
        // Verify the full path for a nested directory: "/foo/bar".
        spawn_kernel_and_run(|locked, current_task| {
            let dir_entry = &testing::create_directory_with_parents(
                vec![BStr::new(b"foo"), BStr::new(b"bar")],
                locked,
                &current_task,
            )
            .entry;

            assert_eq!(BStr::new(b"/foo/bar"), get_fs_relative_path(&dir_entry));
        });
    }
}
