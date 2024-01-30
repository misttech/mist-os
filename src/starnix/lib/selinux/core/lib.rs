// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod access_vector_cache;
pub mod permission_check;
pub mod security_server;
pub mod seq_lock;

/// The Security ID (SID) used internally to refer to a security context.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SecurityId(u64);

impl From<u64> for SecurityId {
    fn from(sid: u64) -> Self {
        Self(sid)
    }
}
