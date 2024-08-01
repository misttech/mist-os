// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_developer_ffx::{RepositoryRegistrationAliasConflictMode, RepositoryStorageType};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod config;
mod instance;
pub mod metrics;
pub mod repo;
mod tunnel;

pub use instance::{
    write_instance_info, PathType, PkgServerInfo, PkgServerInstanceInfo, PkgServerInstances,
    ServerMode,
};

/// Type of registration of the repository on the target device.
/// This mirrors the fidl type RepositoryStorageType, since
/// we need JSON serialization.
///
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub enum RepoStorageType {
    Ephemeral,
    Persistent,
}

impl From<RepositoryStorageType> for RepoStorageType {
    fn from(value: RepositoryStorageType) -> Self {
        match value {
            RepositoryStorageType::Ephemeral => RepoStorageType::Ephemeral,
            RepositoryStorageType::Persistent => RepoStorageType::Persistent,
        }
    }
}

/// How  conflicts in the registration of the repository on the target device will
/// be resolved.
/// This mirrors the fidl type RepositoryRegistrationAliasConflictMode, since
/// we need JSON serialization.
///
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub enum RegistrationConflictMode {
    ErrorOut,
    Replace,
}

impl From<RepositoryRegistrationAliasConflictMode> for RegistrationConflictMode {
    fn from(value: RepositoryRegistrationAliasConflictMode) -> Self {
        match value {
            RepositoryRegistrationAliasConflictMode::ErrorOut => RegistrationConflictMode::ErrorOut,
            RepositoryRegistrationAliasConflictMode::Replace => RegistrationConflictMode::Replace,
        }
    }
}
