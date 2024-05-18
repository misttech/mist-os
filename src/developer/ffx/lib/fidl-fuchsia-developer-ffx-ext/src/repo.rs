// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    camino::Utf8PathBuf,
    fidl_fuchsia_developer_ffx as fidl,
    fidl_fuchsia_net_ext::SocketAddress,
    serde::{Deserialize, Serialize},
    std::{collections::BTreeSet, net::SocketAddr},
    thiserror::Error,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RepositorySpec {
    FileSystem {
        metadata_repo_path: Utf8PathBuf,
        blob_repo_path: Utf8PathBuf,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        aliases: BTreeSet<String>,
    },

    Pm {
        path: Utf8PathBuf,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        aliases: BTreeSet<String>,
    },

    Http {
        metadata_repo_url: String,
        blob_repo_url: String,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        aliases: BTreeSet<String>,
    },

    Gcs {
        metadata_repo_url: String,
        blob_repo_url: String,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        aliases: BTreeSet<String>,
    },
}

impl TryFrom<fidl::RepositorySpec> for RepositorySpec {
    type Error = RepositoryError;

    fn try_from(repo: fidl::RepositorySpec) -> Result<Self, RepositoryError> {
        match repo {
            fidl::RepositorySpec::FileSystem(spec) => Ok(RepositorySpec::FileSystem {
                metadata_repo_path: spec
                    .metadata_repo_path
                    .ok_or(RepositoryError::MissingRepositorySpecField)?
                    .into(),
                blob_repo_path: spec
                    .blob_repo_path
                    .ok_or(RepositoryError::MissingRepositorySpecField)?
                    .into(),
                aliases: spec
                    .aliases
                    .map(|aliases| aliases.into_iter().collect())
                    .unwrap_or_default(),
            }),
            fidl::RepositorySpec::Pm(spec) => Ok(RepositorySpec::Pm {
                path: spec.path.ok_or(RepositoryError::MissingRepositorySpecField)?.into(),
                aliases: spec
                    .aliases
                    .map(|aliases| aliases.into_iter().collect())
                    .unwrap_or_default(),
            }),
            fidl::RepositorySpec::Http(spec) => Ok(RepositorySpec::Http {
                metadata_repo_url: spec
                    .metadata_repo_url
                    .ok_or(RepositoryError::MissingRepositorySpecField)?
                    .into(),
                blob_repo_url: spec
                    .blob_repo_url
                    .ok_or(RepositoryError::MissingRepositorySpecField)?
                    .into(),
                aliases: spec
                    .aliases
                    .map(|aliases| aliases.into_iter().collect())
                    .unwrap_or_default(),
            }),
            fidl::RepositorySpec::Gcs(spec) => Ok(RepositorySpec::Gcs {
                metadata_repo_url: spec
                    .metadata_repo_url
                    .ok_or(RepositoryError::MissingRepositorySpecField)?
                    .into(),
                blob_repo_url: spec
                    .blob_repo_url
                    .ok_or(RepositoryError::MissingRepositorySpecField)?
                    .into(),
                aliases: spec
                    .aliases
                    .map(|aliases| aliases.into_iter().collect())
                    .unwrap_or_default(),
            }),
            fidl::RepositorySpecUnknown!() => Err(RepositoryError::UnknownRepositorySpec),
        }
    }
}

impl From<RepositorySpec> for fidl::RepositorySpec {
    fn from(repo: RepositorySpec) -> Self {
        match repo {
            RepositorySpec::FileSystem { metadata_repo_path, blob_repo_path, aliases } => {
                let metadata_repo_path = metadata_repo_path.into_string();
                let blob_repo_path = blob_repo_path.into_string();
                fidl::RepositorySpec::FileSystem(fidl::FileSystemRepositorySpec {
                    metadata_repo_path: Some(metadata_repo_path),
                    blob_repo_path: Some(blob_repo_path),
                    aliases: if aliases.is_empty() {
                        None
                    } else {
                        Some(aliases.into_iter().collect())
                    },
                    ..Default::default()
                })
            }
            RepositorySpec::Pm { path, aliases } => {
                let path = path.into_string();
                fidl::RepositorySpec::Pm(fidl::PmRepositorySpec {
                    path: Some(path),
                    aliases: if aliases.is_empty() {
                        None
                    } else {
                        Some(aliases.into_iter().collect())
                    },
                    ..Default::default()
                })
            }
            RepositorySpec::Http { metadata_repo_url, blob_repo_url, aliases } => {
                fidl::RepositorySpec::Http(fidl::HttpRepositorySpec {
                    metadata_repo_url: Some(metadata_repo_url),
                    blob_repo_url: Some(blob_repo_url),
                    aliases: if aliases.is_empty() {
                        None
                    } else {
                        Some(aliases.into_iter().collect())
                    },
                    ..Default::default()
                })
            }
            RepositorySpec::Gcs { metadata_repo_url, blob_repo_url, aliases } => {
                fidl::RepositorySpec::Gcs(fidl::GcsRepositorySpec {
                    metadata_repo_url: Some(metadata_repo_url),
                    blob_repo_url: Some(blob_repo_url),
                    aliases: if aliases.is_empty() {
                        None
                    } else {
                        Some(aliases.into_iter().collect())
                    },
                    ..Default::default()
                })
            }
        }
    }
}

impl From<RepositorySpec> for fuchsia_repo::repository::RepositorySpec {
    fn from(repo: RepositorySpec) -> Self {
        match repo {
            RepositorySpec::FileSystem { metadata_repo_path, blob_repo_path, aliases } => {
                Self::FileSystem { metadata_repo_path, blob_repo_path, aliases }
            }
            RepositorySpec::Pm { path, aliases } => Self::Pm { path, aliases },
            RepositorySpec::Http { metadata_repo_url, blob_repo_url, aliases } => {
                Self::Http { metadata_repo_url, blob_repo_url, aliases }
            }
            RepositorySpec::Gcs { metadata_repo_url, blob_repo_url, aliases } => {
                Self::Gcs { metadata_repo_url, blob_repo_url, aliases }
            }
        }
    }
}

impl From<fuchsia_repo::repository::RepositorySpec> for RepositorySpec {
    fn from(repo: fuchsia_repo::repository::RepositorySpec) -> Self {
        match repo {
            fuchsia_repo::repository::RepositorySpec::FileSystem {
                metadata_repo_path,
                blob_repo_path,
                aliases,
            } => RepositorySpec::FileSystem { metadata_repo_path, blob_repo_path, aliases },
            fuchsia_repo::repository::RepositorySpec::Pm { path, aliases } => {
                RepositorySpec::Pm { path, aliases }
            }
            fuchsia_repo::repository::RepositorySpec::Http {
                metadata_repo_url,
                blob_repo_url,
                aliases,
            } => RepositorySpec::Http { metadata_repo_url, blob_repo_url, aliases },
            fuchsia_repo::repository::RepositorySpec::Gcs {
                metadata_repo_url,
                blob_repo_url,
                aliases,
            } => RepositorySpec::Gcs { metadata_repo_url, blob_repo_url, aliases },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryStorageType {
    Ephemeral,
    Persistent,
}

impl From<fidl::RepositoryStorageType> for RepositoryStorageType {
    fn from(storage_type: fidl::RepositoryStorageType) -> Self {
        match storage_type {
            fidl::RepositoryStorageType::Ephemeral => RepositoryStorageType::Ephemeral,
            fidl::RepositoryStorageType::Persistent => RepositoryStorageType::Persistent,
        }
    }
}

impl From<RepositoryStorageType> for fidl::RepositoryStorageType {
    fn from(storage_type: RepositoryStorageType) -> Self {
        match storage_type {
            RepositoryStorageType::Ephemeral => fidl::RepositoryStorageType::Ephemeral,
            RepositoryStorageType::Persistent => fidl::RepositoryStorageType::Persistent,
        }
    }
}

impl From<RepositoryStorageType> for fidl_fuchsia_pkg_ext::RepositoryStorageType {
    fn from(storage_type: RepositoryStorageType) -> Self {
        match storage_type {
            RepositoryStorageType::Ephemeral => {
                fidl_fuchsia_pkg_ext::RepositoryStorageType::Ephemeral
            }
            RepositoryStorageType::Persistent => {
                fidl_fuchsia_pkg_ext::RepositoryStorageType::Persistent
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryRegistrationAliasConflictMode {
    ErrorOut,
    Replace,
}

impl From<fidl::RepositoryRegistrationAliasConflictMode>
    for RepositoryRegistrationAliasConflictMode
{
    fn from(alias_conflict_mode: fidl::RepositoryRegistrationAliasConflictMode) -> Self {
        match alias_conflict_mode {
            fidl::RepositoryRegistrationAliasConflictMode::ErrorOut => {
                RepositoryRegistrationAliasConflictMode::ErrorOut
            }
            fidl::RepositoryRegistrationAliasConflictMode::Replace => {
                RepositoryRegistrationAliasConflictMode::Replace
            }
        }
    }
}

impl From<RepositoryRegistrationAliasConflictMode>
    for fidl::RepositoryRegistrationAliasConflictMode
{
    fn from(alias_conflict_mode: RepositoryRegistrationAliasConflictMode) -> Self {
        match alias_conflict_mode {
            RepositoryRegistrationAliasConflictMode::ErrorOut => {
                fidl::RepositoryRegistrationAliasConflictMode::ErrorOut
            }
            RepositoryRegistrationAliasConflictMode::Replace => {
                fidl::RepositoryRegistrationAliasConflictMode::Replace
            }
        }
    }
}

impl From<RepositoryRegistrationAliasConflictMode>
    for fidl_fuchsia_pkg_ext::RepositoryRegistrationAliasConflictMode
{
    fn from(alias_conflict_mode: RepositoryRegistrationAliasConflictMode) -> Self {
        match alias_conflict_mode {
            RepositoryRegistrationAliasConflictMode::ErrorOut => {
                fidl_fuchsia_pkg_ext::RepositoryRegistrationAliasConflictMode::ErrorOut
            }
            RepositoryRegistrationAliasConflictMode::Replace => {
                fidl_fuchsia_pkg_ext::RepositoryRegistrationAliasConflictMode::Replace
            }
        }
    }
}

/// The below types exist to provide definitions with Serialize.
/// TODO(https://fxbug.dev/42155902) They should be removed in favor of the
/// corresponding fidl-fuchsia-pkg-ext types.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepositoryConfig {
    pub name: String,
    pub spec: RepositorySpec,
}

impl TryFrom<fidl::RepositoryConfig> for RepositoryConfig {
    type Error = RepositoryError;

    fn try_from(repo_config: fidl::RepositoryConfig) -> Result<Self, Self::Error> {
        Ok(RepositoryConfig { name: repo_config.name, spec: repo_config.spec.try_into()? })
    }
}

impl From<RepositoryConfig> for fidl::RepositoryConfig {
    fn from(repo_config: RepositoryConfig) -> Self {
        fidl::RepositoryConfig { name: repo_config.name, spec: repo_config.spec.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryTarget {
    pub repo_name: String,
    pub target_identifier: Option<String>,
    pub aliases: Option<BTreeSet<String>>,
    pub storage_type: Option<RepositoryStorageType>,
}

impl TryFrom<fidl::RepositoryTarget> for RepositoryTarget {
    type Error = RepositoryError;

    fn try_from(repo_target: fidl::RepositoryTarget) -> Result<Self, Self::Error> {
        Ok(RepositoryTarget {
            repo_name: repo_target.repo_name.ok_or(RepositoryError::MissingRepositoryName)?,
            target_identifier: repo_target.target_identifier,
            aliases: repo_target.aliases.map(|aliases| aliases.into_iter().collect()),
            storage_type: repo_target.storage_type.map(|storage_type| storage_type.into()),
        })
    }
}

impl From<RepositoryTarget> for fidl::RepositoryTarget {
    fn from(repo_target: RepositoryTarget) -> Self {
        fidl::RepositoryTarget {
            repo_name: Some(repo_target.repo_name),
            target_identifier: repo_target.target_identifier,
            aliases: repo_target.aliases.map(|aliases| aliases.into_iter().collect()),
            storage_type: repo_target.storage_type.map(|storage_type| storage_type.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ServerStatus {
    Disabled,
    Stopped,
    Running { address: SocketAddr },
}

impl TryFrom<fidl::ServerStatus> for ServerStatus {
    type Error = RepositoryError;

    fn try_from(status: fidl::ServerStatus) -> Result<Self, Self::Error> {
        match status {
            fidl::ServerStatus::Disabled(fidl::Disabled {}) => Ok(Self::Disabled),
            fidl::ServerStatus::Stopped(fidl::Stopped {}) => Ok(Self::Stopped),
            fidl::ServerStatus::Running(fidl::Running { address }) => {
                Ok(Self::Running { address: SocketAddress::from(address).0 })
            }
        }
    }
}

impl From<ServerStatus> for fidl::ServerStatus {
    fn from(status: ServerStatus) -> Self {
        match status {
            ServerStatus::Disabled => Self::Disabled(fidl::Disabled),
            ServerStatus::Stopped => Self::Stopped(fidl::Stopped),
            ServerStatus::Running { address } => {
                Self::Running(fidl::Running { address: SocketAddress(address).into() })
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("the repository name is missing")]
    MissingRepositoryName,

    #[error("repository does not exist")]
    NoMatchingRepository,

    #[error("error communicating with target device")]
    TargetCommunicationFailure,

    #[error("error interacting with the target's RepositoryManager")]
    RepositoryManagerError,

    #[error("error iteracting with the target's RewriteEngine")]
    RewriteEngineError,

    #[error("unknown repository spec type")]
    UnknownRepositorySpec,

    #[error("repository spec is missing a required field")]
    MissingRepositorySpecField,

    #[error("some unspecified error during I/O")]
    IoError,

    #[error("some unspecified internal error")]
    InternalError,

    #[error("repository metadata is expired")]
    ExpiredRepositoryMetadata,

    #[error("repository registration does not exist")]
    NoMatchingRegistration,

    #[error("repository server is not running")]
    ServerNotRunning,

    #[error("invalid url")]
    InvalidUrl,

    #[error("repository server address already in use")]
    ServerAddressAlreadyInUse,

    #[error("package does not exist")]
    NoMatchingPackage,

    #[error("repository registration conflict")]
    ConflictingRegistration,
}

impl From<fidl::RepositoryError> for RepositoryError {
    fn from(err: fidl::RepositoryError) -> Self {
        use {fidl::RepositoryError as fErr, RepositoryError as Err};
        match err {
            fErr::MissingRepositoryName => Err::MissingRepositoryName,
            fErr::NoMatchingRepository => Err::NoMatchingRepository,
            fErr::TargetCommunicationFailure => Err::TargetCommunicationFailure,
            fErr::RepositoryManagerError => Err::RepositoryManagerError,
            fErr::RewriteEngineError => Err::RewriteEngineError,
            fErr::UnknownRepositorySpec => Err::UnknownRepositorySpec,
            fErr::MissingRepositorySpecField => Err::MissingRepositorySpecField,
            fErr::IoError => Err::IoError,
            fErr::InternalError => Err::InternalError,
            fErr::ExpiredRepositoryMetadata => Err::ExpiredRepositoryMetadata,
            fErr::NoMatchingRegistration => Err::NoMatchingRegistration,
            fErr::ServerNotRunning => Err::ServerNotRunning,
            fErr::InvalidUrl => Err::InvalidUrl,
            fErr::ServerAddressAlreadyInUse => Err::ServerAddressAlreadyInUse,
            fErr::NoMatchingPackage => Err::NoMatchingPackage,
            fErr::ConflictingRegistration => Err::ConflictingRegistration,
        }
    }
}

impl From<RepositoryError> for fidl::RepositoryError {
    fn from(err: RepositoryError) -> Self {
        use {fidl::RepositoryError as fErr, RepositoryError as Err};
        match err {
            Err::MissingRepositoryName => fErr::MissingRepositoryName,
            Err::NoMatchingRepository => fErr::NoMatchingRepository,
            Err::TargetCommunicationFailure => fErr::TargetCommunicationFailure,
            Err::RepositoryManagerError => fErr::RepositoryManagerError,
            Err::RewriteEngineError => fErr::RewriteEngineError,
            Err::UnknownRepositorySpec => fErr::UnknownRepositorySpec,
            Err::MissingRepositorySpecField => fErr::MissingRepositorySpecField,
            Err::IoError => fErr::IoError,
            Err::InternalError => fErr::InternalError,
            Err::ExpiredRepositoryMetadata => fErr::ExpiredRepositoryMetadata,
            Err::NoMatchingRegistration => fErr::NoMatchingRegistration,
            Err::ServerNotRunning => fErr::ServerNotRunning,
            Err::InvalidUrl => fErr::InvalidUrl,
            Err::ServerAddressAlreadyInUse => fErr::ServerAddressAlreadyInUse,
            Err::NoMatchingPackage => fErr::NoMatchingPackage,
            Err::ConflictingRegistration => fErr::ConflictingRegistration,
        }
    }
}
