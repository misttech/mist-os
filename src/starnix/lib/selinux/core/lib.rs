// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod access_vector_cache;
pub mod permission_check;
pub mod security_server;

use selinux::policy::arrays::FsUseType;
pub use selinux::InitialSid;

use std::num::NonZeroU32;

/// The Security ID (SID) used internally to refer to a security context.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct SecurityId(NonZeroU32);

impl SecurityId {
    /// Returns a `SecurityId` encoding the specified initial Security Context.
    /// These are used when labeling kernel resources created before policy
    /// load, allowing the policy to determine the Security Context to use.
    pub fn initial(initial_sid: InitialSid) -> Self {
        Self(NonZeroU32::new(initial_sid as u32).unwrap())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileSystemLabel {
    pub sid: SecurityId,
    pub scheme: FileSystemLabelingScheme,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FileSystemLabelingScheme {
    /// This filesystem was mounted with "context=".
    Mountpoint,
    /// This filesystem has an "fs_use_xattr", "fs_use_task", or "fs_use_trans" entry in the
    /// policy. `root_sid` identifies the context for the root of the filesystem and `def_sid`
    /// identifies the context to use for unlabeled files in the filesystem (the "default
    /// context").
    FsUse { fs_use_type: FsUseType, def_sid: SecurityId, root_sid: SecurityId },
}

/// SELinux security context-related filesystem mount options. These options are documented in the
/// `context=context, fscontext=context, defcontext=context, and rootcontext=context` section of
/// the `mount(8)` manpage.
#[derive(Clone, Debug, PartialEq)]
pub struct FileSystemMountOptions {
    /// Specifies the effective security context to use for all nodes in the filesystem, and the
    /// filesystem itself. If the filesystem already contains security attributes then these are
    /// ignored. May not be combined with any of the other options.
    pub context: Option<Vec<u8>>,
    /// Specifies an effective security context to use for un-labeled nodes in the filesystem,
    /// rather than falling-back to the policy-defined "file" context.
    pub def_context: Option<Vec<u8>>,
    /// The value of the `fscontext=[security-context]` mount option. This option is used to
    /// label the filesystem (superblock) itself.
    pub fs_context: Option<Vec<u8>>,
    /// The value of the `rootcontext=[security-context]` mount option. This option is used to
    /// (re)label the inode located at the filesystem mountpoint.
    pub root_context: Option<Vec<u8>>,
}

/// Status information parameter for the [`SeLinuxStatusPublisher`] interface.
pub struct SeLinuxStatus {
    /// SELinux-wide enforcing vs. permissive mode  bit.
    pub is_enforcing: bool,
    /// Number of times the policy has been changed since SELinux started.
    pub change_count: u32,
    /// Bit indicating whether operations unknonwn SELinux abstractions will be denied.
    pub deny_unknown: bool,
}

/// Interface for security server to interact with selinuxfs status file.
pub trait SeLinuxStatusPublisher: Send {
    /// Sets the value part of the associated selinuxfs status file.
    fn set_status(&mut self, policy_status: SeLinuxStatus);
}
