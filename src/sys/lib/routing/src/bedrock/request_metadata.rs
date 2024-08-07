// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// A route request metadata key for the capability type.
pub const METADATA_KEY_TYPE: &'static str = "type";

/// The capability type value for a protocol.
pub const TYPE_PROTOCOL: &'static str = "protocol";
pub const TYPE_CONFIG: &'static str = "configuration";

/// Returns a `Dict` containing Router Request metadata specifying a Protocol
/// porcelain type.
pub fn protocol_metadata() -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_PROTOCOL))),
        )
        .unwrap();
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Config
/// porcelain type.
pub fn config_metadata() -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_CONFIG))),
        )
        .unwrap();
    metadata
}
