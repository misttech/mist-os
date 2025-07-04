// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

mod audit;
pub(super) mod binder;
pub(super) mod bpf;
pub(super) mod file;
pub(super) mod fs_node;
pub(super) mod selinuxfs;
pub(super) mod socket;
pub(super) mod superblock;
pub(super) mod task;
pub(super) mod testing;

use super::{FsNodeSecurityXattr, PermissionFlags};
use crate::task::{CurrentTask, Kernel, Task};
use crate::vfs::{Anon, DirEntry, FileHandle, FileObject, FileSystem, FsNode, OutputBuffer};
use audit::{audit_decision, audit_todo_decision, Auditable};
use selinux::permission_check::PermissionCheck;
use selinux::policy::FsUseType;
use selinux::{
    ClassPermission, CommonFilePermission, CommonFsNodePermission, DirPermission, FdPermission,
    FileClass, FileSystemLabel, FileSystemLabelingScheme, FileSystemMountOptions, FsNodeClass,
    InitialSid, KernelPermission, ProcessPermission, SecurityId, SecurityServer,
};
use starnix_logging::{track_stub, BugRef};
use starnix_sync::{Mutex, MutexGuard};
use starnix_types::ownership::WeakRef;
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Returns the set of `Permissions` on `class`, corresponding to the specified `flags`.
fn permissions_from_flags(flags: PermissionFlags, class: FsNodeClass) -> Vec<KernelPermission> {
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
    kernel: &Kernel,
    subject_sid: SecurityId,
    file: &FileObject,
    permissions: &[KernelPermission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Validate that the `subject` has the "fd { use }" permission to the `file`.
    // If the file and task security domains are identical then `fd { use }` is implicitly granted.
    let file_sid = file.security_state.state.sid;
    if subject_sid != file_sid {
        let node = file.node().as_ref().as_ref();
        let audit_context = [audit_context, file.into(), node.into()];
        check_permission(
            permission_check,
            kernel,
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
            kernel,
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
    kernel: &Kernel,
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    file: &FileObject,
    permissions: &[KernelPermission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Validate that the `subject` has the "fd { use }" permission to the `file`.
    // If the file and task security domains are identical then `fd { use }` is implicitly granted.
    let file_sid = file.security_state.state.sid;
    if subject_sid != file_sid {
        let node = file.node().as_ref().as_ref();
        let audit_context = [audit_context, file.into(), node.into()];
        todo_check_permission(
            bug.clone(),
            kernel,
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
            kernel,
            permission_check,
            subject_sid,
            file.node(),
            permissions,
            (&audit_context).into(),
        )?;
    }

    Ok(())
}

fn has_file_ioctl_permission(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    file: &FileObject,
    ioctl: u16,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Validate that the `subject` has the "fd { use }" permission to the `file`.
    has_file_permissions(permission_check, kernel, subject_sid, file, &[], audit_context)?;

    // Validate that the `subject` has the `ioctl` permission on the underlying node,
    // as well as the specified ioctl extended permission.
    let fs_node = file.node().as_ref().as_ref();
    if Anon::is_private(fs_node) {
        return Ok(());
    }
    let FsNodeSidAndClass { sid: target_sid, class: target_class } =
        fs_node_effective_sid_and_class(fs_node);

    let audit_context =
        &[audit_context, file.into(), fs_node.into(), Auditable::IoctlCommand(ioctl)];

    // Check the `ioctl` permission and extended permission on the underlying node.
    check_ioctl_permission(
        permission_check,
        kernel,
        subject_sid,
        target_sid,
        target_class,
        ioctl,
        audit_context.into(),
    )
}

fn check_ioctl_permission(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    target_sid: SecurityId,
    target_class: FsNodeClass,
    ioctl: u16,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    let ioctl_permission = CommonFsNodePermission::Ioctl.for_class(target_class);
    let result = permission_check.has_ioctl_permission(
        subject_sid,
        target_sid,
        ioctl_permission.clone(),
        ioctl,
    );

    if result.audit {
        if !result.permit {
            kernel
                .security_state
                .state
                .as_ref()
                .unwrap()
                .access_denial_count
                .fetch_add(1, Ordering::Release);
        }

        audit_decision(
            permission_check,
            result.clone(),
            subject_sid,
            target_sid,
            ioctl_permission.into(),
            audit_context.into(),
        );
    }

    result.permit.then_some(Ok(())).unwrap_or_else(|| error!(EACCES))
}

/// Checks that `current_task` has the specified `permissions` to the `node`.
fn has_fs_node_permissions(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    fs_node: &FsNode,
    permissions: &[KernelPermission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    if Anon::is_private(fs_node) {
        return Ok(());
    }

    let target = fs_node_effective_sid_and_class(fs_node);

    let fs = fs_node.fs();
    let audit_context = [audit_context, fs_node.into(), fs.as_ref().into()];
    for permission in permissions {
        check_permission(
            permission_check,
            kernel,
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
    kernel: &Kernel,
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    fs_node: &FsNode,
    permissions: &[KernelPermission],
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    if Anon::is_private(fs_node) {
        return Ok(());
    }

    let target = fs_node_effective_sid_and_class(fs_node);

    let fs = fs_node.fs();
    let audit_context = [audit_context, fs_node.into(), fs.as_ref().into()];
    for permission in permissions {
        todo_check_permission(
            bug.clone(),
            kernel,
            permission_check,
            subject_sid,
            target.sid,
            permission.clone(),
            (&audit_context).into(),
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
    #[cfg(is_debug)]
    panic!(
        "Unlabeled FsNode@{} of class {:?} in {}",
        fs_node.info().ino,
        file_class_from_file_mode(fs_node.info().mode),
        fs_node.fs().name()
    );
    #[cfg(not(is_debug))]
    track_stub!(TODO("https://fxbug.dev/381210513"), "SID requested for unlabeled FsNode");

    FsNodeSidAndClass { sid: InitialSid::Unlabeled.into(), class: FileClass::File.into() }
}

/// Perform the specified check as would `check_permission()`, but report denials as "todo_deny" in
/// the audit output, without actually denying access.
fn todo_check_permission<P: ClassPermission + Into<KernelPermission> + Clone + 'static>(
    bug: BugRef,
    kernel: &Kernel,
    permission_check: &PermissionCheck<'_>,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    if kernel.features.selinux_test_suite {
        check_permission(
            permission_check,
            kernel,
            source_sid,
            target_sid,
            permission,
            audit_context,
        )
    } else {
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
}

/// Checks whether `source_sid` is allowed the specified `permission` on `target_sid`.
fn check_permission<P: ClassPermission + Into<KernelPermission> + Clone + 'static>(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    let result = permission_check.has_permission(source_sid, target_sid, permission.clone());

    if result.audit {
        if !result.permit {
            kernel
                .security_state
                .state
                .as_ref()
                .unwrap()
                .access_denial_count
                .fetch_add(1, Ordering::Release);
        }

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
fn check_self_permission<P: ClassPermission + Into<KernelPermission> + Clone + 'static>(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    permission: P,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    check_permission(permission_check, kernel, subject_sid, subject_sid, permission, audit_context)
}

/// Returns the security state structure for the kernel.
pub(super) fn kernel_init_security(options: String, exceptions: Vec<String>) -> KernelState {
    KernelState {
        server: SecurityServer::new(options, exceptions),
        pending_file_systems: Mutex::default(),
        selinuxfs_null: OnceLock::default(),
        access_denial_count: AtomicU64::new(0u64),
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

    /// Counts the number of times that an AVC denial is audit-logged.
    pub(super) access_denial_count: AtomicU64,
}

impl KernelState {
    pub(super) fn access_denial_count(&self) -> u64 {
        self.access_denial_count.load(Ordering::Acquire)
    }
}

/// The SELinux security structure for `ThreadGroup`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TaskAttrs {
    /// Current SID for the task.
    pub current_sid: SecurityId,

    /// Effective SID for the task. This is usually equal to |current_sid|, but this may be changed
    /// internally for the current task. This should only be accessed for the running task.
    pub effective_sid: SecurityId,

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
        Self::for_sid(InitialSid::Kernel.into())
    }

    /// Returns placeholder state for use when SELinux is not enabled.
    pub(super) fn for_selinux_disabled() -> Self {
        Self::for_sid(InitialSid::Unlabeled.into())
    }

    /// Used to create initial state for tasks with a specified SID.
    pub(super) fn for_sid(sid: SecurityId) -> Self {
        Self {
            current_sid: sid,
            effective_sid: sid,
            previous_sid: sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
        }
    }

    /// Sets the current SID and resets the effective SID to match.
    pub(super) fn set_current_sid(&mut self, sid: SecurityId) {
        self.current_sid = sid;
        self.effective_sid = sid
    }
}

/// Returns the effective SID of a task, i.e. the one that should be used for all checks where the
/// task is the active entity.
pub(in crate::security) fn task_effective_sid(current_task: &CurrentTask) -> SecurityId {
    current_task.security_state.lock().effective_sid
}

/// Returns the SID of a task. Panics if the current and effective SID are not consistent. This
/// should be used for operations that do not make sense under an assumed identity.
pub(in crate::security) fn task_consistent_attrs(
    current_task: &CurrentTask,
) -> MutexGuard<'_, TaskAttrs> {
    let task_attrs = current_task.security_state.lock();
    assert_eq!(task_attrs.effective_sid, task_attrs.current_sid);
    task_attrs
}

/// Structure defining a patch for task attributes that can be temporarily applied. The current
/// SID cannot be modified, since it is observable by other tasks performing access checks.
#[derive(Clone, Debug, PartialEq)]
struct TaskAttrsOverride {
    effective_sid: Option<SecurityId>,
    exec_sid: Option<Option<SecurityId>>,
    fscreate_sid: Option<Option<SecurityId>>,
    keycreate_sid: Option<Option<SecurityId>>,
    sockcreate_sid: Option<Option<SecurityId>>,
}

impl Default for TaskAttrsOverride {
    fn default() -> TaskAttrsOverride {
        TaskAttrsOverride {
            effective_sid: None,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
        }
    }
}

impl TaskAttrsOverride {
    /// Creates a default patch.
    pub fn new() -> Self {
        Default::default()
    }

    /// Returns a modified patch that changes the effective SID to match `effective_sid`.
    /// All temporary SIDs are cleared.
    pub fn effective_sid(&self, effective_sid: SecurityId) -> Self {
        Self {
            effective_sid: Some(effective_sid),
            exec_sid: Some(None),
            fscreate_sid: Some(None),
            keycreate_sid: Some(None),
            sockcreate_sid: Some(None),
        }
    }

    /// Returns a modified patch that sets the fscreate SID to create nodes with the same security
    /// state as `fs_node`.
    pub fn fscreate_sid(&self, fscreate_sid: SecurityId) -> Self {
        Self { fscreate_sid: Some(Some(fscreate_sid)), ..*self }
    }

    /// Runs `f` in `current_task`, with its security attributes modified by the patch. The
    /// security state of `current_task` is restored after the call.
    pub fn run<R>(&self, current_task: &CurrentTask, f: impl FnOnce() -> R) -> R {
        let saved_state;
        {
            let mut task_state = current_task.security_state.lock();
            saved_state = task_state.clone();
            self.apply(&mut *task_state);
        }
        let ret = f();
        *current_task.security_state.lock() = saved_state;
        ret
    }

    fn apply(&self, task_attrs: &mut TaskAttrs) {
        if let Some(effective_sid) = self.effective_sid {
            task_attrs.effective_sid = effective_sid;
        }
        if let Some(exec_sid) = self.exec_sid {
            task_attrs.exec_sid = exec_sid;
        }
        if let Some(fscreate_sid) = self.fscreate_sid {
            task_attrs.fscreate_sid = fscreate_sid;
        }
        if let Some(keycreate_sid) = self.keycreate_sid {
            task_attrs.keycreate_sid = keycreate_sid;
        }
        if let Some(sockcreate_sid) = self.sockcreate_sid {
            task_attrs.sockcreate_sid = sockcreate_sid;
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
pub(super) struct FileSystemState {
    // Fields used prior to policy-load, to hold mount options, etc.
    mount_options: FileSystemMountOptions,
    pending_entries: Mutex<HashSet<WeakKey<DirEntry>>>,

    // Set once the initial policy has been loaded, taking into account `mount_options`.
    label: OnceLock<FileSystemLabel>,
}

impl FileSystemState {
    fn new(mount_options: FileSystemMountOptions) -> Self {
        Self { mount_options, pending_entries: Mutex::new(HashSet::new()), label: OnceLock::new() }
    }

    /// Returns the resolved `FileSystemLabel`, or `None` if no policy has yet been loaded.
    pub fn label(&self) -> Option<&FileSystemLabel> {
        self.label.get()
    }

    /// Returns trye if this file system supports dynamic re-labeling of file nodes.
    pub fn supports_relabel(&self) -> bool {
        // TODO: https://fxbug.dev/357876133 - Fix this to be consistent with observed
        // behaviour under Linux, which allows relabeling of some `genfscon` labeled file systems.
        match self.label() {
            Some(FileSystemLabel { scheme: FileSystemLabelingScheme::FsUse { .. }, .. }) => true,
            _ => false,
        }
    }

    /// Returns true if this file system persists labels in extended attributes.
    pub fn supports_xattr(&self) -> bool {
        match self.label() {
            Some(FileSystemLabel {
                scheme: FileSystemLabelingScheme::FsUse { fs_use_type, .. },
                ..
            }) => *fs_use_type == FsUseType::Xattr,
            _ => false,
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
    fn write_mount_options(
        &self,
        security_server: &SecurityServer,
        buf: &mut impl OutputBuffer,
    ) -> Result<(), Errno> {
        let Some(label) = self.label() else {
            return Self::write_mount_options_to_buf(buf, &self.mount_options);
        };

        let to_context = |sid| security_server.sid_to_security_context(sid);
        let mount_options = FileSystemMountOptions {
            context: label.mount_sids.context.and_then(to_context),
            fs_context: label.mount_sids.fs_context.and_then(to_context),
            def_context: label.mount_sids.def_context.and_then(to_context),
            root_context: label.mount_sids.root_context.and_then(to_context),
        };

        if self.supports_relabel() {
            buf.write_all(b",seclabel").map(|_| ())?;
        }

        Self::write_mount_options_to_buf(buf, &mount_options)
    }

    /// Writes the supplied `mount_options` to the `OutputBuffer`.
    fn write_mount_options_to_buf(
        buf: &mut impl OutputBuffer,
        mount_options: &FileSystemMountOptions,
    ) -> Result<(), Errno> {
        let mut write_option = |prefix: &[u8], option: &Option<Vec<u8>>| -> Result<(), Errno> {
            let Some(value) = option else {
                return Ok(());
            };
            buf.write_all(prefix).map(|_| ())?;
            buf.write_all(value).map(|_| ())
        };
        write_option(b",context=", &mount_options.context)?;
        write_option(b",fscontext=", &mount_options.fs_context)?;
        write_option(b",defcontext=", &mount_options.def_context)?;
        write_option(b",rootcontext=", &mount_options.root_context)
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

/// Security state for a [`crate::vfs::Socket`] instance. This holds the [`selinux::SecurityId`] of
/// the peer socket.
#[derive(Debug, Default)]
pub(super) struct SocketState {
    peer_sid: Mutex<Option<SecurityId>>,
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
fn fs_node_ensure_class(fs_node: &FsNode) -> Result<FsNodeClass, Errno> {
    // TODO: Consider moving the class into a `OnceLock`.
    let class = fs_node.security_state.lock().class;
    if class != FileClass::File.into() {
        return Ok(class);
    }

    let file_mode = fs_node.info().mode;
    let class = file_class_from_file_mode(file_mode)?.into();
    fs_node.security_state.lock().class = class;
    Ok(class)
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
                .unwrap_or_else(|| InitialSid::Unlabeled.into()),
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
