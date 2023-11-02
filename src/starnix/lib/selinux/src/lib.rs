// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod access_vector_cache;
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

/// The security context, a variable-length string associated with each SELinux object in the
/// system. Security contexts are configured by userspace atop Starnix, and mapped to
/// [`SecurityId`]s for internal use in Starnix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityContext(String);

impl From<&str> for SecurityContext {
    fn from(security_context: &str) -> Self {
        Self(security_context.to_string())
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
    #[derive(Default)]
    pub struct AccessVector: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;

        // TODO: Add rights that may be included in an access vector cache response.
    }
}

impl AccessVector {
    pub const NONE: AccessVector = AccessVector { bits: 0 };
    pub const ALL: AccessVector = AccessVector { bits: u32::MAX };
}

impl Into<u32> for AccessVector {
    fn into(self) -> u32 {
        self.bits() as u32
    }
}

/// An interface for computing the rights permitted to a source accessing a target of a particular
/// SELinux object type.
pub trait MutableAccessQueryable: Send {
    /// Computes the [`AccessVector`] permitted to `source_sid` for accessing `target_sid`, an
    /// object of type `target_class`.
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector;
}

/// An interface for computing the rights permitted to a source accessing a target of a particular
/// SELinux object type.
pub trait AccessQueryable: Send {
    /// Computes the [`AccessVector`] permitted to `sid` for accessing `tid`, an object of of type `ty`.
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector;
}

impl<AQ: AccessQueryable> MutableAccessQueryable for AQ {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        (self as &dyn AccessQueryable).query(source_sid, target_sid, target_class)
    }
}

/// A default implementation for [`AccessQueryable`] that permits no [`AccessVector`].
#[derive(Default)]
pub struct DenyAll;

impl AccessQueryable for DenyAll {
    fn query(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
    ) -> AccessVector {
        AccessVector::NONE
    }
}
