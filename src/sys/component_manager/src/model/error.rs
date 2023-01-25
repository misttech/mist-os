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
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx,
    moniker::{AbsoluteMoniker, ChildMoniker, MonikerError},
    std::path::PathBuf,
    thiserror::Error,
};

/// Errors produced by `Model`.
#[derive(Debug, Error, Clone)]
pub enum ModelError {
    #[error("component instance with moniker {} has shut down", moniker)]
    InstanceShutDown { moniker: AbsoluteMoniker },
    #[error("component instance with moniker {} is being destroyed", moniker)]
    InstanceDestroyed { moniker: AbsoluteMoniker },
    #[error("component collection not found with name {}", name)]
    CollectionNotFound { name: String },
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
    #[error("structured config error: {}", err)]
    StructuredConfigError {
        #[from]
        err: StructuredConfigError,
    },
    #[error("timed out after {:?}", duration)]
    Timeout { duration: zx::Duration },
    #[error("error with discover action: {err}")]
    DiscoverActionError {
        #[from]
        err: DiscoverActionError,
    },
    #[error("error with resolve action: {err}")]
    ResolveActionError {
        #[from]
        err: ResolveActionError,
    },
}

impl ModelError {
    pub fn instance_shut_down(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::InstanceShutDown { moniker }
    }

    pub fn instance_destroyed(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::InstanceDestroyed { moniker }
    }

    pub fn instance_not_found(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::from(ComponentInstanceError::instance_not_found(moniker))
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

#[derive(Debug, Error)]
pub enum RebootError {
    #[error("failed to connect to admin protocol in root component's exposed dir: {0}")]
    ConnectToAdminFailed(#[source] anyhow::Error),
    #[error("StateControl Admin protocol encountered FIDL error: {0}")]
    FidlError(#[from] fidl::Error),
    #[error("StateControl Admin responded with status: {0}")]
    AdminError(zx::Status),
    #[error("failed to open root component's exposed dir: {0}")]
    OpenRootExposedDirFailed(#[from] OpenExposedDirError),
}

#[derive(Debug, Error)]
pub enum OpenExposedDirError {
    #[error("instance was destroyed")]
    InstanceDestroyed,
}

#[derive(Debug, Error, Clone)]
pub enum AddDynamicChildError {
    #[error("dynamic children cannot have eager startup")]
    EagerStartupUnsupported,
    #[error("component collection not found with name {}", name)]
    CollectionNotFound { name: String },
    #[error(
        "numbered handles can only be provided when adding components to a single-run collection"
    )]
    NumberedHandleNotInSingleRunCollection,
    #[error("name length is longer than the allowed max {}", max_len)]
    NameTooLong { max_len: usize },
    #[error("collection {} does not allow dynamic offers", collection_name)]
    DynamicOffersNotAllowed { collection_name: String },
    #[error("failed to add child to parent: {}", err)]
    AddChildError {
        #[from]
        err: AddChildError,
    },
    #[error("failed to discover child: {}", err)]
    DiscoverActionError {
        #[from]
        err: DiscoverActionError,
    },
    #[error("failed to resolve parent: {}", err)]
    ResolveActionError {
        #[from]
        err: ResolveActionError,
    },
}

// This is implemented for fuchsia.component.Realm protocol
impl Into<fcomponent::Error> for AddDynamicChildError {
    fn into(self) -> fcomponent::Error {
        match self {
            AddDynamicChildError::CollectionNotFound { .. } => {
                fcomponent::Error::CollectionNotFound
            }
            AddDynamicChildError::NumberedHandleNotInSingleRunCollection => {
                fcomponent::Error::Unsupported
            }
            AddDynamicChildError::EagerStartupUnsupported => fcomponent::Error::Unsupported,
            AddDynamicChildError::AddChildError {
                err: AddChildError::InstanceAlreadyExists { .. },
            } => fcomponent::Error::InstanceAlreadyExists,

            // Invalid Arguments
            AddDynamicChildError::DynamicOffersNotAllowed { .. } => {
                fcomponent::Error::InvalidArguments
            }
            AddDynamicChildError::NameTooLong { .. } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::AddChildError {
                err: AddChildError::DynamicOfferError { .. },
            } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::AddChildError {
                err: AddChildError::ChildMonikerInvalid { .. },
            } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::DiscoverActionError { .. } => fcomponent::Error::Internal,
            AddDynamicChildError::ResolveActionError { .. } => fcomponent::Error::Internal,
        }
    }
}

// This is implemented for fuchsia.sys2.LifecycleController protocol
impl Into<fsys::CreateError> for AddDynamicChildError {
    fn into(self) -> fsys::CreateError {
        match self {
            AddDynamicChildError::CollectionNotFound { .. } => {
                fsys::CreateError::CollectionNotFound
            }
            AddDynamicChildError::DiscoverActionError { .. } => fsys::CreateError::Internal,
            AddDynamicChildError::ResolveActionError { .. } => fsys::CreateError::Internal,
            AddDynamicChildError::EagerStartupUnsupported => {
                fsys::CreateError::EagerStartupForbidden
            }
            AddDynamicChildError::AddChildError {
                err: AddChildError::InstanceAlreadyExists { .. },
            } => fsys::CreateError::InstanceAlreadyExists,

            AddDynamicChildError::DynamicOffersNotAllowed { .. } => {
                fsys::CreateError::DynamicOffersForbidden
            }
            AddDynamicChildError::NameTooLong { .. } => fsys::CreateError::BadChildDecl,
            AddDynamicChildError::AddChildError {
                err: AddChildError::DynamicOfferError { .. },
            } => fsys::CreateError::BadDynamicOffer,
            AddDynamicChildError::AddChildError {
                err: AddChildError::ChildMonikerInvalid { .. },
            } => fsys::CreateError::BadMoniker,
            AddDynamicChildError::NumberedHandleNotInSingleRunCollection => {
                fsys::CreateError::NumberedHandlesForbidden
            }
        }
    }
}

#[derive(Debug, Error, Clone)]
pub enum AddChildError {
    #[error("component instance {} in realm {} already exists", child, moniker)]
    InstanceAlreadyExists { moniker: AbsoluteMoniker, child: ChildMoniker },
    #[error("dynamic offer error: {}", err)]
    DynamicOfferError {
        #[from]
        err: DynamicOfferError,
    },
    #[error("child moniker not valid: {}", err)]
    ChildMonikerInvalid {
        #[from]
        err: MonikerError,
    },
}

#[derive(Debug, Error, Clone)]
pub enum DynamicOfferError {
    #[error("dynamic offer not valid: {}", err)]
    OfferInvalid {
        #[from]
        err: cm_fidl_validator::error::ErrorList,
    },
    #[error("source for dynamic offer not found: {:?}", offer)]
    SourceNotFound { offer: cm_rust::OfferDecl },
    #[error("unknown offer type in dynamic offers")]
    UnknownOfferType,
}

#[derive(Debug, Clone, Error)]
pub enum DiscoverActionError {
    #[error("instance {moniker} was destroyed")]
    InstanceDestroyed { moniker: AbsoluteMoniker },
}

#[derive(Debug, Clone, Error)]
pub enum ResolveActionError {
    #[error("discover action failed: {err}")]
    DiscoverActionError {
        #[from]
        err: DiscoverActionError,
    },
    #[error("instance {moniker} was shut down")]
    InstanceShutDown { moniker: AbsoluteMoniker },
    #[error("instance {moniker} was destroyed")]
    InstanceDestroyed { moniker: AbsoluteMoniker },
    #[error(
        "component address could not parsed for moniker '{}' at url '{}': {}",
        moniker,
        url,
        err
    )]
    ComponentAddressParseError {
        url: String,
        moniker: AbsoluteMoniker,
        #[source]
        err: ResolverError,
    },
    #[error("resolver error for \"{}\": {}", url, err)]
    ResolverError {
        url: String,
        #[source]
        err: ResolverError,
    },
    #[error("error in expose dir VFS for component {moniker}: {err}")]
    // TODO(https://fxbug.dev/120627): Determine whether this is expected to fail.
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
    #[error("could not add static child \"{}\": {}", child_name, err)]
    AddStaticChildError {
        child_name: String,
        #[source]
        err: AddChildError,
    },
    #[error("structured config error: {}", err)]
    StructuredConfigError {
        #[from]
        err: StructuredConfigError,
    },
    #[error("package dir proxy creation failed: {}", err)]
    PackageDirProxyCreateError {
        #[source]
        err: fidl::Error,
    },
    #[error("ABI compatibility check failed: {}", err)]
    AbiCompatibilityError {
        #[from]
        err: AbiRevisionError,
    },
}

// This is implemented for fuchsia.sys2.LifecycleController protocol
impl Into<fsys::ResolveError> for ResolveActionError {
    fn into(self) -> fsys::ResolveError {
        match self {
            ResolveActionError::ResolverError {
                err: ResolverError::PackageNotFound(_), ..
            } => fsys::ResolveError::PackageNotFound,
            ResolveActionError::ResolverError {
                err: ResolverError::ManifestNotFound(_), ..
            } => fsys::ResolveError::ManifestNotFound,
            ResolveActionError::InstanceShutDown { .. }
            | ResolveActionError::InstanceDestroyed { .. } => fsys::ResolveError::InstanceNotFound,
            ResolveActionError::ExposeDirError { .. }
            | ResolveActionError::NamespaceDirError { .. }
            | ResolveActionError::ResolverError { .. }
            | ResolveActionError::StructuredConfigError { .. }
            | ResolveActionError::ComponentAddressParseError { .. }
            | ResolveActionError::AddStaticChildError { .. }
            | ResolveActionError::DiscoverActionError { .. }
            | ResolveActionError::AbiCompatibilityError { .. }
            | ResolveActionError::PackageDirProxyCreateError { .. } => fsys::ResolveError::Internal,
        }
    }
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
// Starting a component instance also causes a resolve.
impl Into<fsys::StartError> for ResolveActionError {
    fn into(self) -> fsys::StartError {
        match self {
            ResolveActionError::ResolverError {
                err: ResolverError::PackageNotFound(_), ..
            } => fsys::StartError::PackageNotFound,
            ResolveActionError::ResolverError {
                err: ResolverError::ManifestNotFound(_), ..
            } => fsys::StartError::ManifestNotFound,
            ResolveActionError::InstanceShutDown { .. }
            | ResolveActionError::InstanceDestroyed { .. } => fsys::StartError::InstanceNotFound,
            ResolveActionError::ExposeDirError { .. }
            | ResolveActionError::NamespaceDirError { .. }
            | ResolveActionError::ResolverError { .. }
            | ResolveActionError::StructuredConfigError { .. }
            | ResolveActionError::ComponentAddressParseError { .. }
            | ResolveActionError::AddStaticChildError { .. }
            | ResolveActionError::DiscoverActionError { .. }
            | ResolveActionError::AbiCompatibilityError { .. }
            | ResolveActionError::PackageDirProxyCreateError { .. } => fsys::StartError::Internal,
        }
    }
}
