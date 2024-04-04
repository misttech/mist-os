// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        bedrock::program,
        model::{events::error::EventsError, storage::StorageError},
    },
    ::routing::{
        error::{ComponentInstanceError, RoutingError},
        policy::PolicyError,
        resolving::ResolverError,
    },
    bedrock_error::{BedrockError, Explain},
    clonable_error::ClonableError,
    cm_config::CompatibilityCheckError,
    cm_moniker::{InstancedExtendedMoniker, InstancedMoniker},
    cm_rust::UseDecl,
    cm_types::Name,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx,
    moniker::{ChildName, Moniker, MonikerError},
    sandbox::ConversionError,
    std::sync::Arc,
    thiserror::Error,
};

/// Errors produced by `Model`.
#[derive(Debug, Error, Clone)]
pub enum ModelError {
    #[error("bad path")]
    BadPath,
    #[error("Moniker error: {}", err)]
    MonikerError {
        #[from]
        err: MonikerError,
    },
    #[error("expected a component instance moniker")]
    UnexpectedComponentManagerMoniker,
    #[error("Routing error: {}", err)]
    RoutingError {
        #[from]
        err: RoutingError,
    },
    #[error(
        "Failed to open path `{}`, in storage directory for `{}` backed by `{}`: {}",
        path,
        moniker,
        source_moniker,
        err
    )]
    OpenStorageFailed {
        source_moniker: InstancedExtendedMoniker,
        moniker: InstancedMoniker,
        path: String,
        #[source]
        err: zx::Status,
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
    #[error("error in service dir VFS for component {moniker}: {err}")]
    ServiceDirError {
        moniker: Moniker,

        #[source]
        err: VfsError,
    },
    #[error("failed to open directory '{}' for component '{}'", relative_path, moniker)]
    OpenDirectoryError { moniker: Moniker, relative_path: String },
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
        err: component_id_index::IndexError,
    },
    #[error("error with action: {}", err)]
    ActionError {
        #[from]
        err: ActionError,
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
    BedrockError {
        #[from]
        err: BedrockError,
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
    pub fn instance_not_found(moniker: Moniker) -> ModelError {
        ModelError::from(ComponentInstanceError::instance_not_found(moniker))
    }

    pub fn open_directory_error(moniker: Moniker, relative_path: impl Into<String>) -> ModelError {
        ModelError::OpenDirectoryError { moniker, relative_path: relative_path.into() }
    }
}

impl Explain for ModelError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            ModelError::RoutingError { err } => err.as_zx_status(),
            ModelError::PolicyError { err } => err.as_zx_status(),
            ModelError::StartActionError { err } => err.as_zx_status(),
            ModelError::ComponentInstanceError { err } => err.as_zx_status(),
            ModelError::OpenOutgoingDirError { err } => err.as_zx_status(),
            ModelError::BedrockError { err } => err.as_zx_status(),
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
    #[error("Failed to match values for key: {}", key)]
    ValueMismatch { key: String },
    #[error("Failed to route structured config values: {_0}")]
    RoutingError(#[from] RoutingError),
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
    #[error("instance is not resolved")]
    InstanceNotResolved,
    #[error("instance was destroyed")]
    InstanceDestroyed,
    #[error("open error: {0}")]
    Open(#[from] zx::Status),
}

impl Explain for OpenExposedDirError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::InstanceNotResolved => zx::Status::NOT_FOUND,
            Self::InstanceDestroyed => zx::Status::NOT_FOUND,
            Self::Open(status) => *status,
        }
    }
}

#[derive(Clone, Debug, Error)]
pub enum OpenOutgoingDirError {
    #[error("instance is not resolved")]
    InstanceNotResolved,
    #[error("instance is non-executable")]
    InstanceNonExecutable,
    #[error("open error: {0}")]
    Open(#[from] zx::Status),
}

impl Explain for OpenOutgoingDirError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::InstanceNotResolved => zx::Status::NOT_FOUND,
            Self::InstanceNonExecutable => zx::Status::NOT_FOUND,
            Self::Open(err) => *err,
        }
    }
}

impl From<OpenOutgoingDirError> for fsys::OpenError {
    fn from(value: OpenOutgoingDirError) -> Self {
        match value {
            OpenOutgoingDirError::InstanceNotResolved => fsys::OpenError::InstanceNotResolved,
            OpenOutgoingDirError::InstanceNonExecutable => fsys::OpenError::NoSuchDir,
            OpenOutgoingDirError::Open(_) => fsys::OpenError::FidlError,
        }
    }
}

impl From<OpenOutgoingDirError> for BedrockError {
    fn from(value: OpenOutgoingDirError) -> Self {
        BedrockError::OpenError(Arc::new(value))
    }
}

#[derive(Debug, Error, Clone)]
pub enum AddDynamicChildError {
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
    #[error("action failed on child: {}", err)]
    ActionError {
        #[from]
        err: ActionError,
    },
    #[error("invalid dictionary")]
    InvalidDictionary,
    #[error(
        "dictionary entry for capability '{capability_name}' conflicts with existing static route"
    )]
    StaticRouteConflict { capability_name: Name },
    #[error("failed to add child to parent: {}", err)]
    AddChildError {
        #[from]
        err: AddChildError,
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
            AddDynamicChildError::AddChildError {
                err: AddChildError::InstanceAlreadyExists { .. },
            } => fcomponent::Error::InstanceAlreadyExists,
            AddDynamicChildError::DynamicOffersNotAllowed { .. } => {
                fcomponent::Error::InvalidArguments
            }
            AddDynamicChildError::ActionError { err } => err.into(),
            AddDynamicChildError::InvalidDictionary { .. } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::StaticRouteConflict { .. } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::NameTooLong { .. } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::AddChildError {
                err: AddChildError::DynamicOfferError { .. },
            } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::AddChildError {
                err: AddChildError::DynamicConfigError { .. },
            } => fcomponent::Error::InvalidArguments,
            AddDynamicChildError::AddChildError { err: AddChildError::ChildNameInvalid { .. } } => {
                fcomponent::Error::InvalidArguments
            }
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
            AddDynamicChildError::AddChildError {
                err: AddChildError::InstanceAlreadyExists { .. },
            } => fsys::CreateError::InstanceAlreadyExists,

            AddDynamicChildError::DynamicOffersNotAllowed { .. } => {
                fsys::CreateError::DynamicOffersForbidden
            }
            AddDynamicChildError::ActionError { .. } => fsys::CreateError::Internal,
            AddDynamicChildError::InvalidDictionary { .. } => fsys::CreateError::Internal,
            AddDynamicChildError::StaticRouteConflict { .. } => fsys::CreateError::Internal,
            AddDynamicChildError::NameTooLong { .. } => fsys::CreateError::BadChildDecl,
            AddDynamicChildError::AddChildError {
                err: AddChildError::DynamicOfferError { .. },
            } => fsys::CreateError::BadDynamicOffer,
            AddDynamicChildError::AddChildError {
                err: AddChildError::DynamicConfigError { .. },
            } => fsys::CreateError::BadDynamicOffer,
            AddDynamicChildError::AddChildError { err: AddChildError::ChildNameInvalid { .. } } => {
                fsys::CreateError::BadMoniker
            }
            AddDynamicChildError::NumberedHandleNotInSingleRunCollection => {
                fsys::CreateError::NumberedHandlesForbidden
            }
        }
    }
}

#[derive(Debug, Error, Clone)]
pub enum AddChildError {
    #[error("component instance {} in realm {} already exists", child, moniker)]
    InstanceAlreadyExists { moniker: Moniker, child: ChildName },
    #[error("dynamic offer error: {}", err)]
    DynamicOfferError {
        #[from]
        err: DynamicOfferError,
    },
    #[error("dynamic config error: {}", err)]
    DynamicConfigError {
        #[from]
        err: cm_fidl_validator::error::ErrorList,
    },
    #[error("child moniker not valid: {}", err)]
    ChildNameInvalid {
        #[from]
        err: MonikerError,
    },
}

#[derive(Debug, Error, Clone, PartialEq)]
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
pub enum ActionError {
    #[error("discover action error: {}", err)]
    DiscoverError {
        #[from]
        err: DiscoverActionError,
    },

    #[error("resolve action error: {}", err)]
    ResolveError {
        #[from]
        err: ResolveActionError,
    },

    #[error("unresolve action error: {}", err)]
    UnresolveError {
        #[from]
        err: UnresolveActionError,
    },

    #[error("start action error: {}", err)]
    StartError {
        #[from]
        err: StartActionError,
    },

    #[error("stop action error: {}", err)]
    StopError {
        #[from]
        err: StopActionError,
    },

    #[error("destroy action error: {}", err)]
    DestroyError {
        #[from]
        err: DestroyActionError,
    },
}

impl Explain for ActionError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            ActionError::DiscoverError { .. } => zx::Status::INTERNAL,
            ActionError::ResolveError { err } => err.as_zx_status(),
            ActionError::UnresolveError { .. } => zx::Status::INTERNAL,
            ActionError::StartError { err } => err.as_zx_status(),
            ActionError::StopError { .. } => zx::Status::INTERNAL,
            ActionError::DestroyError { .. } => zx::Status::INTERNAL,
        }
    }
}

impl From<ActionError> for BedrockError {
    fn from(value: ActionError) -> Self {
        BedrockError::LifecycleError(Arc::new(value))
    }
}

impl From<ActionError> for fcomponent::Error {
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::DiscoverError { .. } => fcomponent::Error::Internal,
            ActionError::ResolveError { .. } => fcomponent::Error::Internal,
            ActionError::UnresolveError { .. } => fcomponent::Error::Internal,
            ActionError::StartError { err } => err.into(),
            ActionError::StopError { err } => err.into(),
            ActionError::DestroyError { err } => err.into(),
        }
    }
}

impl From<ActionError> for fsys::ResolveError {
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::ResolveError { err } => err.into(),
            _ => fsys::ResolveError::Internal,
        }
    }
}

impl From<ActionError> for fsys::UnresolveError {
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::UnresolveError { err } => err.into(),
            _ => fsys::UnresolveError::Internal,
        }
    }
}

impl From<ActionError> for fsys::StartError {
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::StartError { err } => err.into(),
            _ => fsys::StartError::Internal,
        }
    }
}

impl From<ActionError> for fsys::StopError {
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::StopError { err } => err.into(),
            _ => fsys::StopError::Internal,
        }
    }
}

impl From<ActionError> for fsys::DestroyError {
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::DestroyError { err } => err.into(),
            _ => fsys::DestroyError::Internal,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum DiscoverActionError {
    #[error("instance {moniker} was destroyed")]
    InstanceDestroyed { moniker: Moniker },
}

#[derive(Debug, Clone, Error)]
pub enum ResolveActionError {
    #[error("discover action failed: {err}")]
    DiscoverActionError {
        #[from]
        err: DiscoverActionError,
    },
    #[error("instance {moniker} was shut down")]
    InstanceShutDown { moniker: Moniker },
    #[error("instance {moniker} was destroyed")]
    InstanceDestroyed { moniker: Moniker },
    #[error(
        "component address could not parsed for moniker '{}' at url '{}': {}",
        moniker,
        url,
        err
    )]
    ComponentAddressParseError {
        url: String,
        moniker: Moniker,
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
    // TODO(https://fxbug.dev/42071713): Determine whether this is expected to fail.
    ExposeDirError {
        moniker: Moniker,

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
    #[error("ABI compatibility check failed for {url}: {err}")]
    AbiCompatibilityError {
        url: String,
        #[source]
        err: CompatibilityCheckError,
    },
    #[error(transparent)]
    Policy(#[from] PolicyError),
}

impl ResolveActionError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            ResolveActionError::DiscoverActionError { .. }
            | ResolveActionError::InstanceShutDown { .. }
            | ResolveActionError::InstanceDestroyed { .. }
            | ResolveActionError::ComponentAddressParseError { .. }
            | ResolveActionError::AbiCompatibilityError { .. } => zx::Status::NOT_FOUND,
            ResolveActionError::ExposeDirError { .. }
            | ResolveActionError::AddStaticChildError { .. }
            | ResolveActionError::StructuredConfigError { .. }
            | ResolveActionError::PackageDirProxyCreateError { .. } => zx::Status::INTERNAL,
            ResolveActionError::ResolverError { err, .. } => err.as_zx_status(),
            ResolveActionError::Policy(err) => err.as_zx_status(),
        }
    }
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
            | ResolveActionError::ResolverError { .. }
            | ResolveActionError::StructuredConfigError { .. }
            | ResolveActionError::ComponentAddressParseError { .. }
            | ResolveActionError::AddStaticChildError { .. }
            | ResolveActionError::DiscoverActionError { .. }
            | ResolveActionError::AbiCompatibilityError { .. }
            | ResolveActionError::PackageDirProxyCreateError { .. } => fsys::ResolveError::Internal,
            ResolveActionError::Policy(_) => fsys::ResolveError::PolicyError,
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
            | ResolveActionError::ResolverError { .. }
            | ResolveActionError::StructuredConfigError { .. }
            | ResolveActionError::ComponentAddressParseError { .. }
            | ResolveActionError::AddStaticChildError { .. }
            | ResolveActionError::DiscoverActionError { .. }
            | ResolveActionError::AbiCompatibilityError { .. }
            | ResolveActionError::PackageDirProxyCreateError { .. } => fsys::StartError::Internal,
            ResolveActionError::Policy(_) => fsys::StartError::PolicyError,
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
        err: zx::Status,
    },
}

impl PkgDirError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::NoPkgDir => zx::Status::NOT_FOUND,
            Self::OpenFailed { err } => *err,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum ComponentProviderError {
    #[error("failed to start source instance: {err}")]
    SourceStartError {
        #[from]
        err: ActionError,
    },
    #[error("failed to open source instance outgoing dir: {err}")]
    OpenOutgoingDirError {
        #[from]
        err: OpenOutgoingDirError,
    },
}

impl ComponentProviderError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::SourceStartError { err } => err.as_zx_status(),
            Self::OpenOutgoingDirError { err } => err.as_zx_status(),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum CapabilityProviderError {
    #[error("bad path")]
    BadPath,
    #[error("component instance")]
    ComponentInstanceError {
        #[from]
        err: ComponentInstanceError,
    },
    #[error("error in pkg dir capability provider: {err}")]
    PkgDirError {
        #[from]
        err: PkgDirError,
    },
    #[error("error in event source capability provider: {0}")]
    EventSourceError(#[from] EventSourceError),
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
    #[error(transparent)]
    BedrockError {
        #[from]
        err: BedrockError,
    },
    #[error("could not route: {0}")]
    RoutingError(#[from] RoutingError),
    #[error("vfs open error")]
    VfsOpenError(#[source] zx::Status),
}

impl CapabilityProviderError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::BadPath => zx::Status::INVALID_ARGS,
            Self::ComponentInstanceError { err } => err.as_zx_status(),
            Self::CmNamespaceError { .. } => zx::Status::INTERNAL,
            Self::PkgDirError { err } => err.as_zx_status(),
            Self::EventSourceError(err) => err.as_zx_status(),
            Self::ComponentProviderError { err } => err.as_zx_status(),
            Self::BedrockError { err } => err.as_zx_status(),
            Self::RoutingError(err) => err.as_zx_status(),
            Self::VfsOpenError(err) => *err,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum OpenError {
    #[error("failed to get default capability provider: {err}")]
    GetDefaultProviderError {
        // TODO(https://fxbug.dev/42068065): This will get fixed when we untangle ModelError
        #[source]
        err: Box<ModelError>,
    },
    #[error("no capability provider found")]
    CapabilityProviderNotFound,
    #[error("capability provider error: {err}")]
    CapabilityProviderError {
        #[from]
        err: CapabilityProviderError,
    },
    #[error("failed to open storage capability: {err}")]
    OpenStorageError {
        // TODO(https://fxbug.dev/42068065): This will get fixed when we untangle ModelError
        #[source]
        err: Box<ModelError>,
    },
    #[error("timed out opening capability")]
    Timeout,
    #[error("the capability does not support opening: {0}")]
    DoesNotSupportOpen(ConversionError),
}

impl Explain for OpenError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::GetDefaultProviderError { err } => err.as_zx_status(),
            Self::OpenStorageError { err } => err.as_zx_status(),
            Self::CapabilityProviderError { err } => err.as_zx_status(),
            Self::CapabilityProviderNotFound => zx::Status::NOT_FOUND,
            Self::Timeout => zx::Status::TIMED_OUT,
            Self::DoesNotSupportOpen(_) => zx::Status::NOT_SUPPORTED,
        }
    }
}

impl From<OpenError> for BedrockError {
    fn from(value: OpenError) -> Self {
        BedrockError::OpenError(Arc::new(value))
    }
}

#[derive(Debug, Clone, Error)]
pub enum StartActionError {
    #[error("Couldn't start `{moniker}` because it has been destroyed")]
    InstanceShutDown { moniker: Moniker },
    #[error("Couldn't start `{moniker}` because it has been destroyed")]
    InstanceDestroyed { moniker: Moniker },
    #[error("Couldn't start `{moniker}` because it couldn't resolve: {err}")]
    ResolveActionError {
        moniker: Moniker,
        #[source]
        err: Box<ActionError>,
    },
    #[error("Couldn't start `{moniker}` because the runner `{runner}` couldn't resolve: {err}")]
    ResolveRunnerError {
        moniker: Moniker,
        runner: Name,
        #[source]
        err: Box<BedrockError>,
    },
    #[error("Couldn't start `{moniker}` because it uses `\"on_terminate\": \"reboot\"` but is not allowed to by policy: {err}")]
    RebootOnTerminateForbidden {
        moniker: Moniker,
        #[source]
        err: PolicyError,
    },
    #[error("Couldn't start `{moniker}` because we failed to create its namespace: {err}")]
    CreateNamespaceError {
        moniker: Moniker,
        #[source]
        err: CreateNamespaceError,
    },
    #[error("Couldn't start `{moniker}` because we failed to start its program: {err}")]
    StartProgramError {
        moniker: Moniker,
        #[source]
        err: program::StartError,
    },
    #[error("Couldn't start `{moniker}` due to a structured configuration error: {err}")]
    StructuredConfigError {
        moniker: Moniker,
        #[source]
        err: StructuredConfigError,
    },
    #[error("Couldn't start `{moniker}` because one of its eager children failed to start: {err}")]
    EagerStartError {
        moniker: Moniker,
        #[source]
        err: Box<ActionError>,
    },
    #[error("Couldn't start `{moniker}` because it is interrupted by a stop request")]
    Aborted { moniker: Moniker },
}

impl StartActionError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            StartActionError::InstanceDestroyed { .. } | Self::InstanceShutDown { .. } => {
                zx::Status::NOT_FOUND
            }
            StartActionError::StartProgramError { .. }
            | StartActionError::StructuredConfigError { .. }
            | StartActionError::EagerStartError { .. } => zx::Status::INTERNAL,
            StartActionError::RebootOnTerminateForbidden { err, .. } => err.as_zx_status(),
            StartActionError::ResolveRunnerError { err, .. } => err.as_zx_status(),
            StartActionError::CreateNamespaceError { err, .. } => err.as_zx_status(),
            StartActionError::ResolveActionError { err, .. } => err.as_zx_status(),
            StartActionError::Aborted { .. } => zx::Status::NOT_FOUND,
        }
    }
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
impl Into<fsys::StartError> for StartActionError {
    fn into(self) -> fsys::StartError {
        match self {
            StartActionError::ResolveActionError { err, .. } => (*err).into(),
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
    #[error("failed to stop program: {0}")]
    ProgramStopError(#[source] program::StopError),
    #[error("failed to get top instance")]
    GetTopInstanceFailed,
    #[error("failed to get parent instance")]
    GetParentFailed,
    #[error("failed to destroy dynamic children: {err}")]
    DestroyDynamicChildrenFailed { err: Box<ActionError> },
    #[error("failed to resolve component: {err}")]
    ResolveActionError {
        #[source]
        err: Box<ActionError>,
    },
    #[error("a component started while shutdown was ongoing")]
    ComponentStartedDuringShutdown,
}

// This is implemented for fuchsia.sys2.LifecycleController protocol.
impl Into<fsys::StopError> for StopActionError {
    fn into(self) -> fsys::StopError {
        fsys::StopError::Internal
    }
}

impl Into<fcomponent::Error> for StopActionError {
    fn into(self) -> fcomponent::Error {
        fcomponent::Error::Internal
    }
}

#[cfg(test)]
impl PartialEq for StopActionError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StopActionError::ProgramStopError(_), StopActionError::ProgramStopError(_)) => true,
            (StopActionError::GetTopInstanceFailed, StopActionError::GetTopInstanceFailed) => true,
            (StopActionError::GetParentFailed, StopActionError::GetParentFailed) => true,
            (
                StopActionError::DestroyDynamicChildrenFailed { .. },
                StopActionError::DestroyDynamicChildrenFailed { .. },
            ) => true,
            (
                StopActionError::ResolveActionError { .. },
                StopActionError::ResolveActionError { .. },
            ) => true,
            _ => false,
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
        err: Box<ActionError>,
    },
    #[error("could not find instance with moniker {}", moniker)]
    InstanceNotFound { moniker: Moniker },
    #[error("instance with moniker {} is not resolved", moniker)]
    InstanceNotResolved { moniker: Moniker },
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
    InstanceRunning { moniker: Moniker },
    #[error("{moniker} was destroyed")]
    InstanceDestroyed { moniker: Moniker },
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
pub enum CreateNamespaceError {
    #[error("failed to clone pkg dir: {0}")]
    ClonePkgDirFailed(#[source] fuchsia_fs::node::CloneError),

    #[error("use decl without path cannot be installed into the namespace: {0:?}")]
    UseDeclWithoutPath(UseDecl),

    #[error("{0}")]
    InstanceNotInInstanceIdIndex(#[source] RoutingError),

    #[error(transparent)]
    BuildNamespaceError(#[from] serve_processargs::BuildNamespaceError),

    #[error("failed to convert namespace into directory")]
    ConvertToDirectory(#[source] ClonableError),

    #[error(transparent)]
    ComponentInstanceError(#[from] ComponentInstanceError),
}

impl CreateNamespaceError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::ClonePkgDirFailed(_) => zx::Status::INTERNAL,
            Self::UseDeclWithoutPath(_) => zx::Status::NOT_FOUND,
            Self::InstanceNotInInstanceIdIndex(e) => e.as_zx_status(),
            Self::BuildNamespaceError(_) => zx::Status::NOT_FOUND,
            Self::ConvertToDirectory(_) => zx::Status::INTERNAL,
            Self::ComponentInstanceError(err) => err.as_zx_status(),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum EventSourceError {
    #[error("component instance error: {0}")]
    ComponentInstance(#[from] ComponentInstanceError),
    #[error("model error: {0}")]
    // TODO(https://fxbug.dev/42068065): This will get fixed when we untangle ModelError
    Model(#[from] Box<ModelError>),
    #[error("event stream already consumed")]
    AlreadyConsumed,
}

impl EventSourceError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::ComponentInstance(err) => err.as_zx_status(),
            Self::Model(err) => err.as_zx_status(),
            Self::AlreadyConsumed => zx::Status::INTERNAL,
        }
    }
}
