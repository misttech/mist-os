// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

extern crate core as rust_core;
#[macro_use]
extern crate lazy_static;

/// Peers are identified by ids, which should be treated as opaque by service
/// libraries. Stack implementations should ensure that each PeerId identifies a
/// single peer over a single instance of the stack - a
/// [`bt_gatt::Central::connect`] should always attempt to connect to the
/// same peer as long as the PeerId was retrieved after the `Central` was
/// instantiated. PeerIds can be valid longer than that (often if the peer is
/// bonded)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerId(pub u64);

impl rust_core::fmt::Display for PeerId {
    fn fmt(
        &self,
        f: &mut rust_core::fmt::Formatter<'_>,
    ) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "{:x}", self.0)
    }
}

pub mod core;

pub mod company_id;
pub use company_id::CompanyId;

pub mod generic_audio;

pub mod packet_encoding;

pub mod uuids;
pub use crate::uuids::Uuid;

pub mod debug_command;
