// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::availability::AvailabilityMetadata;

/// A route request metadata key for the capability type.
pub const METADATA_KEY_TYPE: &'static str = "type";

/// The capability type value for a protocol.
pub const TYPE_PROTOCOL: &'static str = "protocol";
pub const TYPE_CONFIG: &'static str = "configuration";

/// Returns a `Dict` containing Router Request metadata specifying a Protocol
/// porcelain type.
pub fn protocol_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_PROTOCOL))),
        )
        .unwrap();
    metadata.set_availability(availability);
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Dictionary
/// porcelain type.
pub fn dictionary_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(
                cm_rust::CapabilityTypeName::Dictionary.to_string(),
            ))),
        )
        .unwrap();
    metadata.set_availability(availability);
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Config
/// porcelain type.
pub fn config_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_CONFIG))),
        )
        .unwrap();
    metadata.set_availability(availability);
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Runner
/// porcelain type.
pub fn runner_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(
                cm_rust::CapabilityTypeName::Runner.to_string(),
            )),
        )
        .unwrap();
    metadata.set_availability(availability);
    metadata
}
