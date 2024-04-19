// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This module provides types and hook APIs supporting Linux Security Modules
/// functionality in Starnix.  LSM provides a generic set of hooks, and opaque
/// types, used to decouple the rest of the kernel from the details of any
/// specific security enforcement subsystem (e.g. SELinux, POSIX.1e, etc).
///
/// Although this module is hard-wired to the SELinux implementation, callers
/// should treat the types as opaque; hook implementations necessarily have access
/// to kernel structures, but not the other way around.
use selinux::SecurityId;

/// SELinux implementations called by the LSM hooks.
mod selinux_hooks;

/// Linux Security Modules hooks for use within the Starnix kernel.
mod hooks;
pub use hooks::*;

/// Opaque structure encapsulating security state for a `ThreadGroup`.
#[derive(Debug)]
pub struct ThreadGroupState(selinux_hooks::ThreadGroupState);

/// Opaque structure holding security state associated with a `ResolvedElf` instance.
#[derive(Debug, PartialEq)]
pub struct ResolvedElfState(SecurityId);

// TODO(b/322850635): Create a clean separation between the procattr filesystem, and LSM hooks.
// TODO(b/335397745): Move the SELinux filesystem bits under the selinux directory.
pub mod fs;
