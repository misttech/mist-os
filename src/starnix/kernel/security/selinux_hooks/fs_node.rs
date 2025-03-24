// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use super::{
    check_permission, fs_node_effective_sid_and_class, fs_node_ensure_class,
    fs_node_set_label_with_task, has_fs_node_permissions, permissions_from_flags, scoped_fs_create,
    set_cached_sid, todo_check_permission, todo_has_fs_node_permissions, FileSystemLabelState,
    FsNodeLabel, FsNodeSecurityXattr, FsNodeSidAndClass, PermissionFlags, ScopedFsCreate,
};

use crate::task::CurrentTask;
use crate::vfs::{
    DirEntryHandle, FileSystem, FsNode, FsStr, FsString, PathBuilder, UnlinkKind, ValueOrSize,
    XattrOp,
};
use crate::TODO_DENY;
use bstr::BStr;

use selinux::policy::FsUseType;
use selinux::{
    CommonFilePermission, CommonFsNodePermission, DirPermission, FileClass, FileSystemLabel,
    FileSystemLabelingScheme, FileSystemPermission, FsNodeClass, InitialSid, ObjectClass,
    SecurityId, SecurityServer,
};
use starnix_logging::{log_debug, log_warn, track_stub};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked};
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{Errno, ENODATA};
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::{errno, error, XATTR_NAME_SELINUX};
use syncio::zxio_node_attr_has_t;

/// Maximum supported size for the extended attribute value used to store SELinux security
/// contexts in a filesystem node extended attributes.
const SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE: usize = 4096;

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

/// Verifies that the file system labelling is `FsUse`, and if so then it attempts to
/// apply the given context string to the node.
pub(in crate::security) fn fs_node_notify_security_context(
    security_server: &SecurityServer,
    fs_node: &FsNode,
    security_context: &FsStr,
) -> Result<(), Errno> {
    let fs = fs_node.fs();
    if !matches!(
        *fs.security_state.state.0.lock(),
        FileSystemLabelState::Labeled {
            label: FileSystemLabel { scheme: FileSystemLabelingScheme::FsUse { .. }, .. }
        }
    ) {
        return error!(ENOTSUP);
    }
    let sid = security_server
        .security_context_to_sid(security_context.into())
        .map_err(|_| errno!(EINVAL))?;
    set_cached_sid(fs_node, sid);
    Ok(())
}

/// Called by the VFS to initialize the security state for an `FsNode` that is being linked at
/// `dir_entry`. If `locked_or_no_xattr` is `None`, xattrs will not be read - this makes sense
/// for entries containing anonymous nodes, that will not have an associated filesystem entry.
pub(in crate::security) fn fs_node_init_with_dentry(
    locked_or_no_xattr: Option<&mut Locked<'_, FileOpsCore>>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    dir_entry: &DirEntryHandle,
) -> Result<(), Errno> {
    // Attempt to derive a specific security class for the `FsNode`, based on its file mode.
    // TODO: This ensures a correct class for nodes with a wrong `FileMode` at
    // creation, but should not really be required.
    fs_node_ensure_class(&dir_entry.node)?;

    // This hook is called every time an `FsNode` is linked to a `DirEntry`, so it is expected that
    // the `FsNode` may already have been labeled.
    let fs_node = &dir_entry.node;
    if fs_node.security_state.lock().label.is_initialized() {
        return Ok(());
    }

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
        FileSystemLabelingScheme::FsUse { fs_use_type, computed_def_sid: def_sid, .. } => {
            match (fs_use_type, locked_or_no_xattr) {
                (FsUseType::Xattr, Some(locked)) => {
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
                                // the `root_sid` label here, if available, or the filesystem label.
                                let root_sid = label.mount_sids.root_context;
                                let root_or_fs_sid = root_sid.unwrap_or(label.sid);
                                let root_context = security_server
                                    .sid_to_security_context(root_or_fs_sid)
                                    .unwrap();
                                fs_node.ops().set_xattr(
                                    &mut locked.cast_locked::<FileOpsCore>(),
                                    fs_node,
                                    current_task,
                                    XATTR_NAME_SELINUX.to_bytes().into(),
                                    root_context.as_slice().into(),
                                    XattrOp::Create,
                                )?;
                                Some(root_or_fs_sid)
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
                (FsUseType::Xattr, None) => {
                    log_warn!(
                        "Node {:?} in filesystem {} ({:?}-labeled) created in a context where the \
                        FileOpsCore lock cannot be taken.",
                        dir_entry,
                        fs.name(),
                        fs_use_type
                    );
                    SecurityId::initial(InitialSid::Unlabeled)
                }
                _ => {
                    // Ephemeral nodes are then labeled by applying SID computation between their
                    // SID of the task that created them, and their parent file node's label (or
                    // the filesystem sid if they don't have a parent node).
                    // TODO: https://fxbug.dev/381275592 - Use the SID from the creating task,
                    // rather than current_task!
                    let name = dir_entry.local_name();
                    return fs_node_init_on_create(
                        security_server,
                        current_task,
                        fs_node,
                        dir_entry.parent().as_ref().map(|x| &**x.node),
                        name.as_slice().into(),
                    )
                    .map(|_| ());
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
            let sub_path = if fs_node_class == FileClass::Link.into() {
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
        starnix_uapi::S_IFSOCK => Ok(FileClass::SockFile),
        0 => {
            track_stub!(TODO("https://fxbug.dev/378864191"), "File with zero IFMT?");
            Ok(FileClass::File)
        }
        _ => error!(EINVAL, format!("mode: {:?}", mode)),
    }
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
    fs: &FileSystem,
    parent: Option<&FsNode>,
    new_node_class: FsNodeClass,
    name: &FsStr,
) -> Result<Option<(SecurityId, FileSystemLabel)>, Errno> {
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
            // For root nodes, the specified root_sid takes precedence over all other rules.
            if parent.is_none() {
                let root_sid = label.mount_sids.root_context;
                if let Some(root_sid) = root_sid {
                    return Ok(Some((root_sid, label)));
                }
            }

            // If the task has an "fscreate" (for files) or "sockcreate" (for sockets) context set
            // then apply it to the new object rather than applying the policy-defined labeling.
            match new_node_class {
                FsNodeClass::Socket(_) => {
                    if let Some(sid) = current_task.security_state.lock().sockcreate_sid {
                        return Ok(Some((sid, label)));
                    }
                }
                FsNodeClass::File(_) => {
                    if let Some(fscreate_sid) = current_task.security_state.lock().fscreate_sid {
                        return Ok(Some((fscreate_sid, label)));
                    }
                }
            };

            // TODO: https://fxbug.dev/393086830 For root nodes created when mounting ephemeral
            // filesystems, this should be the kernel sid. However, for parent-less nodes (e.g.
            // pipes in pipefs) this should be the task sid.
            let current_task_sid = current_task.security_state.lock().current_sid;
            if fs_use_type == FsUseType::Task {
                // TODO: https://fxbug.dev/377912777 - verify that this is how fs_use_task is
                // supposed to work (https://selinuxproject.org/page/NB_ComputingSecurityContexts).
                Ok(Some((current_task_sid, label)))
            } else {
                // If we have a parent, use its sid. Otherwise, compute the sid of root nodes from
                // the sid of the filesystem.
                let parent_sid = if let Some(parent) = parent {
                    fs_node_effective_sid_and_class(parent).sid
                } else {
                    label.sid
                };
                let permission_check = security_server.as_permission_check();
                let sid = permission_check
                    .compute_new_fs_node_sid(
                        current_task_sid,
                        parent_sid,
                        new_node_class,
                        name.into(),
                    )
                    // TODO: https://fxbug.dev/377915452 - is EPERM right here? What does it mean
                    // for compute_new_fs_node_sid to have failed?
                    .map_err(|_| errno!(EPERM))?;
                Ok(Some((sid, label)))
            }
        }
    }
}

/// Called by file-system implementations when creating the `FsNode` for a new file.
pub(in crate::security) fn fs_node_init_on_create(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    new_node: &FsNode,
    parent: Option<&FsNode>,
    name: &FsStr,
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
    if let Some((sid, label)) = compute_new_fs_node_sid(
        security_server,
        current_task,
        &new_node.fs(),
        parent,
        new_node_class,
        name.into(),
    )? {
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

/// Called to label file nodes not linked in any filesystem's directory structure, e.g.
/// usereventfds, etc.
pub(in crate::security) fn fs_node_init_anon(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    new_node: &FsNode,
    node_type: &str,
) {
    let fs_node_class = FileClass::AnonFsNode.into();

    // TODO: https://fxbug.dev/ - Fold this into the `fs_node_init_with_dentry*()` logic?
    let maybe_label = security_server.has_policy().then(|| {
        let task_sid = current_task.security_state.lock().current_sid;
        let sid = security_server
            .as_permission_check()
            .compute_new_fs_node_sid(task_sid, task_sid, fs_node_class, node_type.into())
            .expect("Compute label for anon_inode");
        FsNodeLabel::SecurityId { sid }
    });

    let mut state = new_node.security_state.lock();
    state.class = fs_node_class;
    if let Some(label) = maybe_label {
        state.label = label;
    }
}

/// Helper used by filesystem node creation checks to validate that `current_task` has necessary
/// permissions to create a new node under the specified `parent`.
fn may_create(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    new_file_mode: FileMode, // Only used to determine the file class.
    name: &FsStr,
) -> Result<(), Errno> {
    let permission_check = security_server.as_permission_check();

    // Verify that the caller has permissions required to add new entries to the target
    // directory node.
    let current_sid = current_task.security_state.lock().current_sid;
    let parent_sid = fs_node_effective_sid_and_class(parent).sid;
    let fs = parent.fs();

    let audit_context = [current_task.into(), parent.into(), fs.as_ref().into()];
    check_permission(
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::Search,
        (&audit_context).into(),
    )?;
    check_permission(
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::AddName,
        (&audit_context).into(),
    )?;

    // Verify that the caller has permission to create new nodes of the desired type.
    let new_file_class = file_class_from_file_mode(new_file_mode)?.into();
    let new_file_sid = compute_new_fs_node_sid(
        security_server,
        current_task,
        &fs,
        Some(parent),
        new_file_class,
        name.into(),
    )?
    .map(|(sid, _)| sid)
    .unwrap_or_else(|| SecurityId::initial(InitialSid::File));

    let audit_context = [current_task.into(), fs.as_ref().into()];
    check_permission(
        &permission_check,
        current_sid,
        new_file_sid,
        CommonFsNodePermission::Create.for_class(new_file_class),
        (&audit_context).into(),
    )?;

    // Verify that the new node's label is permitted to be created in the target filesystem.
    let filesystem_sid = match &*fs.security_state.state.0.lock() {
        FileSystemLabelState::Labeled { label } => Ok(label.sid),
        FileSystemLabelState::Unlabeled { .. } => {
            track_stub!(
                TODO("https://fxbug.dev/367585803"),
                "may_create() should not be called until policy load has completed"
            );
            error!(EPERM)
        }
    }?;
    check_permission(
        &permission_check,
        new_file_sid,
        filesystem_sid,
        FileSystemPermission::Associate,
        (&audit_context).into(),
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
    let audit_context = current_task.into();

    let permission_check = security_server.as_permission_check();
    let current_sid = current_task.security_state.lock().current_sid;
    let parent_sid = fs_node_effective_sid_and_class(parent).sid;
    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(existing_node);

    let FsNodeClass::File(file_class) = file_class else {
        panic!("may_link called on non-file-like class")
    };
    check_permission(
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::Search,
        audit_context,
    )?;
    check_permission(
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::AddName,
        audit_context,
    )?;
    check_permission(
        &permission_check,
        current_sid,
        file_sid,
        CommonFilePermission::Link.for_class(file_class),
        audit_context,
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
    let audit_context = current_task.into();

    let permission_check = security_server.as_permission_check();
    let current_sid = current_task.security_state.lock().current_sid;
    let parent_sid = fs_node_effective_sid_and_class(parent).sid;

    check_permission(
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::Search,
        audit_context,
    )?;
    check_permission(
        &permission_check,
        current_sid,
        parent_sid,
        DirPermission::RemoveName,
        audit_context,
    )?;

    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(fs_node);
    let FsNodeClass::File(file_class) = file_class else {
        panic!("may_unlink_or_rmdir called on non-file-like class")
    };

    match operation {
        UnlinkKind::NonDirectory => check_permission(
            &permission_check,
            current_sid,
            file_sid,
            CommonFilePermission::Unlink.for_class(file_class),
            audit_context,
        ),
        UnlinkKind::Directory => check_permission(
            &permission_check,
            current_sid,
            file_sid,
            DirPermission::RemoveDir,
            audit_context,
        ),
    }
}

/// Validate that `current_task` has permission to create a regular file in the `parent` directory,
/// with the specified file `mode`.
pub(in crate::security) fn check_fs_node_create_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
    name: &FsStr,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, mode, name)
}

/// Validate that `current_task` has permission to create a symlink to `old_path` in the `parent`
/// directory.
pub(in crate::security) fn check_fs_node_symlink_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    name: &FsStr,
    _old_path: &FsStr,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, FileMode::IFLNK, name)
}

/// Validate that `current_task` has permission to create a new directory in the `parent` directory,
/// with the specified file `mode`.
pub(in crate::security) fn check_fs_node_mkdir_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
    name: &FsStr,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, mode, name)
}

/// Validate that `current_task` has permission to create a new special file, socket or pipe, in the
/// `parent` directory, and with the specified file `mode` and `device_id`.
pub(in crate::security) fn check_fs_node_mknod_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
    name: &FsStr,
    _device_id: DeviceType,
) -> Result<(), Errno> {
    may_create(security_server, current_task, parent, mode, name)
}

/// Validate that `current_task` has the permission to create a new hard link to a file.
pub(in crate::security) fn check_fs_node_link_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    target_directory: &FsNode,
    existing_node: &FsNode,
) -> Result<(), Errno> {
    may_link(security_server, current_task, target_directory, existing_node)
}

/// Validate that `current_task` has the permission to remove a hard link to a file.
pub(in crate::security) fn check_fs_node_unlink_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    assert!(!child.is_dir());

    may_unlink_or_rmdir(security_server, current_task, parent, child, UnlinkKind::NonDirectory)
}

/// Validate that `current_task` has the permission to remove a directory.
pub(in crate::security) fn check_fs_node_rmdir_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    assert!(child.is_dir());

    may_unlink_or_rmdir(security_server, current_task, parent, child, UnlinkKind::Directory)
}

/// Validates that `current_task` has the permissions to move `moving_node`.
pub(in crate::security) fn check_fs_node_rename_access(
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

    check_permission(
        &permission_check,
        current_sid,
        old_parent_sid,
        DirPermission::Search,
        current_task.into(),
    )?;
    check_permission(
        &permission_check,
        current_sid,
        old_parent_sid,
        DirPermission::RemoveName,
        current_task.into(),
    )?;

    let FsNodeSidAndClass { sid: file_sid, class: file_class } =
        fs_node_effective_sid_and_class(moving_node);
    let FsNodeClass::File(file_class) = file_class else {
        panic!("fs_node_rename called on non-file-like class")
    };

    check_permission(
        &permission_check,
        current_sid,
        file_sid,
        CommonFilePermission::Rename.for_class(file_class),
        current_task.into(),
    )?;

    let new_parent_sid = fs_node_effective_sid_and_class(new_parent).sid;
    check_permission(
        &permission_check,
        current_sid,
        new_parent_sid,
        DirPermission::AddName,
        current_task.into(),
    )?;

    // If a file already exists with the new name, then verify that the existing file can be
    // removed.
    if let Some(replaced_node) = replaced_node {
        let replaced_node_class = replaced_node.security_state.lock().class;
        may_unlink_or_rmdir(
            security_server,
            current_task,
            new_parent,
            replaced_node,
            if replaced_node_class == FileClass::Dir.into() {
                UnlinkKind::Directory
            } else {
                UnlinkKind::NonDirectory
            },
        )?;
    }

    if !std::ptr::eq(old_parent, new_parent) {
        // If the parent nodes are the same directory, we have already verified the search
        // permission during the `old_parent_sid` verification.
        check_permission(
            &permission_check,
            current_sid,
            new_parent_sid,
            DirPermission::Search,
            current_task.into(),
        )?;

        // If the file is a directory and its parent directory is being changed by the rename,
        // we additionally check for the reparent permission. Note that the `reparent` permission is
        // only defined for directories.
        if file_class == FileClass::Dir.into() {
            check_permission(
                &permission_check,
                current_sid,
                file_sid,
                DirPermission::Reparent,
                current_task.into(),
            )?;
        }
    }

    Ok(())
}

/// Validates that `current_task` has the permissions to read the symbolic link `fs_node`.
pub(in crate::security) fn check_fs_node_read_link_access(
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
        CommonFsNodePermission::Read.for_class(file_class),
        current_task.into(),
    )
}

/// Validates that the `current_task` has the permissions to access `fs_node`.
pub(in crate::security) fn fs_node_permission(
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
        current_task.into(),
    )
}

pub(in crate::security) fn check_fs_node_getattr_access(
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
        CommonFsNodePermission::GetAttr.for_class(file_class),
        current_task.into(),
    )
}

/// Checks whether `current_task` can set attributes on `node`.
pub(in crate::security) fn check_fs_node_setattr_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    attributes: &zxio_node_attr_has_t,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeSidAndClass { class: file_class, .. } = fs_node_effective_sid_and_class(fs_node);

    let permissions = if attributes.mode
        || attributes.uid
        || attributes.gid
        || attributes.access_time
        || attributes.modification_time
        || attributes.change_time
        || attributes.casefold
    {
        [CommonFsNodePermission::SetAttr.for_class(file_class)]
    } else {
        [CommonFsNodePermission::Write.for_class(file_class)]
    };

    has_fs_node_permissions(
        &security_server.as_permission_check(),
        current_sid,
        fs_node,
        &permissions,
        current_task.into(),
    )
}

pub(in crate::security) fn check_fs_node_setxattr_access(
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
        CommonFsNodePermission::SetAttr.for_class(file_class),
        current_task.into(),
    )
}

pub(in crate::security) fn check_fs_node_getxattr_access(
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
        CommonFsNodePermission::GetAttr.for_class(file_class),
        current_task.into(),
    )
}

pub(in crate::security) fn check_fs_node_listxattr_access(
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
        CommonFsNodePermission::GetAttr.for_class(file_class),
        current_task.into(),
    )
}

pub(in crate::security) fn check_fs_node_removexattr_access(
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
        CommonFsNodePermission::SetAttr.for_class(file_class),
        current_task.into(),
    )
}

pub(in crate::security) fn fs_node_copy_up<'a>(
    current_task: &'a CurrentTask,
    fs_node: &FsNode,
) -> ScopedFsCreate<'a> {
    let file_sid = fs_node_effective_sid_and_class(fs_node).sid;
    scoped_fs_create(current_task, file_sid)
}

/// If `fs_node` is in a filesystem without xattr support, returns the xattr name for the security
/// label (i.e. "security.selinux"). Otherwise returns None.
pub(in crate::security) fn fs_node_listsecurity(fs_node: &FsNode) -> Option<FsString> {
    if fs_node.fs().security_state.state.supports_xattr() {
        None
    } else {
        Some(XATTR_NAME_SELINUX.to_bytes().into())
    }
}

/// Returns the Security Context corresponding to the SID with which `FsNode`
/// is labelled, otherwise delegates to the node's [`crate::vfs::FsNodeOps`].
pub(in crate::security) fn fs_node_getsecurity<L>(
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
pub(in crate::security) fn fs_node_setsecurity<L>(
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
        let audit_context = current_task.into();

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
                CommonFsNodePermission::RelabelFrom.for_class(file_class),
                audit_context,
            )?;
        } else {
            check_permission(
                &permission_check,
                task_sid,
                old_sid,
                CommonFsNodePermission::RelabelFrom.for_class(file_class),
                audit_context,
            )?;
        }
        check_permission(
            &permission_check,
            task_sid,
            new_sid,
            CommonFsNodePermission::RelabelTo.for_class(file_class),
            audit_context,
        )?;
        check_permission(
            &permission_check,
            new_sid,
            fs_label.sid,
            FileSystemPermission::Associate,
            audit_context,
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

#[cfg(test)]
mod tests {
    use super::super::testing::{
        self, spawn_kernel_with_selinux_hooks_test_policy_and_run, TEST_FILE_NAME,
    };
    use super::super::{get_cached_sid, scoped_fs_create};
    use super::*;

    use crate::testing::spawn_kernel_and_run;
    use crate::vfs::XattrOp;
    use starnix_sync::FileOpsCore;
    use starnix_uapi::errno;

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
                fs_node_init_with_dentry(
                    Some(&mut locked.cast_locked()),
                    &security_server,
                    &current_task,
                    dir_entry,
                )
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
                fs_node_init_with_dentry(
                    Some(&mut locked.cast_locked()),
                    &security_server,
                    &current_task,
                    dir_entry,
                )
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
                fs_node_init_with_dentry(
                    Some(&mut locked.cast_locked()),
                    &security_server,
                    &current_task,
                    dir_entry,
                )
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

    #[fuchsia::test]
    async fn create_file_with_fscreate_sid() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let sid =
                    security_server.security_context_to_sid(VALID_SECURITY_CONTEXT.into()).unwrap();
                let scoped_fs_create = scoped_fs_create(current_task, sid);
                let dir_entry = &testing::create_test_file(locked, current_task).entry;
                std::mem::drop(scoped_fs_create);

                let effective_sid = fs_node_effective_sid_and_class(&dir_entry.node).sid;
                assert_eq!(sid, effective_sid);
            },
        );
    }
}
