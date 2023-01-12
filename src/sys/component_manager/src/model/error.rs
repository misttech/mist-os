// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::StopComponentError, events::error::EventsError, routing::OpenResourceError,
        storage::StorageError,
    },
    ::routing::{
        component_id_index::ComponentIdIndexError,
        config::AbiRevisionError,
        error::{ComponentInstanceError, RoutingError},
        policy::PolicyError,
        resolving::ResolverError,
    },
    anyhow::Error,
    clonable_error::ClonableError,
    fuchsia_zircon as zx,
    moniker::{AbsoluteMoniker, ChildMoniker, MonikerError},
    std::path::PathBuf,
    thiserror::Error,
};

/// Errors produced by `Model`.
#[derive(Debug, Error, Clone)]
pub enum ModelError {
    #[error("component instance {} in realm {} already exists", child, moniker)]
    InstanceAlreadyExists { moniker: AbsoluteMoniker, child: ChildMoniker },
    #[error("component instance with moniker {} has shut down", moniker)]
    InstanceShutDown { moniker: AbsoluteMoniker },
    #[error("component instance with moniker {} is being destroyed", moniker)]
    InstanceDestroyed { moniker: AbsoluteMoniker },
    #[error("component collection not found with name {}", name)]
    CollectionNotFound { name: String },
    #[error("collection {} does not allow dynamic offers", collection_name)]
    DynamicOffersNotAllowed { collection_name: String },
    #[error("dynamic offer not valid: {}", err)]
    DynamicOfferInvalid {
        #[source]
        err: cm_fidl_validator::error::ErrorList,
    },
    #[error("source for dynamic offer not found: {:?}", offer)]
    DynamicOfferSourceNotFound { offer: cm_rust::OfferDecl },
    #[error("name length is longer than the allowed max {}", max_len)]
    NameTooLong { max_len: usize },
    #[error(
        "component address could not be computed for component '{}' at url '{}': {:#?}",
        moniker,
        url,
        err
    )]
    ComponentAddressNotAvailable {
        url: String,
        moniker: AbsoluteMoniker,
        #[source]
        err: ResolverError,
    },
    #[error("{} is not supported", feature)]
    Unsupported { feature: String },
    #[error("package directory handle missing")]
    PackageDirectoryMissing,
    // TODO(https://fxbug.dev/117080): Remove this error by using the `camino` library
    #[error("path is not utf-8: {:?}", path)]
    PathIsNotUtf8 { path: PathBuf },
    #[error("path is not valid: {:?}", path)]
    PathInvalid { path: String },
    #[error("Moniker error: {}", err)]
    MonikerError {
        #[from]
        err: MonikerError,
    },
    #[error("expected a component instance moniker")]
    UnexpectedComponentManagerMoniker,
    #[error("The model is not available")]
    ModelNotAvailable,
    #[error("Namespace creation failed: {}", err)]
    NamespaceCreationFailed {
        #[source]
        err: ClonableError,
    },
    #[error("calling Admin/Reboot failed: {}", err)]
    RebootFailed {
        #[source]
        err: ClonableError,
    },
    #[error("failed to resolve \"{}\": {}", url, err)]
    ResolverError {
        url: String,
        #[source]
        err: ResolverError,
    },
    #[error("Routing error: {}", err)]
    RoutingError {
        #[from]
        err: RoutingError,
    },
    #[error("Failed to open resource: {}", err)]
    OpenResourceError {
        #[from]
        err: OpenResourceError,
    },
    #[error("storage error: {}", err)]
    StorageError {
        #[from]
        err: StorageError,
    },
    #[error("component instance error: {}", err)]
    ComponentInstanceError {
        #[from]
        err: ComponentInstanceError,
    },
    #[error("error in expose dir VFS for component {moniker}: {err}")]
    ExposeDirError {
        moniker: AbsoluteMoniker,

        #[source]
        err: VfsError,
    },
    #[error("error in namespace dir VFS for component {moniker}: {err}")]
    NamespaceDirError {
        moniker: AbsoluteMoniker,

        #[source]
        err: VfsError,
    },
    #[error("error in hub dir VFS for component {moniker}: {err}")]
    HubDirError {
        moniker: AbsoluteMoniker,

        #[source]
        err: VfsError,
    },
    #[error("error in collection service dir VFS for component {moniker}: {err}")]
    CollectionServiceDirError {
        moniker: AbsoluteMoniker,

        #[source]
        err: VfsError,
    },
    #[error("failed to open directory '{}' for component '{}'", relative_path, moniker)]
    OpenDirectoryError { moniker: AbsoluteMoniker, relative_path: String },
    #[error("failed to create stream from channel")]
    StreamCreationError {
        #[source]
        err: fidl::Error,
    },
    #[error("failed to stop component {}: {}", moniker, err)]
    StopComponentError {
        moniker: AbsoluteMoniker,
        #[source]
        err: StopComponentError,
    },
    #[error("events error: {}", err)]
    EventsError {
        #[from]
        err: EventsError,
    },
    #[error("policy error: {}", err)]
    PolicyError {
        #[from]
        err: PolicyError,
    },
    #[error("component id index error: {}", err)]
    ComponentIdIndexError {
        #[from]
        err: ComponentIdIndexError,
    },
    #[error("component ABI error: {}", err)]
    AbiRevisionError {
        #[from]
        err: AbiRevisionError,
    },
    #[error("structured config error: {}", err)]
    StructuredConfigError {
        #[from]
        err: StructuredConfigError,
    },
    #[error("timed out after {:?}", duration)]
    Timeout { duration: zx::Duration },
}

impl ModelError {
    pub fn instance_already_exists(moniker: AbsoluteMoniker, child: ChildMoniker) -> ModelError {
        ModelError::InstanceAlreadyExists { moniker, child }
    }

    pub fn instance_shut_down(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::InstanceShutDown { moniker }
    }

    pub fn instance_destroyed(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::InstanceDestroyed { moniker }
    }

    pub fn instance_not_found(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::from(ComponentInstanceError::instance_not_found(moniker))
    }

    pub fn collection_not_found(name: impl Into<String>) -> ModelError {
        ModelError::CollectionNotFound { name: name.into() }
    }

    pub fn dynamic_offers_not_allowed(collection_name: impl Into<String>) -> ModelError {
        ModelError::DynamicOffersNotAllowed { collection_name: collection_name.into() }
    }

    pub fn dynamic_offer_invalid(err: cm_fidl_validator::error::ErrorList) -> ModelError {
        ModelError::DynamicOfferInvalid { err }
    }

    pub fn dynamic_offer_source_not_found(offer: cm_rust::OfferDecl) -> ModelError {
        ModelError::DynamicOfferSourceNotFound { offer }
    }

    pub fn name_too_long(max_len: usize) -> ModelError {
        ModelError::NameTooLong { max_len }
    }

    pub fn unsupported(feature: impl Into<String>) -> ModelError {
        ModelError::Unsupported { feature: feature.into() }
    }

    pub fn path_is_not_utf8(path: PathBuf) -> ModelError {
        ModelError::PathIsNotUtf8 { path }
    }

    pub fn path_invalid(path: impl Into<String>) -> ModelError {
        ModelError::PathInvalid { path: path.into() }
    }

    pub fn namespace_creation_failed(err: impl Into<Error>) -> ModelError {
        ModelError::NamespaceCreationFailed { err: err.into().into() }
    }

    pub fn reboot_failed(err: impl Into<Error>) -> ModelError {
        ModelError::RebootFailed { err: err.into().into() }
    }

    pub fn open_directory_error(
        moniker: AbsoluteMoniker,
        relative_path: impl Into<String>,
    ) -> ModelError {
        ModelError::OpenDirectoryError { moniker, relative_path: relative_path.into() }
    }

    pub fn stream_creation_error(err: fidl::Error) -> ModelError {
        ModelError::StreamCreationError { err }
    }

    pub fn timeout(duration: zx::Duration) -> ModelError {
        ModelError::Timeout { duration }
    }

    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            ModelError::RoutingError { err } => err.as_zx_status(),
            ModelError::PolicyError { err } => err.as_zx_status(),
            ModelError::ComponentInstanceError {
                err: ComponentInstanceError::InstanceNotFound { .. },
            } => zx::Status::NOT_FOUND,
            ModelError::Unsupported { .. } => zx::Status::NOT_SUPPORTED,
            ModelError::Timeout { .. } => zx::Status::TIMED_OUT,
            // Any other type of error is not expected.
            _ => zx::Status::INTERNAL,
        }
    }
}

#[derive(Debug, Error, Clone)]
pub enum StructuredConfigError {
    #[error("component has a config schema but resolver did not provide values")]
    ConfigValuesMissing,
    #[error("failed to resolve component's config: {_0}")]
    ConfigResolutionFailed(#[source] config_encoder::ResolutionError),
    #[error("couldn't create vmo: {_0}")]
    VmoCreateFailed(#[source] zx::Status),
    #[error("couldn't write to vmo: {_0}")]
    VmoWriteFailed(#[source] zx::Status),
}

#[derive(Clone, Debug, Error)]
pub enum VfsError {
    #[error("failed to add node \"{name}\": {status}")]
    AddNodeError { name: String, status: zx::Status },
    #[error("failed to remove node \"{name}\": {status}")]
    RemoveNodeError { name: String, status: zx::Status },
}
