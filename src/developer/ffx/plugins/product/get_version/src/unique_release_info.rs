// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! UniqueReleaseInfo defines a struct for holding release information for assenbly input
//! artifacts, and is used to construct the output of `ffx product get-version`.
//!
//! This file represents a contract between ffx, MOS, and fuchsia infrastructure. Changes to this
//! file must be implemented using soft transitions.

use assembly_partitions_config::Slot;
use assembly_release_info::ReleaseInfo;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// Struct holding release information (name, repository, version, etc..) for a given assembly
/// input artifact.
#[derive(Clone, Debug, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UniqueReleaseInfo {
    /// The name of this assembly artifact.
    pub name: String,

    /// Release version for this assembly artifact.
    pub version: String,

    /// Origin where this release artifact was created.
    pub repository: Option<String>,

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

/// Convert a ReleaseInfo instance into a UniqueReleaseInfo instance, to better suit the format
/// needed by customers of `ffx product get-version`.
pub fn from_release_info(info: &ReleaseInfo, slot: &Slot) -> UniqueReleaseInfo {
    UniqueReleaseInfo {
        name: info.name.clone(),
        version: info.version.clone(),
        repository: Some(info.repository.clone()),
        slot: vec![slot.clone()],
    }
}
