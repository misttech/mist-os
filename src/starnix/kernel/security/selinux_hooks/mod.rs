// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

mod audit;
pub(super) mod bpf;
pub(super) mod file;
pub(super) mod fs_node;
pub(super) mod selinuxfs;
pub(super) mod socket;
pub(super) mod superblock;
pub(super) mod task;
pub(super) mod testing;

use super::{FsNodeSecurityXattr, PermissionFlags};
use crate::task::{CurrentTask, Task};
use crate::vfs::{DirEntry, FileHandle, FileObject, FileSystem, FsNode, FsStr, OutputBuffer};
use crate::TODO_DENY;
use audit::{audit_decision, audit_todo_decision, Auditable};
use selinux::permission_check::PermissionCheck;
use selinux::policy::FsUseType;
use selinux::{
    ClassPermission, CommonFilePermission, CommonFsNodePermission, DirPermission, FdPermission,
    FileClass, FileSystemLabel, FileSystemLabelingScheme, FileSystemMountOptions, FsNodeClass,
    InitialSid, Permission, ProcessPermission, SecurityId, SecurityServer,
};
use starnix_logging::{track_stub, BugRef};
use starnix_sync::Mutex;
use starnix_types::ownership::WeakRef;
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

/// Returns the set of `Permissions` on `class`, corresponding to the specified `flags`.
fn permissions_from_flags(flags: PermissionFlags, class: FsNodeClass) -> Vec<Permission> {
    let mut result = Vec::new();
    if flags.contains(PermissionFlags::READ) {
        result.push(CommonFsNodePermission::Read.for_class(class));
    }
    // SELinux uses the `APPEND` bit to distinguish which of the "append" or the more general
    // "write" permission to check for.
    if flags.contains(PermissionFlags::APPEND) {
        result.push(CommonFsNodePermission::Append.for_class(class));
    } else if flags.contains(PermissionFlags::WRITE) {
        result.push(CommonFsNodePermission::Write.for_class(class));
    }

    if let FsNodeClass::File(class) = class {
        if flags.contains(PermissionFlags::EXEC) {
            if class == FileClass::Dir {
                result.push(DirPermission::Search.into());
            } else {
                result.push(CommonFilePermission::Execute.for_class(class));
            }
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
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Validate that the `subject` has the "fd { use }" permission to the `file`.
    // If the file and task security domains are identical then `fd { use }` is implicitly granted.
    let file_sid = file.security_state.state.sid;
    if subject_sid == SecurityId::initial(InitialSid::Kernel)
        || file_sid == SecurityId::initial(InitialSid::Kernel)
    {
        track_stub!(
            TODO("https://fxbug.dev/385121365"),
            "Enforce fs:use where source or target is the kernel SID?"
        );
    } else if subject_sid != file_sid {
        let node = file.node().as_ref().as_ref();
        let audit_context = [audit_context, file.into(), node.into()];
        check_permission(
            permission_check,
            subject_sid,
            file_sid,
            FdPermission::Use,
            (&audit_context).into(),
        )?;
    }

    // Validate that the `subject` has the desired `permissions`, if any, to the underlying node.
    if !permissions.is_empty() {
        let audit_context = [audit_context, file.into()];
        has_fs_node_permissions(
            permission_check,
            subject_sid,
            file.node(),
            permissions,
            (&audit_context).into(),
        )?;
    }

    Ok(())
}

/// Checks that `current_task` has permission to "use" the specified `file`, and the specified
/// `permissions` to the underlying [`crate::vfs::FsNode`], with "todo_deny" on denial.
fn todo_has_file_permissions(
    bug: BugRef,
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    file: &FileObject,
    permissions: &[Permission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Validate that the `subject` has the "fd { use }" permission to the `file`.
    // If the file and task security domains are identical then `fd { use }` is implicitly granted.
    let file_sid = file.security_state.state.sid;
    if subject_sid == SecurityId::initial(InitialSid::Kernel)
        || file_sid == SecurityId::initial(InitialSid::Kernel)
    {
        track_stub!(
            TODO("https://fxbug.dev/385121365"),
            "Enforce fs:use where source or target is the kernel SID?"
        );
    } else if subject_sid != file_sid {
        let node = file.node().as_ref().as_ref();
        let audit_context = [audit_context, file.into(), node.into()];
        todo_check_permission(
            bug.clone(),
            permission_check,
            subject_sid,
            file_sid,
            FdPermission::Use,
            (&audit_context).into(),
        )?;
    }

    // Validate that the `subject` has the desired `permissions`, if any, to the underlying node.
    if !permissions.is_empty() {
        let audit_context = [audit_context, file.into()];
        todo_has_fs_node_permissions(
            bug.clone(),
            permission_check,
            subject_sid,
            file.node(),
            permissions,
            (&audit_context).into(),
        )?;
    }

    Ok(())
}

/// Checks that `current_task` has the specified `permissions` to the `node`.
fn has_fs_node_permissions(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    node: &FsNode,
    permissions: &[Permission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    let target = fs_node_effective_sid_and_class(node);

    // TODO: https://fxbug.dev/364568517 - Some sockets are incorrectly classed "sock_file".
    if target.class == FileClass::SockFile.into() {
        return Ok(());
    }

    let audit_context = [audit_context, node.into()];
    for permission in permissions {
        check_permission(
            permission_check,
            subject_sid,
            target.sid,
            permission.clone(),
            (&audit_context).into(),
        )?;
    }

    Ok(())
}

/// Checks that `current_task` has `permissions` to `node`, with "todo_deny" on denial.
fn todo_has_fs_node_permissions(
    bug: BugRef,
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    node: &FsNode,
    permissions: &[Permission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    let target = fs_node_effective_sid_and_class(node);

    // TODO: https://fxbug.dev/364568517 - Some sockets are incorrectly classed "sock_file".
    if target.class == FileClass::SockFile.into() {
        return Ok(());
    }

    for permission in permissions {
        todo_check_permission(
            bug.clone(),
            permission_check,
            subject_sid,
            target.sid,
            permission.clone(),
            audit_context,
        )?;
    }

    Ok(())
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

#[macro_export]
macro_rules! TODO_DENY {
    ($bug_url:literal, $message:literal) => {{
        use starnix_logging::bug_ref;
        bug_ref!($bug_url)
    }};
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

    FsNodeSidAndClass {
        sid: SecurityId::initial(InitialSid::Unlabeled),
        class: FileClass::File.into(),
    }
}

/// Perform the specified check as would `check_permission()`, but report denials as "todo_deny" in
/// the audit output, without actually denying access.
fn todo_check_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    bug: BugRef,
    permission_check: &PermissionCheck<'_>,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    if permission.class() == FileClass::AnonFsNode.into() {
        // TODO: https://fxbug.dev/404773987 - Skip all `anon_inode` checks, since most should be
        // treated as "private" to the kernel, and therefore un-checked.
        return Ok(());
    }

    let result = permission_check.has_permission(source_sid, target_sid, permission.clone());

    if result.audit {
        audit_todo_decision(
            bug,
            permission_check,
            result,
            source_sid,
            target_sid,
            permission.into(),
            audit_context,
        );
    }

    Ok(())
}

/// Checks whether `source_sid` is allowed the specified `permission` on `target_sid`.
fn check_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    permission_check: &PermissionCheck<'_>,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    if permission.class() == FileClass::AnonFsNode.into() {
        // TODO: https://fxbug.dev/404773987 - Skip all `anon_inode` checks, since most should be
        // treated as "private" to the kernel, and therefore un-checked.
        return Ok(());
    } else if permission.class() == FileClass::SockFile.into() {
        return todo_check_permission(
            TODO_DENY!(
                "https://fxbug.dev/364568517",
                "Re-enable enforcement of FsNode permission checks on sockets"
            ),
            permission_check,
            source_sid,
            target_sid,
            permission,
            audit_context,
        );
    }

    let result = permission_check.has_permission(source_sid, target_sid, permission.clone());

    if result.audit {
        audit_decision(
            permission_check,
            result.clone(),
            source_sid,
            target_sid,
            permission.into(),
            audit_context,
        );
    };

    result.permit.then_some(Ok(())).unwrap_or_else(|| error!(EACCES))
}

/// Checks that `subject_sid` has the specified process `permission` on `self`.
fn check_self_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    permission: P,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    check_permission(permission_check, subject_sid, subject_sid, permission, audit_context)
}

/// Returns the security state structure for the kernel.
pub(super) fn kernel_init_security(exceptions_config: String) -> KernelState {
    KernelState {
        server: SecurityServer::new_with_exceptions(exceptions_config),
        pending_file_systems: Mutex::default(),
        selinuxfs_null: OnceLock::default(),
    }
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
            mount_options: mount_options,
            pending_entries: HashSet::new(),
        }))
    }

    /// Returns true if this `FileSystemState` is labeled with `fs_use_xattr` and thus supports
    /// xattr.
    pub fn supports_xattr(&self) -> bool {
        if let FileSystemLabelState::Labeled { label } = &mut *self.0.lock() {
            if let FileSystemLabelingScheme::FsUse { fs_use_type, .. } = label.scheme {
                return fs_use_type == FsUseType::Xattr;
            }
        }
        return false;
    }

    /// Returns true if the security state of `self` is equivalent to the one derived from
    /// `other_mount_options` for the filesystem named `fs_name`.
    fn equivalent_to_options(
        &self,
        security_server: &SecurityServer,
        other_mount_options: &FileSystemMountOptions,
        fs_name: &'static FsStr,
    ) -> bool {
        let guard = self.0.lock();
        match &*guard {
            FileSystemLabelState::Unlabeled { name: _, mount_options, pending_entries: _ } => {
                return *other_mount_options == *mount_options
            }
            FileSystemLabelState::Labeled { label } => {
                return superblock::label_from_mount_options_and_name(
                    security_server,
                    other_mount_options,
                    fs_name.into(),
                ) == *label
            }
        }
    }

    /// Writes the Security mount options for the `FileSystemState` into `buf`.
    /// This is used where the mount options need to be stringified to expose to userspace, as
    /// is the case for `/proc/mounts`
    ///
    /// This function always writes a leading comma because it is only ever called to append to a
    /// non-empty list of comma-separated values.
    ///
    /// Example output:
    ///     ",context=foo,root_context=bar"
    /// TODO(357876133): Write "seclabel" when relevant.
    fn write_mount_options(
        &self,
        security_server: &SecurityServer,
        buf: &mut impl OutputBuffer,
    ) -> Result<(), Errno> {
        let security_state = self.0.lock();

        let mut write_options = |mount_options: &FileSystemMountOptions| -> Result<(), Errno> {
            let mut write_option = |prefix: &[u8], option: &Option<Vec<u8>>| -> Result<(), Errno> {
                if let Some(value) = option {
                    let _ = buf.write_all(prefix)?;
                    let _ = buf.write_all(value)?;
                }
                Ok(())
            };
            write_option(b",context=", &mount_options.context)?;
            write_option(b",fscontext=", &mount_options.fs_context)?;
            write_option(b",defcontext=", &mount_options.def_context)?;
            write_option(b",rootcontext=", &mount_options.root_context)
        };

        match &*security_state {
            FileSystemLabelState::Unlabeled { mount_options, name: _, pending_entries: _ } => {
                write_options(mount_options)
            }
            FileSystemLabelState::Labeled { label } => {
                let get_option = |sid_opt: Option<_>| -> Option<Vec<u8>> {
                    if let Some(sid) = sid_opt {
                        return security_server.sid_to_security_context(sid);
                    }
                    Option::None
                };
                let mount_options = FileSystemMountOptions {
                    context: get_option(label.mount_sids.context),
                    fs_context: get_option(label.mount_sids.fs_context),
                    def_context: get_option(label.mount_sids.def_context),
                    root_context: get_option(label.mount_sids.root_context),
                };
                write_options(&mount_options)
            }
        }
    }
}

/// Holds security state associated with a [`crate::vfs::FsNode`].
#[derive(Debug, Clone)]
pub struct FsNodeState {
    label: FsNodeLabel,
    class: FsNodeClass,
}

impl Default for FsNodeState {
    fn default() -> Self {
        Self { label: FsNodeLabel::Uninitialized, class: FileClass::File.into() }
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
    class: FsNodeClass,
}

/// Security state for a bpf [`ebpf_api::maps::Map`] instance. This currently just holds the
/// SID that the [`crate::task::Task`] that created the file object had.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BpfMapState {
    sid: SecurityId,
}

/// Security state for a bpf [`starnix_core::bpf::program::Program`]. instance. This currently just
/// holds the SID that the [`crate::task::Task`] that created the file object had.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BpfProgState {
    sid: SecurityId,
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
fn fs_node_set_label_with_task(fs_node: &FsNode, task: WeakRef<Task>) {
    fs_node.security_state.lock().label = FsNodeLabel::FromTask { weak_task: task };
}

/// Ensures that the `fs_node`'s security state has an appropriate security class set.
/// As per the NSA report description, the security class is chosen based on the `FileMode`, unless
/// a security class more specific than "file" has already been set on the node.
fn fs_node_ensure_class(fs_node: &FsNode) -> Result<(), Errno> {
    if fs_node.security_state.lock().class == FileClass::File.into() {
        let file_mode = fs_node.info().mode;
        fs_node.security_state.lock().class = file_class_from_file_mode(file_mode)?.into();
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

/// Encapsulates a temporary override of the SID with which file nodes will be created.
/// Restores the previously used file creation SID when dropped.
pub struct ScopedFsCreate<'a> {
    task: &'a CurrentTask,
    old_fscreate_sid: Option<SecurityId>,
}

pub(super) fn scoped_fs_create<'a>(
    task: &'a CurrentTask,
    fscreate_sid: SecurityId,
) -> ScopedFsCreate<'a> {
    let mut task_attrs = task.security_state.lock();
    let old_fscreate_sid = std::mem::replace(&mut task_attrs.fscreate_sid, Some(fscreate_sid));
    ScopedFsCreate { task, old_fscreate_sid }
}

impl Drop for ScopedFsCreate<'_> {
    fn drop(&mut self) {
        let mut task_attrs = self.task.security_state.lock();
        task_attrs.fscreate_sid = self.old_fscreate_sid;
    }
}

#[cfg(test)]
/// Returns the SID with which the node is labeled, if any, for use by `FsNode` labeling tests.
pub(super) fn get_cached_sid(fs_node: &FsNode) -> Option<SecurityId> {
    get_cached_sid_and_class(fs_node).map(|sid_and_class| sid_and_class.sid)
}
