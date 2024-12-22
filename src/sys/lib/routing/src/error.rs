// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::policy::PolicyError;
use crate::rights::Rights;
use async_trait::async_trait;
use clonable_error::ClonableError;
use cm_rust::{CapabilityTypeName, ExposeDeclCommon, OfferDeclCommon, SourceName, UseDeclCommon};
use cm_types::Name;
use moniker::{ChildName, ExtendedMoniker, Moniker};
use router_error::{DowncastErrorForTest, Explain, RouterError};
use std::sync::Arc;
use thiserror::Error;
use {fidl_fuchsia_component as fcomponent, zx_status as zx};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Errors produced by `ComponentInstanceInterface`.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone)]
pub enum ComponentInstanceError {
    #[error("could not find `{moniker}`")]
    InstanceNotFound { moniker: Moniker },
    #[error("component manager instance unavailable")]
    ComponentManagerInstanceUnavailable {},
    #[error("expected a component instance, but got component manager's instance")]
    ComponentManagerInstanceUnexpected {},
    #[error("malformed url `{url}` for `{moniker}`")]
    MalformedUrl { url: String, moniker: Moniker },
    #[error("url `{url}` for `{moniker}` does not resolve to an absolute url")]
    NoAbsoluteUrl { url: String, moniker: Moniker },
    // The capability routing static analyzer never produces this error subtype, so we don't need
    // to serialize it.
    #[cfg_attr(feature = "serde", serde(skip))]
    #[error("failed to resolve `{moniker}`:\n\t{err}")]
    ResolveFailed {
        moniker: Moniker,
        #[source]
        err: ClonableError,
    },
}

impl ComponentInstanceError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            ComponentInstanceError::ResolveFailed { .. }
            | ComponentInstanceError::InstanceNotFound { .. }
            | ComponentInstanceError::ComponentManagerInstanceUnavailable {}
            | ComponentInstanceError::NoAbsoluteUrl { .. } => zx::Status::NOT_FOUND,
            ComponentInstanceError::MalformedUrl { .. }
            | ComponentInstanceError::ComponentManagerInstanceUnexpected { .. } => {
                zx::Status::INTERNAL
            }
        }
    }

    pub fn instance_not_found(moniker: Moniker) -> ComponentInstanceError {
        ComponentInstanceError::InstanceNotFound { moniker }
    }

    pub fn cm_instance_unavailable() -> ComponentInstanceError {
        ComponentInstanceError::ComponentManagerInstanceUnavailable {}
    }

    pub fn resolve_failed(moniker: Moniker, err: impl Into<anyhow::Error>) -> Self {
        Self::ResolveFailed { moniker, err: err.into().into() }
    }
}

impl Explain for ComponentInstanceError {
    fn as_zx_status(&self) -> zx::Status {
        self.as_zx_status()
    }
}

impl From<ComponentInstanceError> for ExtendedMoniker {
    fn from(err: ComponentInstanceError) -> ExtendedMoniker {
        match err {
            ComponentInstanceError::InstanceNotFound { moniker }
            | ComponentInstanceError::MalformedUrl { moniker, .. }
            | ComponentInstanceError::NoAbsoluteUrl { moniker, .. }
            | ComponentInstanceError::ResolveFailed { moniker, .. } => {
                ExtendedMoniker::ComponentInstance(moniker)
            }
            ComponentInstanceError::ComponentManagerInstanceUnavailable {}
            | ComponentInstanceError::ComponentManagerInstanceUnexpected {} => {
                ExtendedMoniker::ComponentManager
            }
        }
    }
}

// Custom implementation of PartialEq in which two ComponentInstanceError::ResolveFailed errors are
// never equal.
impl PartialEq for ComponentInstanceError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::InstanceNotFound { moniker: self_moniker },
                Self::InstanceNotFound { moniker: other_moniker },
            ) => self_moniker.eq(other_moniker),
            (
                Self::ComponentManagerInstanceUnavailable {},
                Self::ComponentManagerInstanceUnavailable {},
            ) => true,
            (Self::ResolveFailed { .. }, Self::ResolveFailed { .. }) => false,
            _ => false,
        }
    }
}

/// Errors produced during routing.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum RoutingError {
    #[error(
        "backing directory `{capability_id}` was not exposed to `{moniker}` from `#{child_moniker}`"
    )]
    StorageFromChildExposeNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_id: String,
    },

    #[error(
        "`{target_moniker}` tried to use a storage capability from `{source_moniker}` but it is \
        not in the component id index.\n\t\
        See: https://fuchsia.dev/go/components/instance-id"
    )]
    ComponentNotInIdIndex { source_moniker: Moniker, target_moniker: Moniker },

    #[error("`{capability_id}` is not a built-in capability")]
    UseFromComponentManagerNotFound { capability_id: String },

    #[error("`{capability_id}` is not a built-in capability")]
    RegisterFromComponentManagerNotFound { capability_id: String },

    #[error("`{capability_id}` is not a built-in capability")]
    OfferFromComponentManagerNotFound { capability_id: String },

    #[error("`{capability_id}` was not offered to `{moniker}` by parent")]
    UseFromParentNotFound { moniker: Moniker, capability_id: String },

    #[error("`{capability_id}` was not declared as a capability by `{moniker}`")]
    UseFromSelfNotFound { moniker: Moniker, capability_id: String },

    #[error("`{moniker}` does not have child `#{child_moniker}`")]
    UseFromChildInstanceNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_id: String,
    },

    #[error(
        "{capability_type} `{capability_name}` was not registered in environment of `{moniker}`"
    )]
    UseFromEnvironmentNotFound { moniker: Moniker, capability_type: String, capability_name: Name },

    #[error(
        "`{moniker}` tried to use {capability_type} `{capability_name}` from the root environment"
    )]
    UseFromRootEnvironmentNotAllowed {
        moniker: Moniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error("{capability_type} `{capability_name}` was not offered to `{moniker}` by parent")]
    EnvironmentFromParentNotFound {
        moniker: Moniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error("`{capability_name}` was not exposed to `{moniker}` from `#{child_moniker}`")]
    EnvironmentFromChildExposeNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error("`{moniker}` does not have child `#{child_moniker}`")]
    EnvironmentFromChildInstanceNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_name: Name,
        capability_type: String,
    },

    #[error("`{capability_id}` was not offered to `{moniker}` by parent")]
    OfferFromParentNotFound { moniker: Moniker, capability_id: String },

    #[error(
        "cannot offer `{capability_id}` because was not declared as a capability by `{moniker}`"
    )]
    OfferFromSelfNotFound { moniker: Moniker, capability_id: String },

    #[error("`{capability_id}` was not offered to `{moniker}` by parent")]
    StorageFromParentNotFound { moniker: Moniker, capability_id: String },

    #[error("`{moniker}` does not have child `#{child_moniker}`")]
    OfferFromChildInstanceNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_id: String,
    },

    #[error("`{moniker}` does not have collection `#{collection}`")]
    OfferFromCollectionNotFound { collection: String, moniker: Moniker, capability: Name },

    #[error("`{capability_id}` was not exposed to `{moniker}` from `#{child_moniker}`")]
    OfferFromChildExposeNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_id: String,
    },

    // TODO: Could this be distinguished by use/offer/expose?
    #[error("`{capability_id}` is not a framework capability (at component `{moniker}`)")]
    CapabilityFromFrameworkNotFound { moniker: Moniker, capability_id: String },

    #[error(
        "A capability was sourced to a base capability `{capability_id}` from `{moniker}`, but this is unsupported",
    )]
    CapabilityFromCapabilityNotFound { moniker: Moniker, capability_id: String },

    // TODO: Could this be distinguished by use/offer/expose?
    #[error("`{capability_id}` is not a framework capability")]
    CapabilityFromComponentManagerNotFound { capability_id: String },

    #[error(
        "unable to expose `{capability_id}` because it was not declared as a capability by `{moniker}`"
    )]
    ExposeFromSelfNotFound { moniker: Moniker, capability_id: String },

    #[error("`{moniker}` does not have child `#{child_moniker}`")]
    ExposeFromChildInstanceNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_id: String,
    },

    #[error("`{moniker}` does not have collection `#{collection}`")]
    ExposeFromCollectionNotFound { collection: String, moniker: Moniker, capability: Name },

    #[error("`{capability_id}` was not exposed to `{moniker}` from `#{child_moniker}`")]
    ExposeFromChildExposeNotFound {
        child_moniker: ChildName,
        moniker: Moniker,
        capability_id: String,
    },

    #[error(
        "`{moniker}` tried to expose `{capability_id}` from the framework, but no such framework capability was found"
    )]
    ExposeFromFrameworkNotFound { moniker: Moniker, capability_id: String },

    #[error("`{capability_id}` was not exposed to `{moniker}` from `#{child_moniker}`")]
    UseFromChildExposeNotFound { child_moniker: ChildName, moniker: Moniker, capability_id: String },

    #[error("routing a capability from an unsupported source type `{source_type}` at `{moniker}`")]
    UnsupportedRouteSource { source_type: String, moniker: ExtendedMoniker },

    #[error("routing a capability of an unsupported type `{type_name}` at `{moniker}`")]
    UnsupportedCapabilityType { type_name: CapabilityTypeName, moniker: ExtendedMoniker },

    #[error(
        "dictionaries are not yet supported for {cap_type} capabilities at component `{moniker}`"
    )]
    DictionariesNotSupported { moniker: Moniker, cap_type: CapabilityTypeName },

    #[error("dynamic dictionaries are not allowed at component `{moniker}`")]
    DynamicDictionariesNotAllowed { moniker: Moniker },

    #[error("the capability does not support member access at `{moniker}`")]
    BedrockMemberAccessUnsupported { moniker: ExtendedMoniker },

    #[error("item `{name}` is not present in dictionary at component `{moniker}`")]
    BedrockNotPresentInDictionary { name: String, moniker: ExtendedMoniker },

    #[error("routed capability was the wrong type at component `{moniker}`. Was: {actual}, expected: {expected}")]
    BedrockWrongCapabilityType { actual: String, expected: String, moniker: ExtendedMoniker },

    #[error("there was an error remoting a capability at component `{moniker}`")]
    BedrockRemoteCapability { moniker: Moniker },

    #[error("source dictionary was not found in child's exposes at component `{moniker}`")]
    BedrockSourceDictionaryExposeNotFound { moniker: Moniker },

    #[error("Some capability in the routing chain could not be cloned at `{moniker}`.")]
    BedrockNotCloneable { moniker: ExtendedMoniker },

    #[error(
        "a capability in a dictionary extended from a source dictionary collides with \
        a capability in the source dictionary that has the same key at `{moniker}`"
    )]
    BedrockSourceDictionaryCollision { moniker: ExtendedMoniker },

    #[error("failed to send message for capability `{capability_id}` from component `{moniker}`")]
    BedrockFailedToSend { moniker: ExtendedMoniker, capability_id: String },

    #[error("failed to route capability because the route source has been shutdown and possibly destroyed")]
    RouteSourceShutdown { moniker: Moniker },

    #[error(transparent)]
    ComponentInstanceError(#[from] ComponentInstanceError),

    #[error(transparent)]
    EventsRoutingError(#[from] EventsRoutingError),

    #[error(transparent)]
    RightsRoutingError(#[from] RightsRoutingError),

    #[error(transparent)]
    AvailabilityRoutingError(#[from] AvailabilityRoutingError),

    #[error(transparent)]
    PolicyError(#[from] PolicyError),

    #[error(
        "source capability at component {moniker} is void. \
        If the offer/expose declaration has `source_availability` set to `unknown`, \
        the source component instance likely isn't defined in the component declaration"
    )]
    SourceCapabilityIsVoid { moniker: Moniker },

    #[error(
        "routes that do not set the `debug` flag are unsupported in the current configuration (at `{moniker}`)."
    )]
    NonDebugRoutesUnsupported { moniker: ExtendedMoniker },

    #[error("{type_name} router unexpectedly returned debug info for target {moniker}")]
    RouteUnexpectedDebug { type_name: CapabilityTypeName, moniker: ExtendedMoniker },

    #[error("{type_name} router unexpectedly returned unavailable for target {moniker}")]
    RouteUnexpectedUnavailable { type_name: CapabilityTypeName, moniker: ExtendedMoniker },

    #[error("{name} at {moniker} is missing porcelain type metadata.")]
    MissingPorcelainType { name: Name, moniker: Moniker },
}

impl Explain for RoutingError {
    /// Convert this error into its approximate `zx::Status` equivalent.
    fn as_zx_status(&self) -> zx::Status {
        match self {
            RoutingError::UseFromRootEnvironmentNotAllowed { .. }
            | RoutingError::DynamicDictionariesNotAllowed { .. } => zx::Status::ACCESS_DENIED,
            RoutingError::StorageFromChildExposeNotFound { .. }
            | RoutingError::ComponentNotInIdIndex { .. }
            | RoutingError::UseFromComponentManagerNotFound { .. }
            | RoutingError::RegisterFromComponentManagerNotFound { .. }
            | RoutingError::OfferFromComponentManagerNotFound { .. }
            | RoutingError::UseFromParentNotFound { .. }
            | RoutingError::UseFromSelfNotFound { .. }
            | RoutingError::UseFromChildInstanceNotFound { .. }
            | RoutingError::UseFromEnvironmentNotFound { .. }
            | RoutingError::EnvironmentFromParentNotFound { .. }
            | RoutingError::EnvironmentFromChildExposeNotFound { .. }
            | RoutingError::EnvironmentFromChildInstanceNotFound { .. }
            | RoutingError::OfferFromParentNotFound { .. }
            | RoutingError::OfferFromSelfNotFound { .. }
            | RoutingError::StorageFromParentNotFound { .. }
            | RoutingError::OfferFromChildInstanceNotFound { .. }
            | RoutingError::OfferFromCollectionNotFound { .. }
            | RoutingError::OfferFromChildExposeNotFound { .. }
            | RoutingError::CapabilityFromFrameworkNotFound { .. }
            | RoutingError::CapabilityFromCapabilityNotFound { .. }
            | RoutingError::CapabilityFromComponentManagerNotFound { .. }
            | RoutingError::ExposeFromSelfNotFound { .. }
            | RoutingError::ExposeFromChildInstanceNotFound { .. }
            | RoutingError::ExposeFromCollectionNotFound { .. }
            | RoutingError::ExposeFromChildExposeNotFound { .. }
            | RoutingError::ExposeFromFrameworkNotFound { .. }
            | RoutingError::UseFromChildExposeNotFound { .. }
            | RoutingError::UnsupportedRouteSource { .. }
            | RoutingError::UnsupportedCapabilityType { .. }
            | RoutingError::EventsRoutingError(_)
            | RoutingError::BedrockNotPresentInDictionary { .. }
            | RoutingError::BedrockSourceDictionaryExposeNotFound { .. }
            | RoutingError::BedrockSourceDictionaryCollision { .. }
            | RoutingError::BedrockFailedToSend { .. }
            | RoutingError::RouteSourceShutdown { .. }
            | RoutingError::BedrockWrongCapabilityType { .. }
            | RoutingError::BedrockRemoteCapability { .. }
            | RoutingError::BedrockNotCloneable { .. }
            | RoutingError::AvailabilityRoutingError(_) => zx::Status::NOT_FOUND,
            RoutingError::BedrockMemberAccessUnsupported { .. }
            | RoutingError::NonDebugRoutesUnsupported { .. }
            | RoutingError::DictionariesNotSupported { .. } => zx::Status::NOT_SUPPORTED,
            RoutingError::ComponentInstanceError(err) => err.as_zx_status(),
            RoutingError::RightsRoutingError(err) => err.as_zx_status(),
            RoutingError::PolicyError(err) => err.as_zx_status(),
            RoutingError::SourceCapabilityIsVoid { .. } => zx::Status::NOT_FOUND,
            RoutingError::RouteUnexpectedDebug { .. }
            | RoutingError::RouteUnexpectedUnavailable { .. }
            | RoutingError::MissingPorcelainType { .. } => zx::Status::INTERNAL,
        }
    }
}

impl From<RoutingError> for ExtendedMoniker {
    fn from(err: RoutingError) -> ExtendedMoniker {
        match err {
            RoutingError::BedrockRemoteCapability { moniker, .. }
            | RoutingError::BedrockSourceDictionaryExposeNotFound { moniker, .. }
            | RoutingError::CapabilityFromCapabilityNotFound { moniker, .. }
            | RoutingError::CapabilityFromFrameworkNotFound { moniker, .. }
            | RoutingError::ComponentNotInIdIndex { source_moniker: moniker, .. }
            | RoutingError::DictionariesNotSupported { moniker, .. }
            | RoutingError::EnvironmentFromChildExposeNotFound { moniker, .. }
            | RoutingError::EnvironmentFromChildInstanceNotFound { moniker, .. }
            | RoutingError::EnvironmentFromParentNotFound { moniker, .. }
            | RoutingError::ExposeFromChildExposeNotFound { moniker, .. }
            | RoutingError::ExposeFromChildInstanceNotFound { moniker, .. }
            | RoutingError::ExposeFromCollectionNotFound { moniker, .. }
            | RoutingError::ExposeFromFrameworkNotFound { moniker, .. }
            | RoutingError::ExposeFromSelfNotFound { moniker, .. }
            | RoutingError::OfferFromChildExposeNotFound { moniker, .. }
            | RoutingError::OfferFromChildInstanceNotFound { moniker, .. }
            | RoutingError::OfferFromCollectionNotFound { moniker, .. }
            | RoutingError::OfferFromParentNotFound { moniker, .. }
            | RoutingError::OfferFromSelfNotFound { moniker, .. }
            | RoutingError::SourceCapabilityIsVoid { moniker, .. }
            | RoutingError::StorageFromChildExposeNotFound { moniker, .. }
            | RoutingError::StorageFromParentNotFound { moniker, .. }
            | RoutingError::UseFromChildExposeNotFound { moniker, .. }
            | RoutingError::UseFromChildInstanceNotFound { moniker, .. }
            | RoutingError::UseFromEnvironmentNotFound { moniker, .. }
            | RoutingError::UseFromParentNotFound { moniker, .. }
            | RoutingError::UseFromRootEnvironmentNotAllowed { moniker, .. }
            | RoutingError::DynamicDictionariesNotAllowed { moniker, .. }
            | RoutingError::RouteSourceShutdown { moniker }
            | RoutingError::UseFromSelfNotFound { moniker, .. }
            | RoutingError::MissingPorcelainType { moniker, .. } => moniker.into(),

            RoutingError::BedrockMemberAccessUnsupported { moniker }
            | RoutingError::BedrockNotPresentInDictionary { moniker, .. }
            | RoutingError::BedrockNotCloneable { moniker }
            | RoutingError::BedrockSourceDictionaryCollision { moniker }
            | RoutingError::BedrockFailedToSend { moniker, .. }
            | RoutingError::BedrockWrongCapabilityType { moniker, .. }
            | RoutingError::NonDebugRoutesUnsupported { moniker }
            | RoutingError::RouteUnexpectedDebug { moniker, .. }
            | RoutingError::RouteUnexpectedUnavailable { moniker, .. }
            | RoutingError::UnsupportedCapabilityType { moniker, .. }
            | RoutingError::UnsupportedRouteSource { moniker, .. } => moniker,
            RoutingError::AvailabilityRoutingError(err) => err.into(),
            RoutingError::ComponentInstanceError(err) => err.into(),
            RoutingError::EventsRoutingError(err) => err.into(),
            RoutingError::PolicyError(err) => err.into(),
            RoutingError::RightsRoutingError(err) => err.into(),

            RoutingError::CapabilityFromComponentManagerNotFound { .. }
            | RoutingError::OfferFromComponentManagerNotFound { .. }
            | RoutingError::RegisterFromComponentManagerNotFound { .. }
            | RoutingError::UseFromComponentManagerNotFound { .. } => {
                ExtendedMoniker::ComponentManager
            }
        }
    }
}

impl From<RoutingError> for RouterError {
    fn from(value: RoutingError) -> Self {
        Self::NotFound(Arc::new(value))
    }
}

impl From<RouterError> for RoutingError {
    fn from(value: RouterError) -> Self {
        match value {
            RouterError::NotFound(arc_dyn_explain) => {
                arc_dyn_explain.downcast_for_test::<Self>().clone()
            }
            err => panic!("Cannot downcast {err} to RoutingError!"),
        }
    }
}

impl RoutingError {
    /// Convert this error into its approximate `fuchsia.component.Error` equivalent.
    pub fn as_fidl_error(&self) -> fcomponent::Error {
        fcomponent::Error::ResourceUnavailable
    }

    pub fn storage_from_child_expose_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::StorageFromChildExposeNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn use_from_component_manager_not_found(capability_id: impl Into<String>) -> Self {
        Self::UseFromComponentManagerNotFound { capability_id: capability_id.into() }
    }

    pub fn register_from_component_manager_not_found(capability_id: impl Into<String>) -> Self {
        Self::RegisterFromComponentManagerNotFound { capability_id: capability_id.into() }
    }

    pub fn offer_from_component_manager_not_found(capability_id: impl Into<String>) -> Self {
        Self::OfferFromComponentManagerNotFound { capability_id: capability_id.into() }
    }

    pub fn use_from_parent_not_found(moniker: &Moniker, capability_id: impl Into<String>) -> Self {
        Self::UseFromParentNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn use_from_self_not_found(moniker: &Moniker, capability_id: impl Into<String>) -> Self {
        Self::UseFromSelfNotFound { moniker: moniker.clone(), capability_id: capability_id.into() }
    }

    pub fn use_from_child_instance_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::UseFromChildInstanceNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn use_from_environment_not_found(
        moniker: &Moniker,
        capability_type: impl Into<String>,
        capability_name: &Name,
    ) -> Self {
        Self::UseFromEnvironmentNotFound {
            moniker: moniker.clone(),
            capability_type: capability_type.into(),
            capability_name: capability_name.clone(),
        }
    }

    pub fn offer_from_parent_not_found(
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::OfferFromParentNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn offer_from_self_not_found(moniker: &Moniker, capability_id: impl Into<String>) -> Self {
        Self::OfferFromSelfNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn storage_from_parent_not_found(
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::StorageFromParentNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn offer_from_child_instance_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::OfferFromChildInstanceNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn offer_from_child_expose_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::OfferFromChildExposeNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn use_from_child_expose_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::UseFromChildExposeNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn expose_from_self_not_found(moniker: &Moniker, capability_id: impl Into<String>) -> Self {
        Self::ExposeFromSelfNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn expose_from_child_instance_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::ExposeFromChildInstanceNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn expose_from_child_expose_not_found(
        child_moniker: &ChildName,
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::ExposeFromChildExposeNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn capability_from_framework_not_found(
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::CapabilityFromFrameworkNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn capability_from_capability_not_found(
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::CapabilityFromCapabilityNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn capability_from_component_manager_not_found(capability_id: impl Into<String>) -> Self {
        Self::CapabilityFromComponentManagerNotFound { capability_id: capability_id.into() }
    }

    pub fn expose_from_framework_not_found(
        moniker: &Moniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::ExposeFromFrameworkNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn unsupported_route_source(
        moniker: impl Into<ExtendedMoniker>,
        source: impl Into<String>,
    ) -> Self {
        Self::UnsupportedRouteSource { source_type: source.into(), moniker: moniker.into() }
    }

    pub fn unsupported_capability_type(
        moniker: impl Into<ExtendedMoniker>,
        type_name: impl Into<CapabilityTypeName>,
    ) -> Self {
        Self::UnsupportedCapabilityType { type_name: type_name.into(), moniker: moniker.into() }
    }
}

/// Errors produced during routing specific to events.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Error, Debug, Clone, PartialEq)]
pub enum EventsRoutingError {
    #[error("filter is not a subset at `{moniker}`")]
    InvalidFilter { moniker: ExtendedMoniker },

    #[error("event routes must end at source with a filter declaration at `{moniker}`")]
    MissingFilter { moniker: ExtendedMoniker },
}

impl From<EventsRoutingError> for ExtendedMoniker {
    fn from(err: EventsRoutingError) -> ExtendedMoniker {
        match err {
            EventsRoutingError::InvalidFilter { moniker }
            | EventsRoutingError::MissingFilter { moniker } => moniker,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum RightsRoutingError {
    #[error(
        "requested rights ({requested}) greater than provided rights ({provided}) at \"{moniker}\""
    )]
    Invalid { moniker: ExtendedMoniker, requested: Rights, provided: Rights },

    #[error("directory routes must end at source with a rights declaration, it's missing at \"{moniker}\"")]
    MissingRightsSource { moniker: ExtendedMoniker },
}

impl RightsRoutingError {
    /// Convert this error into its approximate `zx::Status` equivalent.
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            RightsRoutingError::Invalid { .. } => zx::Status::ACCESS_DENIED,
            RightsRoutingError::MissingRightsSource { .. } => zx::Status::NOT_FOUND,
        }
    }
}

impl From<RightsRoutingError> for ExtendedMoniker {
    fn from(err: RightsRoutingError) -> ExtendedMoniker {
        match err {
            RightsRoutingError::Invalid { moniker, .. }
            | RightsRoutingError::MissingRightsSource { moniker } => moniker,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum AvailabilityRoutingError {
    #[error(
        "availability requested by the target has stronger guarantees than what \
    is being provided at the source at `{moniker}`"
    )]
    TargetHasStrongerAvailability { moniker: ExtendedMoniker },

    #[error("offer uses void source, but target requires the capability at `{moniker}`")]
    OfferFromVoidToRequiredTarget { moniker: ExtendedMoniker },

    #[error("expose uses void source, but target requires the capability at `{moniker}`")]
    ExposeFromVoidToRequiredTarget { moniker: ExtendedMoniker },
}

impl From<availability::TargetHasStrongerAvailability> for AvailabilityRoutingError {
    fn from(value: availability::TargetHasStrongerAvailability) -> Self {
        let availability::TargetHasStrongerAvailability { moniker } = value;
        AvailabilityRoutingError::TargetHasStrongerAvailability { moniker }
    }
}

impl From<AvailabilityRoutingError> for ExtendedMoniker {
    fn from(err: AvailabilityRoutingError) -> ExtendedMoniker {
        match err {
            AvailabilityRoutingError::ExposeFromVoidToRequiredTarget { moniker }
            | AvailabilityRoutingError::OfferFromVoidToRequiredTarget { moniker }
            | AvailabilityRoutingError::TargetHasStrongerAvailability { moniker } => moniker,
        }
    }
}

// Implements error reporting upon routing failure. For example, component
// manager logs the error.
#[async_trait]
pub trait ErrorReporter: Clone + Send + Sync + 'static {
    async fn report(&self, request: &RouteRequestErrorInfo, err: &RouterError);
}

/// What to print in an error if a route request fails.
pub struct RouteRequestErrorInfo {
    capability_type: cm_rust::CapabilityTypeName,
    name: cm_types::Name,
    availability: cm_rust::Availability,
}

impl RouteRequestErrorInfo {
    pub fn availability(&self) -> cm_rust::Availability {
        self.availability.clone()
    }
}

impl From<&cm_rust::UseDecl> for RouteRequestErrorInfo {
    fn from(value: &cm_rust::UseDecl) -> Self {
        RouteRequestErrorInfo {
            capability_type: value.into(),
            name: value.source_name().clone(),
            availability: value.availability().clone(),
        }
    }
}

impl From<&cm_rust::UseConfigurationDecl> for RouteRequestErrorInfo {
    fn from(value: &cm_rust::UseConfigurationDecl) -> Self {
        RouteRequestErrorInfo {
            capability_type: CapabilityTypeName::Config,
            name: value.source_name().clone(),
            availability: value.availability().clone(),
        }
    }
}

impl From<&cm_rust::ExposeDecl> for RouteRequestErrorInfo {
    fn from(value: &cm_rust::ExposeDecl) -> Self {
        RouteRequestErrorInfo {
            capability_type: value.into(),
            name: value.target_name().clone(),
            availability: value.availability().clone(),
        }
    }
}

impl From<&cm_rust::OfferDecl> for RouteRequestErrorInfo {
    fn from(value: &cm_rust::OfferDecl) -> Self {
        RouteRequestErrorInfo {
            capability_type: value.into(),
            name: value.target_name().clone(),
            availability: value.availability().clone(),
        }
    }
}

impl std::fmt::Display for RouteRequestErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} `{}`", self.capability_type, self.name)
    }
}
