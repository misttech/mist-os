// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod access_vector_cache;
pub mod permission_check;
pub mod security_server;
pub mod seq_lock;

pub use selinux_common::InitialSid;

use std::num::NonZeroU32;

/// The Security ID (SID) used internally to refer to a security context.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SecurityId(NonZeroU32);

impl SecurityId {
    /// Returns a `SecurityId` encoding the specified initial Security Context.
    /// These are used when labeling kernel resources created before policy
    /// load, allowing the policy to determine the Security Context to use.
    pub fn initial(initial_sid: InitialSid) -> Self {
        Self(NonZeroU32::new(initial_sid as u32).unwrap())
    }
}
