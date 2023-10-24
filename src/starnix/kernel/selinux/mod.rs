// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod access_vector_cache;
pub mod fs;
pub mod security_server;

use bitflags::bitflags;

/// The Security ID (SID) used internally to refer to a security context.
pub struct SecurityId(u32);

impl From<u32> for SecurityId {
    fn from(sid: u32) -> Self {
        Self(sid)
    }
}

/// An identifier for a class of object with SELinux-managed rights.
pub enum ObjectClass {
    // TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    Process,
    // TODO: Include all object classes supported by SELinux.
}

bitflags! {
    /// The set of rights that may be granted to sources accessing targets controlled by SELinux.
    pub struct AccessVector: u32 {
        // TODO: Add rights that may be included in an access vector cache response.
    }
}

impl AccessVector {
    pub const NONE: AccessVector = AccessVector { bits: 0 };
}

/// An interface for computing the rights permitted to a subject accessing an object (or target) of
/// a particular SELinux object type.
pub trait AccessQueryable: Send {
    /// Computes the [`Rights`] permitted to `source_sid` for accessing `target_sid`, an object of
    ///  type `target_class`.
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector;
}

/// A default implementation for [`AccessQueryable`] that permits no [`Rights`].
#[derive(Default)]
pub struct DenyAll;

impl AccessQueryable for DenyAll {
    fn query(
        &mut self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
    ) -> AccessVector {
        AccessVector::NONE
    }
}
