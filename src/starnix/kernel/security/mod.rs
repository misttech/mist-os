// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module provides types and hook APIs supporting Linux Security Modules
//! functionality in Starnix.  LSM provides a generic set of hooks, and opaque
//! types, used to decouple the rest of the kernel from the details of any
//! specific security enforcement subsystem (e.g. SELinux, POSIX.1e, etc).
//!
//! Although this module is hard-wired to the SELinux implementation, callers
//! should treat the types as opaque; hook implementations necessarily have access
//! to kernel structures, but not the other way around.

use selinux_core::security_server::SecurityServer;
use selinux_core::SecurityId;
use std::sync::Arc;

/// SELinux implementations called by the LSM hooks.
mod selinux_hooks;

/// Linux Security Modules hooks for use within the Starnix kernel.
mod hooks;
pub use hooks::*;

/// Opaque structure encapsulating security subsystem state for the whole system.
pub struct KernelState {
    server: Option<Arc<SecurityServer>>,
}

/// Opaque structure encapsulating security state for a `ThreadGroup`.
#[derive(Debug)]
pub struct TaskState {
    attrs: selinux_hooks::TaskAttrs,
}

/// Opaque structure holding security state associated with a `ResolvedElf` instance.
#[derive(Debug, PartialEq)]
pub struct ResolvedElfState {
    sid: Option<SecurityId>,
}

/// The opaque type used by [`crate::vfs::FsNodeInfo`] to store security state. Note that
/// [`crate::vfs::FsNodeInfo`] implements `Default` and `Clone`, requiring [`FsNodeState`] to
/// implement them as well.
#[derive(Debug, Default, Clone)]
pub struct FsNodeState {
    sid: Option<SecurityId>,
}

/// Opaque structure holding security state for a [`crate::vfs::Filesystem`].
#[derive(Debug, Clone)]
pub struct FileSystemState {
    state: selinux_hooks::FileSystemState,
}
