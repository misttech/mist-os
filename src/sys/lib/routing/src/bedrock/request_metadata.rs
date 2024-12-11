// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::rights::Rights;
use cm_rust::Availability;
use sandbox::{Capability, Data, Dict, DictKey};
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

/// A route request metadata key for the capability type.
pub const METADATA_KEY_TYPE: &'static str = "type";

/// The capability type value for a protocol.
pub const TYPE_PROTOCOL: &'static str = "protocol";
pub const TYPE_DICTIONARY: &'static str = "dictionary";
pub const TYPE_CONFIG: &'static str = "configuration";

/// A type which has accessors for route request metadata of type T.
pub trait Metadata<T> {
    /// A key string used for setting and getting the metadata.
    const KEY: &'static str;

    /// Infallibly assigns `value` to `self`.
    fn set_metadata(&self, value: T);

    /// Retrieves the subdir metadata from `self`, if present.
    fn get_metadata(&self) -> Option<T>;
}

impl Metadata<Availability> for Dict {
    const KEY: &'static str = "availability";

    fn set_metadata(&self, value: Availability) {
        let key = DictKey::new(<Self as Metadata<Availability>>::KEY)
            .expect("dict key creation failed unexpectedly");
        match self.insert(key, Capability::Data(Data::String(value.to_string()))) {
            // When an entry already exists for a key in a Dict, insert() will
            // still replace that entry with the new value, even though it
            // returns an ItemAlreadyExists error. As a result, we can treat
            // ItemAlreadyExists as a success case.
            Ok(()) | Err(fsandbox::CapabilityStoreError::ItemAlreadyExists) => (),
            // Dict::insert() only returns `CapabilityStoreError::ItemAlreadyExists` variant
            Err(e) => panic!("unexpected error variant returned from Dict::insert(): {e:?}"),
        }
    }

    fn get_metadata(&self) -> Option<Availability> {
        let key = DictKey::new(<Self as Metadata<Availability>>::KEY)
            .expect("dict key creation failed unexpectedly");
        let capability = self.get(&key).ok()??;
        match capability {
            Capability::Data(Data::String(availability)) => match availability.as_str() {
                "Optional" => Some(Availability::Optional),
                "Required" => Some(Availability::Required),
                "SameAsTarget" => Some(Availability::SameAsTarget),
                "Transitional" => Some(Availability::Transitional),
                _ => None,
            },
            _ => None,
        }
    }
}

impl Metadata<Rights> for Dict {
    const KEY: &'static str = "rights";

    fn set_metadata(&self, value: Rights) {
        let key = DictKey::new(<Self as Metadata<Rights>>::KEY)
            .expect("dict key creation failed unexpectedly");
        match self.insert(key, Capability::Data(Data::Uint64(value.into()))) {
            // When an entry already exists for a key in a Dict, insert() will
            // still replace that entry with the new value, even though it
            // returns an ItemAlreadyExists error. As a result, we can treat
            // ItemAlreadyExists as a success case.
            Ok(()) | Err(fsandbox::CapabilityStoreError::ItemAlreadyExists) => (),
            // Dict::insert() only returns `CapabilityStoreError::ItemAlreadyExists` variant
            Err(e) => panic!("unexpected error variant returned from Dict::insert(): {e:?}"),
        }
    }

    fn get_metadata(&self) -> Option<Rights> {
        let key = DictKey::new(<Self as Metadata<Rights>>::KEY)
            .expect("dict key creation failed unexpectedly");
        let capability = self.get(&key).ok()??;
        let rights = match capability {
            Capability::Data(Data::Uint64(rights)) => fio::Operations::from_bits(rights)?,
            _ => None?,
        };
        Some(Rights::from(rights))
    }
}

/// Returns a `Dict` containing Router Request metadata specifying a Protocol porcelain type.
pub fn protocol_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_PROTOCOL))),
        )
        .unwrap();
    metadata.set_metadata(availability);
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Dictionary porcelain type.
pub fn dictionary_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_DICTIONARY))),
        )
        .unwrap();
    metadata.set_metadata(availability);
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Config porcelain type.
pub fn config_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(String::from(TYPE_CONFIG))),
        )
        .unwrap();
    metadata.set_metadata(availability);
    metadata
}

/// Returns a `Dict` containing Router Request metadata specifying a Runner porcelain type.
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
    metadata.set_metadata(availability);
    metadata
}

/// Returns a `Dict` Containing Router Request metadata specifying a Resolver porcelain type.
pub fn resolver_metadata(availability: cm_types::Availability) -> sandbox::Dict {
    let metadata = sandbox::Dict::new();
    metadata
        .insert(
            cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
            sandbox::Capability::Data(sandbox::Data::String(
                cm_rust::CapabilityTypeName::Resolver.to_string(),
            )),
        )
        .unwrap();
    metadata.set_metadata(availability);
    metadata
}
