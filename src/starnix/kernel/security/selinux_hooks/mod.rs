// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(super) mod selinuxfs;
pub(super) mod task;
pub(super) mod testing;

use super::{FsNodeSecurityXattr, FsNodeState};
use crate::task::CurrentTask;
use crate::vfs::fs_args::MountParams;
use crate::vfs::{
    FileSystem, FsNode, FsNodeHandle, FsStr, FsString, NamespaceNode, ValueOrSize, XattrOp,
};
use linux_uapi::XATTR_NAME_SELINUX;
use selinux::{ClassPermission, InitialSid, Permission, ProcessPermission};
use selinux_core::permission_check::PermissionCheck;
use selinux_core::security_server::SecurityServer;
use selinux_core::SecurityId;
use starnix_logging::track_stub;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::unmount_flags::UnmountFlags;
use std::sync::OnceLock;

/// Maximum supported size for the extended attribute value used to store SELinux security
/// contexts in a filesystem node extended attributes.
const SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE: usize = 4096;

/// Checks if the task with `_source_sid` has the permission to mount at `_path` the object specified by
/// `_dev_name` of type `_fs_type`, with the mounting flags `_flags` and filesystem data `_data`.
pub(super) fn sb_mount(
    _permission_check: &impl PermissionCheck,
    _current_task: &CurrentTask,
    _dev_name: &bstr::BStr,
    _path: &NamespaceNode,
    _fs_type: &bstr::BStr,
    _flags: MountFlags,
    _data: &bstr::BStr,
) -> Result<(), Errno> {
    track_stub!(TODO("https://fxbug.dev/352507622"), "sb_mount: validate permission");
    Ok(())
}

/// Checks if the task with `_source_sid` has the permission to unmount the filesystem mounted on
/// `_node` using the unmount flags `_flags`.
pub(super) fn sb_umount(
    _permission_check: &impl PermissionCheck,
    _current_task: &CurrentTask,
    _node: &NamespaceNode,
    _flags: UnmountFlags,
) -> Result<(), Errno> {
    track_stub!(TODO("https://fxbug.dev/353936182"), "sb_umount: validate permission");
    Ok(())
}

// Returns the labeling scheme to use for `fs`.
// Must not be called until a policy has been loaded.
fn ensure_fs_label<'a>(
    security_server: &SecurityServer,
    fs: &'a FileSystem,
) -> &'a FileSystemLabel {
    assert!(security_server.has_policy());
    fs.security_state.state.label.get_or_init(|| {
        let mount_options = &fs.security_state.state.mount_options;
        if let Some(context) = &mount_options.context {
            let sid = security_server
                .security_context_to_sid((&context).into())
                .ok()
                .unwrap_or(SecurityId::initial(InitialSid::Unlabeled));
            return FileSystemLabel { sid, scheme: FileSystemLabelingScheme::Mountpoint };
        }
        let fs_sid = mount_options
            .fs_context
            .as_ref()
            .and_then(|context| security_server.security_context_to_sid((&context).into()).ok())
            .unwrap_or(SecurityId::initial(InitialSid::Unlabeled));
        let root_sid = mount_options
            .root_context
            .as_ref()
            .and_then(|context| security_server.security_context_to_sid((&context).into()).ok())
            .unwrap_or(fs_sid);
        let def_sid = mount_options
            .def_context
            .as_ref()
            .and_then(|context| security_server.security_context_to_sid((&context).into()).ok())
            .unwrap_or(SecurityId::initial(InitialSid::File));

        FileSystemLabel {
            sid: fs_sid,
            // TODO: https://fxbug.dev/355628002 - Determine labeling scheme from policy.
            scheme: FileSystemLabelingScheme::FsUse { def_sid, root_sid },
        }
    })
}

/// Returns the security attribute to label a newly created inode with, if any.
pub(super) fn fs_node_init_security_and_xattr(
    _security_server: &SecurityServer,
    _current_task: &CurrentTask,
    _new_node: &FsNodeHandle,
    _parent: Option<&FsNodeHandle>,
) -> Result<Option<FsNodeSecurityXattr>, Errno> {
    // TODO(b/334091674): If there is no `parent` then this is the "root" node; apply `root_context`, if set.
    Ok(None)
}

/// Returns the Security Context corresponding to the SID with which `FsNode`
/// is labelled, otherwise delegates to the node's [`crate::vfs::FsNodeOps`].
pub(super) fn fs_node_getsecurity(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    max_size: usize,
) -> Result<ValueOrSize<FsString>, Errno> {
    if name == FsStr::new(XATTR_NAME_SELINUX.to_bytes()) {
        // If a SID can be resolved for the node then use that.
        let sid = fs_node_resolve_security_label(security_server, current_task, fs_node);
        if let Some(sid) = sid {
            if let Some(context) = security_server.sid_to_security_context(sid) {
                return Ok(ValueOrSize::Value(context.into()));
            }
        }

        // If the node is still unlabelled at this point then it most likely does have a value set for
        // "security.selinux", but the value is not a valid Security Context, so we defer to the
        // attribute value stored in the file system for this node.
    }
    fs_node.ops().get_xattr(fs_node, current_task, name, max_size)
}

/// Sets the `name`d security attribute on `fs_node` and updates internal
/// kernel state.
pub(super) fn fs_node_setsecurity(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    value: &FsStr,
    op: XattrOp,
) -> Result<(), Errno> {
    fs_node.ops().set_xattr(fs_node, current_task, name, value, op)?;
    if name == FsStr::new(XATTR_NAME_SELINUX.to_bytes()) {
        // Update or remove the SID from `fs_node`, dependent whether the new value
        // represents a valid Security Context.
        match security_server.security_context_to_sid(value.into()) {
            Ok(sid) => set_cached_sid(fs_node, sid),
            Err(_) => clear_cached_sid(fs_node),
        }
    }
    Ok(())
}

/// Returns the `SecurityId` that should be used for SELinux access control checks against `fs_node`.
/// The node's "effective" SID is the one assigned to it by `fs_node_resolve_security_label()`,
/// otherwise falling-back to using the policy's "unlabeled" initial SID.
// TODO: https://fxbug.dev/334094811 - Validate that "unlabeled" is the right fall-back.
fn fs_node_effective_sid(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> SecurityId {
    fs_node_resolve_security_label(security_server, current_task, fs_node)
        .unwrap_or(SecurityId::initial(InitialSid::Unlabeled))
}

/// Determines the appropriate SID with which to label `fs_node`, labels `fs_node` with it, and
/// returns it.
/// If `fs_node` resides on a mountpoint-labelled file system then use the Security Context
/// specified in the "context=" option.
/// If `fs_node` resides on an xattr-labelled file system then the Security Context is read from the
/// node's "security.selinux" attribute, and the associated SID attached to `fs_node`, and returned.
/// If `fs_node` has no "security.selinux" extended attribute then the SID of the node's file
/// system's default Security Context is stored, and returned.
/// If the "security.selinux" attribute is invalid, the file system does not support xattr labelling
/// or an error occurs while reading the attribute, then `None` is returned.
fn fs_node_resolve_security_label(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Option<SecurityId> {
    // Early-return here, so that the `fs_node.info()` read lock is released before performing
    // the resolution logic below.
    if let Some(sid) = fs_node.info().security_state.sid {
        return Some(sid);
    }

    let fs = fs_node.fs();
    let label = ensure_fs_label(security_server, &fs);
    let sid = match label.scheme {
        // mountpoint-labelling labels every node from the "context=" mount option.
        FileSystemLabelingScheme::Mountpoint => Some(label.sid),
        FileSystemLabelingScheme::FsUse { def_sid, .. } => {
            // fs_use_xattr-labelling defers to the security attribute on the file node, otherwise uses a
            // default value specified via mount option, or from the policy.
            let attr = fs_node.ops().get_xattr(
                fs_node,
                current_task,
                XATTR_NAME_SELINUX.to_bytes().into(),
                SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
            );
            match attr {
                Ok(ValueOrSize::Value(security_context)) => {
                    security_server.security_context_to_sid((&security_context).into()).ok()
                }
                _ => {
                    // TODO: https://fxbug.dev/334094811 - Determine how to handle errors besides
                    // `ENODATA` (no such xattr).
                    Some(def_sid)
                }
            }
        }
    };

    if let Some(sid) = sid {
        set_cached_sid(fs_node, sid);
    }
    sid
}

/// Checks if `permissions` are allowed from the task with `source_sid` to the task with `target_sid`.
fn check_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permissions: P,
) -> Result<(), Errno> {
    match permission_check.has_permission(source_sid, target_sid, permissions) {
        true => Ok(()),
        false => error!(EACCES),
    }
}

/// Checks that `subject_sid` has the specified process `permissions` on `self`.
fn check_self_permission(
    permission_check: &impl PermissionCheck,
    subject_sid: SecurityId,
    permissions: ProcessPermission,
) -> Result<(), Errno> {
    check_permission(permission_check, subject_sid, subject_sid, permissions)
}

/// Return security state to associate with a filesystem based on the supplied mount options.
pub fn file_system_init_security(
    _fs_type: &FsStr,
    mount_params: &MountParams,
) -> Result<FileSystemState, Errno> {
    let context = mount_params.get(FsStr::new(b"context")).cloned();
    let def_context = mount_params.get(FsStr::new(b"defcontext")).cloned();
    let fs_context = mount_params.get(FsStr::new(b"fscontext")).cloned();
    let root_context = mount_params.get(FsStr::new(b"rootcontext")).cloned();

    #[cfg(not(test))]
    // TODO: https://fxbug.dev/355628002 - Remove this as soon as `fs_use_*` Contexts are being determine from policy.
    let def_context = if **_fs_type == *b"tmpfs" && def_context.is_none() && context.is_none() {
        Some(b"u:object_r:tmpfs:s0".into())
    } else {
        def_context
    };

    // If a "context" is specified then it is used for all nodes in the filesystem, so none of the other
    // security context options would be meaningful to combine with it.
    if context.is_some()
        && (def_context.is_some() || fs_context.is_some() || root_context.is_some())
    {
        return error!(EINVAL);
    }

    let mount_options = FileSystemMountOptions { context, def_context, fs_context, root_context };

    Ok(FileSystemState { mount_options, label: OnceLock::new() })
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

/// Security state for a [`crate::vfs::FileSystem`] instance. This holds the security fields
/// parsed from the mount options and the selected labeling scheme.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FileSystemState {
    // TODO: https://fxbug.dev/349800754 - Investigate whether the mount options
    // need to be retained after the file system has been labeled.
    mount_options: FileSystemMountOptions,
    label: OnceLock<FileSystemLabel>,
}

#[derive(Clone, Debug, PartialEq)]
struct FileSystemLabel {
    sid: SecurityId,
    scheme: FileSystemLabelingScheme,
}

#[derive(Clone, Debug, PartialEq)]
enum FileSystemLabelingScheme {
    /// This filesystem was mounted with "context=".
    Mountpoint,
    /// This filesystem has an "fs_use_xattr", "fs_use_task", or "fs_use_trans" entry in the
    /// policy. `root_sid` identifies the context for the root of the filesystem and `def_sid`
    /// identifies the context to use for unlabeled files in the filesystem (the "default
    /// context").
    FsUse { def_sid: SecurityId, root_sid: SecurityId },
}

/// SELinux security context-related filesystem mount options. These options are documented in the
/// `context=context, fscontext=context, defcontext=context, and rootcontext=context` section of
/// the `mount(8)` manpage.
#[derive(Clone, Debug, PartialEq)]
struct FileSystemMountOptions {
    /// Specifies the effective security context to use for all nodes in the filesystem, and the
    /// filesystem itself. If the filesystem already contains security attributes then these are
    /// ignored. May not be combined with any of the other options.
    context: Option<FsString>,
    /// Specifies an effective security context to use for un-labeled nodes in the filesystem,
    /// rather than falling-back to the policy-defined "file" context.
    def_context: Option<FsString>,
    /// The value of the `fscontext=[security-context]` mount option. This option is used to
    /// label the filesystem (superblock) itself.
    fs_context: Option<FsString>,
    /// The value of the `rootcontext=[security-context]` mount option. This option is used to
    /// (re)label the inode located at the filesystem mountpoint.
    root_context: Option<FsString>,
}

/// Sets the cached security id associated with `fs_node` to `sid`. Storing the security id will
/// cause the security id to *not* be recomputed by the SELinux LSM when determining the effective
/// security id of this [`FsNode`].
fn set_cached_sid(fs_node: &FsNode, sid: SecurityId) {
    fs_node.update_info(|info| info.security_state = FsNodeState { sid: Some(sid) });
}

/// Clears the cached security id on `fs_node`. Clearing the security id will cause the security id
/// to be be recomputed by the SELinux LSM when determining the effective security id of this
/// [`FsNode`].
fn clear_cached_sid(fs_node: &FsNode) {
    fs_node.update_info(|info| info.security_state = FsNodeState { sid: None });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::create_kernel_task_and_unlocked_with_selinux;
    use crate::vfs::XattrOp;
    use starnix_uapi::errno;

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";

    #[fuchsia::test]
    async fn fs_node_resolved_and_effective_sids_for_missing_xattr() {
        let security_server = testing::security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let node = &testing::create_test_file(&mut locked, &current_task).entry.node;

        // Remove the "security.selinux" label, if any.
        let _ = node.ops().remove_xattr(node, &current_task, XATTR_NAME_SELINUX.to_bytes().into());
        assert_eq!(
            node.ops()
                .get_xattr(node, &current_task, XATTR_NAME_SELINUX.to_bytes().into(), 4096)
                .unwrap_err(),
            errno!(ENODATA)
        );

        // Clear the cached SID to force it to be (re-)resolved from the label.
        clear_cached_sid(node);
        assert_eq!(None, testing::get_cached_sid(node));

        // `fs_node_getsecurity()` should now fall-back to the policy's "file" Context.
        let default_file_context = security_server
            .sid_to_security_context(SecurityId::initial(InitialSid::File))
            .unwrap()
            .into();
        let result = fs_node_getsecurity(
            &security_server,
            &current_task,
            node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
        )
        .unwrap();
        assert_eq!(result, ValueOrSize::Value(default_file_context));
        assert!(testing::get_cached_sid(node).is_some());
    }

    #[fuchsia::test]
    async fn fs_node_resolved_and_effective_sids_for_invalid_xattr() {
        let security_server = testing::security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let node = &testing::create_test_file(&mut locked, &current_task).entry.node;

        const INVALID_CONTEXT: &[u8] = b"invalid context!";

        // Set the security label to a value which is not a valid Security Context.
        node.ops()
            .set_xattr(
                node,
                &current_task,
                XATTR_NAME_SELINUX.to_bytes().into(),
                INVALID_CONTEXT.into(),
                XattrOp::Set,
            )
            .expect("setxattr");

        // Clear the cached SID to force it to be (re-)resolved from the label.
        clear_cached_sid(node);
        assert_eq!(None, testing::get_cached_sid(node));

        // `fs_node_getsecurity()` should report the same invalid string as is in the xattr.
        let result = fs_node_getsecurity(
            &security_server,
            &current_task,
            node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
        )
        .unwrap();
        assert_eq!(result, ValueOrSize::Value(INVALID_CONTEXT.into()));

        // No SID should be cached for the node.
        assert_eq!(None, testing::get_cached_sid(node));

        // The effective SID of the node should be "unlabeled".
        assert_eq!(
            SecurityId::initial(InitialSid::Unlabeled),
            fs_node_effective_sid(&security_server, &current_task, node)
        );
    }

    #[fuchsia::test]
    async fn fs_node_effective_sid_valid_xattr_stored() {
        let security_server = testing::security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let node = &testing::create_test_file(&mut locked, &current_task).entry.node;

        // Store a valid Security Context in the attribute, and ensure any label cached on the `FsNode`
        // is removed, to force the effective-SID query to resolve the label again.
        node.ops()
            .set_xattr(
                node,
                &current_task,
                XATTR_NAME_SELINUX.to_bytes().into(),
                VALID_SECURITY_CONTEXT.into(),
                XattrOp::Set,
            )
            .expect("setxattr");

        // Clear the cached SID to force it to be (re-)resolved from the label.
        clear_cached_sid(node);
        assert_eq!(None, testing::get_cached_sid(node));

        // `fs_node_getsecurity()` should report the same valid Security Context string as the xattr holds.
        let result = fs_node_getsecurity(
            &security_server,
            &current_task,
            node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
        )
        .unwrap();
        assert_eq!(result, ValueOrSize::Value(VALID_SECURITY_CONTEXT.into()));

        // There should be a SID cached, and it should map to the valid Security Context.
        let cached_sid = testing::get_cached_sid(node).unwrap();
        assert_eq!(
            security_server.sid_to_security_context(cached_sid).unwrap(),
            VALID_SECURITY_CONTEXT
        );

        // Requesting the effective SID should simply return the cached value.
        assert_eq!(cached_sid, fs_node_effective_sid(&security_server, &current_task, node));
    }

    #[fuchsia::test]
    async fn setxattr_set_sid() {
        let security_server = testing::security_server_with_policy();
        let expected_sid = security_server
            .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
            .expect("no SID for VALID_SECURITY_CONTEXT");
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &testing::create_unlabeled_test_file(&mut locked, &current_task).entry.node;

        node.set_xattr(
            current_task.as_ref(),
            &current_task.fs().root().mount,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("setxattr");

        // Verify that the SID now cached on the node corresponds to VALID_SECURITY_CONTEXT.
        assert_eq!(Some(expected_sid), testing::get_cached_sid(node));
    }
}
