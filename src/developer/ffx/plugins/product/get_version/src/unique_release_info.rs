// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! UniqueReleaseInfo defines a struct for holding release information for assembly
//! input artifacts, and is used to construct the output of `ffx product get-version`.
//!
//! This file represents a contract between ffx, MOS, and fuchsia infrastructure.
//! Changes to this file must be implemented using soft transitions.

use assembly_partitions_config::Slot;
use assembly_release_info::{BoardReleaseInfo, ProductReleaseInfo, ReleaseInfo};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::hash::{Hash, Hasher};

/// Struct holding release information (name, repository, version, etc..)
/// for a given assembly input artifact.
#[derive(Clone, Debug, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UniqueReleaseInfo {
    /// The name of this assembly artifact.
    pub name: String,

    /// Release version for this assembly artifact.
    pub version: String,

    /// Origin where this release artifact was created.
    pub repository: String,

    /// System image location.
    pub slot: Vec<Slot>,
}

impl PartialOrd for UniqueReleaseInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for UniqueReleaseInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name
            .cmp(&other.name)
            .then_with(|| self.version.cmp(&other.version))
            .then_with(|| self.repository.cmp(&other.repository))
    }
}

impl PartialEq for UniqueReleaseInfo {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.version == other.version
            && self.repository == other.repository
    }
}

impl Hash for UniqueReleaseInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.version.hash(state);
        self.repository.hash(state);
    }
}

/// Convert a ReleaseInfo instance into a UniqueReleaseInfo instance,
/// to better suit the format needed by customers of `ffx product get-version`.
pub fn from_release_info(info: &ReleaseInfo, slot: &Option<Slot>) -> UniqueReleaseInfo {
    UniqueReleaseInfo {
        name: info.name.clone(),
        version: info.version.clone(),
        repository: info.repository.clone(),
        slot: if let Some(s) = slot { vec![s.clone()] } else { vec![] },
    }
}

/// Convert a BoardReleaseInfo instance into a UniqueReleaseInfo instance,
/// to better suit the format needed by customers of `ffx product get-version`.
pub fn from_board_release_info(info: &BoardReleaseInfo, slot: &Option<Slot>) -> UniqueReleaseInfo {
    UniqueReleaseInfo {
        name: info.info.name.clone(),
        version: info.info.version.clone(),
        repository: info.info.repository.clone(),
        slot: if let Some(s) = slot { vec![s.clone()] } else { vec![] },
    }
}

/// Convert a ProductReleaseInfo instance into a UniqueReleaseInfo instance,
/// to better suit the format needed by customers of `ffx product get-version`.
pub fn from_product_release_info(
    info: &ProductReleaseInfo,
    slot: &Option<Slot>,
) -> UniqueReleaseInfo {
    UniqueReleaseInfo {
        name: info.info.name.clone(),
        version: info.info.version.clone(),
        repository: info.info.repository.clone(),
        slot: if let Some(s) = slot { vec![s.clone()] } else { vec![] },
    }
}

/// Struct holding a vector of UniqueReleaseInfo elements.
///
/// This is used to modify the serialization / deserialization of a vector
/// of elements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UniqueReleaseInfoVector(pub Vec<UniqueReleaseInfo>);

// Custom wrapper for conditional serialization/deserialization

impl Serialize for UniqueReleaseInfoVector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.0.len() == 1 {
            // If there's exactly one item, serialize the item directly
            self.0[0].serialize(serializer)
        } else {
            // Otherwise, serialize the entire vector
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for UniqueReleaseInfoVector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // First, deserialize into a serde_json::Value. This consumes the
        // deserializer once, but we can then try to deserialize from the Value
        // multiple times without consuming it.
        let value = Value::deserialize(deserializer)?;

        // Try to deserialize as a single UniqueReleaseInfo object
        if let Ok(item) = UniqueReleaseInfo::deserialize(&value) {
            return Ok(UniqueReleaseInfoVector(vec![item]));
        }

        // If that fails, try to deserialize as a Vec<UniqueReleaseInfo>
        if let Ok(vec) = Vec::<UniqueReleaseInfo>::deserialize(&value) {
            return Ok(UniqueReleaseInfoVector(vec));
        }

        // If neither conversion works, return an error
        Err(D::Error::custom(
            "Expected either a single ReleaseInfo object or an array of ReleaseInfo objects",
        ))
    }
}
