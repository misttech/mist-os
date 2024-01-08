// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod access_vector_cache;
pub mod hooks;
pub mod permission_check;
pub mod security_context;
pub mod security_server;

use bitflags::bitflags;

/// The Security ID (SID) used internally to refer to a security context.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct SecurityId(u64);

impl From<u64> for SecurityId {
    fn from(sid: u64) -> Self {
        Self(sid)
    }
}

/// An identifier for a class of object with SELinux-managed rights.
#[derive(Clone, Copy, PartialEq)]
pub enum ObjectClass {
    /// Placeholder value used when an [`ObjectClass`] is required, but uninitialized.
    Undefined,
    // TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    Process,
    // TODO: Include all object classes supported by SELinux.
}

impl Default for ObjectClass {
    fn default() -> Self {
        Self::Undefined
    }
}

bitflags! {
    /// The set of rights that may be granted to sources accessing targets controlled by SELinux.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct AccessVector: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;

        // TODO: Add rights that may be included in an access vector cache response.
    }
}

impl AccessVector {
    pub const NONE: AccessVector = AccessVector::from_bits_retain(0);
    pub const ALL: AccessVector = AccessVector::from_bits_retain(u32::MAX);
}

impl Into<u32> for AccessVector {
    fn into(self) -> u32 {
        self.bits() as u32
    }
}
