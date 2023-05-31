// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{events::error::EventsError, routing::RouteRequest, storage::StorageError},
    },
    ::routing::{
        component_id_index::ComponentIdIndexError,
        config::AbiRevisionError,
        error::{ComponentInstanceError, RoutingError},
        policy::PolicyError,
        resolving::ResolverError,
    },
    clonable_error::ClonableError,
    cm_moniker::{InstancedExtendedMoniker, InstancedRelativeMoniker},
    cm_types::Name,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx,
    moniker::{AbsoluteMoniker, ChildMoniker, MonikerError},
    std::path::PathBuf,
    thiserror::Error,
};

/// Errors produced by `Model`.
#[derive(Debug, Error, Clone)]
pub enum ModelError {
    #[error("component collection not found with name {}", name)]
    CollectionNotFound { name: String },
    // TODO(https://fxbug.dev/117080): Remove this error by using the `camino` library
    #[error("path is not utf-8: {:?}", path)]
    PathIsNotUtf8 { path: PathBuf },
    #[error("Moniker error: {}", err)]
    MonikerError {
        #[from]
        err: MonikerError,
    },
    #[error("expected a component instance moniker")]
    UnexpectedComponentManagerMoniker,
    #[error("The model is not available")]
    ModelNotAvailable,
    #[error("Routing error: {}", err)]
    RoutingError {
        #[from]
        err: RoutingError,
    },
    #[error(
        "Failed to open path `{}`, in storage directory for `{}` backed by `{}`: {}",
        path,
        relative_moniker,
        moniker,
        err
    )]
    OpenStorageFailed {
        moniker: InstancedExtendedMoniker,
        relative_moniker: InstancedRelativeMoniker,
        path: String,
        #[source]
        err: fidl::Error,
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
    #[error("error in collection service dir VFS for component {moniker}: {err}")]
    CollectionServiceDirError {
        moniker: AbsoluteMoniker,

        #[source]
        err: VfsError,
    },
    #[error("failed to open directory '{}' for component '{}'", relative_path, moniker)]
    OpenDirectoryError { moniker: AbsoluteMoniker, relative_path: String },
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
    #[error("error with resolve action: {err}")]
    ResolveActionError {
        #[from]
        err: ResolveActionError,
    },
    #[error("{err}")]
    StartActionError {
        #[from]
        err: StartActionError,
    },
    #[error("failed to open outgoing dir: {err}")]
    OpenOutgoingDirError {
        #[from]
        err: OpenOutgoingDirError,
    },
    #[error(transparent)]
    RouteAndOpenCapabilityError {
        #[from]
        err: RouteAndOpenCapabilityError,
    },
    #[error("error with capability provider: {err}")]
    CapabilityProviderError {
        #[from]
        err: CapabilityProviderError,
    },
    #[error("failed to open capability: {err}")]
    OpenError {
        #[from]
        err: OpenError,
    },
}

impl ModelError {
    pub fn instance_not_found(moniker: AbsoluteMoniker) -> ModelError {
        ModelError::from(ComponentInstanceError::instance_not_found(moniker))
    }

    pub fn path_is_not_utf8(path: PathBuf) -> ModelError {
        ModelError::PathIsNotUtf8 { path }
    }

    pub fn open_directory_error(
        moniker: AbsoluteMoniker,
        relative_path: impl Into<String>,
    ) -> ModelError {
        ModelError::OpenDirectoryError { moniker, relative_path: relative_path.into() }
    }

    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            ModelError::RoutingError { err } => err.as_zx_status(),
            ModelError::PolicyError { err } => err.as_zx_status(),
            ModelError::StartActionError { err } => err.as_zx_status(),
            ModelError::ComponentInstanceError {
                err: ComponentInstanceError::InstanceNotFound { .. },
            } => zx::Status::NOT_FOUND,
            ModelError::OpenOutgoingDirError { err } => err.as_zx_status(),
            ModelError::RouteAndOpenCapabilityError { err } => err.as_zx_status(),
            ModelError::CapabilityProviderError { err } => err.as_zx_status(),
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

#[derive(Clone, Debug, Error)]
pub enum OpenOutgoingDirError {
    #[error("instance is not running")]
    InstanceNotRunning,
    #[error("instance is non-executable")]
    InstanceNonExecutable,
    #[error("open call FIDL error: {0}")]
    Fidl(#[from] fidl::Error),
}

impl OpenOutgoingDirError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::InstanceNotRunning | Self::InstanceNonExecutable => zx::Status::UNAVAILABLE,
            Self::Fidl(_) => zx::Status::INTERNAL,
        }
    }
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

#[derive(Debug, Clone, Error)]
pub enum PkgDirError {
    #[error("no pkg dir found for component")]
    NoPkgDir,
    #[error("error opening pkg dir: {err}")]
    OpenFailed {
        #[from]
        err: fidl::Error,
    },
}

impl PkgDirError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::NoPkgDir => zx::Status::NOT_FOUND,
            Self::OpenFailed { .. } => zx::Status::IO,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum ComponentProviderError {
    #[error("source instance not found")]
    SourceInstanceNotFound,
    #[error("target instance not found")]
    TargetInstanceNotFound,
    #[error("failed to start source instance: {err}")]
    SourceStartError {
        #[from]
        err: StartActionError,
    },
    #[error("failed to open source instance outgoing dir: {err}")]
    OpenOutgoingDirError {
        #[from]
        err: OpenOutgoingDirError,
    },
}

impl ComponentProviderError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::SourceInstanceNotFound | Self::TargetInstanceNotFound => zx::Status::NOT_FOUND,
            Self::SourceStartError { err } => err.as_zx_status(),
            Self::OpenOutgoingDirError { err } => err.as_zx_status(),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum CapabilityProviderError {
    #[error("failed to create stream from channel")]
    StreamCreationError,
    #[error("bad path")]
    BadPath,
    #[error("bad flags")]
    BadFlags,
    #[error("error in pkg dir capability provider: {err}")]
    PkgDirError {
        #[from]
        err: PkgDirError,
    },
    #[error("error in event source capability provider: {err}")]
    EventSourceError {
        #[source]
        err: ComponentInstanceError,
    },
    #[error("error in component capability provider: {err}")]
    ComponentProviderError {
        #[from]
        err: ComponentProviderError,
    },
    #[error("error in component manager namespace capability provider: {err}")]
    CmNamespaceError {
        #[from]
        err: ClonableError,
    },
}

impl CapabilityProviderError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::StreamCreationError => zx::Status::BAD_HANDLE,
            Self::BadFlags | Self::BadPath => zx::Status::INVALID_ARGS,
            Self::PkgDirError { err } => err.as_zx_status(),
            Self::EventSourceError { err } => err.as_zx_status(),
            Self::ComponentProviderError { err } => err.as_zx_status(),
            Self::CmNamespaceError { .. } => zx::Status::INTERNAL,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum OpenError {
    #[error("failed to get default capability provider: {err}")]
    GetDefaultProviderError {
        // TODO(https://fxbug.dev/116855): This will get fixed when we untangle ModelError
        #[source]
        err: Box<ModelError>,
    },
    #[error("source instance not found")]
    SourceInstanceNotFound,
    #[error("no capability provider found")]
    CapabilityProviderNotFound,
    #[error("capability provider error: {err}")]
    CapabilityProviderError {
        #[from]
        err: CapabilityProviderError,
    },
    #[error("failed to open storage capability: {err}")]
    OpenStorageError {
        // TODO(https://fxbug.dev/116855): This will get fixed when we untangle ModelError
        #[source]
        err: Box<ModelError>,
    },
    #[error("timed out opening capability")]
    Timeout,
}

impl OpenError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::GetDefaultProviderError { err } => err.as_zx_status(),
            Self::OpenStorageError { err } => err.as_zx_status(),
            Self::CapabilityProviderError { err } => err.as_zx_status(),
            Self::CapabilityProviderNotFound => zx::Status::NOT_FOUND,
            Self::SourceInstanceNotFound => zx::Status::NOT_FOUND,
            Self::Timeout => zx::Status::TIMED_OUT,
        }
    }
}

/// Describes all errors encountered when routing and opening a namespace capability.
#[derive(Debug, Clone, Error)]
pub enum RouteAndOpenCapabilityError {
    #[error("not a namespace capability")]
    NotNamespaceCapability,
    #[error("could not route {request}: {err}")]
    RoutingError {
        request: RouteRequest,
        #[source]
        err: RoutingError,
    },
    #[error("could not open {source}: {err}")]
    OpenError {
        source: CapabilitySource,
        #[source]
        err: OpenError,
    },
}

impl RouteAndOpenCapabilityError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::NotNamespaceCapability { .. } => zx::Status::INVALID_ARGS,
            Self::RoutingError { err, .. } => err.as_zx_status(),
            Self::OpenError { err, .. } => err.as_zx_status(),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum StartActionError {
    #[error("Couldn't start `{moniker}` because it was shutdown")]
    InstanceShutDown { moniker: AbsoluteMoniker },
    #[error("Couldn't start `{moniker}` because it has been destroyed")]
    InstanceDestroyed { moniker: AbsoluteMoniker },
    #[error("Couldn't start `{moniker}` because it couldn't resolve: {err}")]
    ResolveActionError {
        moniker: AbsoluteMoniker,
        #[source]
        err: ResolveActionError,
    },
    #[error("Couldn't start `{moniker}` because the runner `{runner}` couldn't resolve: {err}")]
    ResolveRunnerError {
        moniker: AbsoluteMoniker,
        runner: Name,
        #[source]
        err: Box<RouteAndOpenCapabilityError>,
    },
    #[error("Couldn't start `{moniker}` because it uses reboot_on_terminate but is not allowed to by policy: {err}")]
    RebootOnTerminateForbidden {
        moniker: AbsoluteMoniker,
        #[source]
        err: PolicyError,
    },
    #[error("Couldn't start `{moniker}` because we failed to populate its namespace: {err}")]
    NamespacePopulateError {
        moniker: AbsoluteMoniker,
        #[source]
        err: NamespacePopulateError,
    },
    #[error("Couldn't start `{moniker}` due to a structured configuration error: {err}")]
    StructuredConfigError {
        moniker: AbsoluteMoniker,
        #[source]
        err: StructuredConfigError,
    },
    #[error("Couldn't start `{moniker}` because one of its eager children failed to start: {err}")]
    EagerStartError {
        moniker: AbsoluteMoniker,
        #[source]
        err: Box<StartActionError>,
    },
}

impl StartActionError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::RebootOnTerminateForbidden { err, .. } => err.as_zx_status(),
            Self::ResolveRunnerError { err, .. } => err.as_zx_status(),
            Self::InstanceDestroyed { .. } | Self::InstanceShutDown { .. } => zx::Status::NOT_FOUND,
            Self::NamespacePopulateError { err, .. } => err.as_zx_status(),
            _ => zx::Status::INTERNAL,
        }
    }
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
impl Into<fsys::StartError> for StartActionError {
    fn into(self) -> fsys::StartError {
        match self {
            StartActionError::ResolveActionError { err, .. } => err.into(),
            StartActionError::InstanceDestroyed { .. } => fsys::StartError::InstanceNotFound,
            StartActionError::InstanceShutDown { .. } => fsys::StartError::InstanceNotFound,
            _ => fsys::StartError::Internal,
        }
    }
}

// This is implemented for fuchsia.component.Realm protocol.
impl Into<fcomponent::Error> for StartActionError {
    fn into(self) -> fcomponent::Error {
        match self {
            StartActionError::ResolveActionError { .. } => fcomponent::Error::InstanceCannotResolve,
            StartActionError::RebootOnTerminateForbidden { .. } => fcomponent::Error::AccessDenied,
            StartActionError::InstanceShutDown { .. } => fcomponent::Error::InstanceDied,
            StartActionError::InstanceDestroyed { .. } => fcomponent::Error::InstanceDied,
            _ => fcomponent::Error::InstanceCannotStart,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum StopActionError {
    #[error("stop failed with unexpected FIDL error ({0})")]
    ControllerStopFidlError(#[source] fidl::Error),
    #[error("kill failed with unexpected FIDL error ({0})")]
    ControllerKillFidlError(#[source] fidl::Error),
    #[error("failed to get top instance")]
    GetTopInstanceFailed,
    #[error("failed to get parent instance")]
    GetParentFailed,
    #[error("failed to destroy dynamic children: {err}")]
    DestroyDynamicChildrenFailed { err: DestroyActionError },
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
impl Into<fsys::StopError> for StopActionError {
    fn into(self) -> fsys::StopError {
        fsys::StopError::Internal
    }
}

#[cfg(test)]
impl PartialEq for StopActionError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                StopActionError::ControllerStopFidlError(_),
                StopActionError::ControllerStopFidlError(_),
            ) => true,
            (
                StopActionError::ControllerKillFidlError(_),
                StopActionError::ControllerKillFidlError(_),
            ) => true,
            (StopActionError::GetTopInstanceFailed, StopActionError::GetTopInstanceFailed) => true,
            (StopActionError::GetParentFailed, StopActionError::GetParentFailed) => true,
            (
                StopActionError::DestroyDynamicChildrenFailed { .. },
                StopActionError::DestroyDynamicChildrenFailed { .. },
            ) => true,
            _ => false,
        }
    }

    fn ne(&self, other: &Self) -> bool {
        match (self, other) {
            (
                StopActionError::ControllerStopFidlError(_),
                StopActionError::ControllerStopFidlError(_),
            ) => false,
            (
                StopActionError::ControllerKillFidlError(_),
                StopActionError::ControllerKillFidlError(_),
            ) => false,
            (StopActionError::GetTopInstanceFailed, StopActionError::GetTopInstanceFailed) => false,
            (StopActionError::GetParentFailed, StopActionError::GetParentFailed) => false,
            (
                StopActionError::DestroyDynamicChildrenFailed { .. },
                StopActionError::DestroyDynamicChildrenFailed { .. },
            ) => false,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum DestroyActionError {
    #[error("failed to discover component: {}", err)]
    DiscoverActionError {
        #[from]
        err: DiscoverActionError,
    },
    #[error("failed to shutdown component: {}", err)]
    ShutdownFailed {
        #[source]
        err: Box<StopActionError>,
    },
    #[error("could not find instance with moniker {}", moniker)]
    InstanceNotFound { moniker: AbsoluteMoniker },
    #[error("instance with moniker {} is not resolved", moniker)]
    InstanceNotResolved { moniker: AbsoluteMoniker },
}

// This is implemented for fuchsia.component.Realm protocol.
impl Into<fcomponent::Error> for DestroyActionError {
    fn into(self) -> fcomponent::Error {
        match self {
            DestroyActionError::InstanceNotFound { .. } => fcomponent::Error::InstanceNotFound,
            _ => fcomponent::Error::Internal,
        }
    }
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
impl Into<fsys::DestroyError> for DestroyActionError {
    fn into(self) -> fsys::DestroyError {
        match self {
            DestroyActionError::InstanceNotFound { .. } => fsys::DestroyError::InstanceNotFound,
            _ => fsys::DestroyError::Internal,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum UnresolveActionError {
    #[error("failed to shutdown component: {}", err)]
    ShutdownFailed {
        #[from]
        err: StopActionError,
    },
    #[error("{moniker} cannot be unresolved while it is running")]
    InstanceRunning { moniker: AbsoluteMoniker },
    #[error("{moniker} was destroyed")]
    InstanceDestroyed { moniker: AbsoluteMoniker },
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
impl Into<fsys::UnresolveError> for UnresolveActionError {
    fn into(self) -> fsys::UnresolveError {
        match self {
            UnresolveActionError::InstanceDestroyed { .. } => {
                fsys::UnresolveError::InstanceNotFound
            }
            _ => fsys::UnresolveError::Internal,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum NamespacePopulateError {
    #[error("failed to clone pkg dir: {0}")]
    ClonePkgDirFailed(#[source] ClonableError),

    #[error("{0}")]
    InstanceNotInInstanceIdIndex(#[source] RoutingError),
}

impl NamespacePopulateError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::ClonePkgDirFailed(_) => zx::Status::IO,
            Self::InstanceNotInInstanceIdIndex(e) => e.as_zx_status(),
        }
    }
}
