// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_component_decl as fcomponent_decl,
    fidl_fuchsia_component_resolution as fcomponent_resolution, fidl_fuchsia_pkg as fpkg,
};

pub mod component;
pub mod context_authenticator;
pub mod package;

#[derive(thiserror::Error, Debug)]
pub(crate) enum ResolverError {
    #[error("invalid component URL")]
    InvalidUrl(#[from] fuchsia_url::errors::ParseError),

    #[error("component URL with package hash not supported")]
    PackageHashNotSupported,

    #[error("component not found")]
    ComponentNotFound(#[source] mem_util::FileError),

    #[error("couldn't parse component manifest")]
    ParsingManifest(#[source] fidl::Error),

    #[error("couldn't find config values")]
    ConfigValuesNotFound(#[source] mem_util::FileError),

    #[error("config source missing or invalid")]
    InvalidConfigSource,

    #[error("unsupported config source: {0:?}")]
    UnsupportedConfigSource(fcomponent_decl::ConfigValueSource),

    #[error("failed to read the manifest")]
    ReadManifest(#[source] mem_util::DataError),

    #[error("serve package directory")]
    ServePackageDirectory(#[source] package_directory::Error),

    #[error("context must be empty when resolving absolute URL")]
    ContextWithAbsoluteUrl,

    #[error("subpackage name was not found in the package's subpackage list")]
    SubpackageNotFound,

    #[error("failed to read abi revision")]
    AbiRevision(#[source] fidl_fuchsia_component_abi_ext::AbiRevisionFileError),

    #[error("the package URL was not found in the base package index")]
    PackageNotInBase(fuchsia_url::AbsolutePackageUrl),

    #[error("failed to read the superpackage's subpackage manifest")]
    ReadingSubpackageManifest(#[from] package_directory::SubpackagesError),

    #[error("invalid context")]
    InvalidContext(#[from] context_authenticator::ContextAuthenticatorError),

    #[error("resolve must be called with an absolute (not relative) url")]
    AbsoluteUrlRequired,

    #[error("failed to convert proxy to channel")]
    ConvertProxyToChannel,

    #[error(
        "package directory for {superpackage} was not open when resolving subpackage {subpackage}"
    )]
    SuperpackageNotOpen {
        superpackage: fuchsia_hash::Hash,
        subpackage: fuchsia_url::RelativePackageUrl,
    },
}

impl From<&ResolverError> for fcomponent_resolution::ResolverError {
    fn from(err: &ResolverError) -> fcomponent_resolution::ResolverError {
        use fcomponent_resolution::ResolverError as ferror;
        use ResolverError::*;
        match err {
            InvalidUrl(_)
            | PackageHashNotSupported
            | InvalidContext(_)
            | AbsoluteUrlRequired
            | ContextWithAbsoluteUrl => ferror::InvalidArgs,
            ComponentNotFound(_) => ferror::ManifestNotFound,
            ConfigValuesNotFound(_) => ferror::ConfigValuesNotFound,
            ParsingManifest(_) | UnsupportedConfigSource(_) | InvalidConfigSource => {
                ferror::InvalidManifest
            }
            ReadManifest(_) | ServePackageDirectory(_) | ReadingSubpackageManifest(_) => ferror::Io,
            ConvertProxyToChannel | SuperpackageNotOpen { .. } => ferror::Internal,
            SubpackageNotFound | PackageNotInBase(_) => ferror::PackageNotFound,
            AbiRevision(_) => ferror::InvalidAbiRevision,
        }
    }
}

impl From<ResolverError> for fcomponent_resolution::ResolverError {
    fn from(err: ResolverError) -> fcomponent_resolution::ResolverError {
        (&err).into()
    }
}

impl From<&ResolverError> for fpkg::ResolveError {
    fn from(err: &ResolverError) -> fpkg::ResolveError {
        use fpkg::ResolveError as ferror;
        use ResolverError::*;
        match err {
            InvalidUrl(_) | PackageHashNotSupported | AbsoluteUrlRequired => ferror::InvalidUrl,
            ComponentNotFound(_)
            | ConfigValuesNotFound(_)
            | AbiRevision(_)
            | ParsingManifest(_)
            | UnsupportedConfigSource(_)
            | InvalidConfigSource
            | ConvertProxyToChannel
            | SuperpackageNotOpen { .. } => ferror::Internal,
            ReadManifest(_) | ServePackageDirectory(_) | ReadingSubpackageManifest(_) => ferror::Io,
            PackageNotInBase(_) | SubpackageNotFound => ferror::PackageNotFound,
            ContextWithAbsoluteUrl | InvalidContext(_) => ferror::InvalidContext,
        }
    }
}
