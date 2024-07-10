// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::policy::PolicyError;
use crate::rights::Rights;
use clonable_error::ClonableError;
use cm_rust::CapabilityTypeName;
use cm_types::Name;
use moniker::{ChildName, Moniker, MonikerError};
use router_error::{Explain, RouterError};
use std::sync::Arc;
use thiserror::Error;
use {fidl_fuchsia_component as fcomponent, fuchsia_zircon_status as zx};

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
    #[error("`{capability_id}` is not a framework capability")]
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

    #[error("routing a capability from an unsupported source type: {source_type}")]
    UnsupportedRouteSource { source_type: String },

    #[error("routing a capability of an unsupported type: {type_name}")]
    UnsupportedCapabilityType { type_name: CapabilityTypeName },

    #[error("dictionaries are not yet supported for {cap_type} capabilities")]
    DictionariesNotSupported { cap_type: CapabilityTypeName },

    #[error("the capability does not support member access")]
    BedrockMemberAccessUnsupported,

    #[error("item `{name}` is not present in dictionary")]
    BedrockNotPresentInDictionary { name: String },

    #[error("routed capability was the wrong type. Was: {actual}, expected: {expected}")]
    BedrockWrongCapabilityType { actual: String, expected: String },

    #[error("there was an error remoting a capability")]
    BedrockRemoteCapability,

    #[error("source dictionary was not found in child's exposes")]
    BedrockSourceDictionaryExposeNotFound,

    #[error("Some capability in the routing chain could not be cloned.")]
    BedrockNotCloneable,

    #[error(
        "a capability in a dictionary extended from a source dictionary collides with \
        a capability in the source dictionary that has the same key"
    )]
    BedrockSourceDictionaryCollision,

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

    #[error(transparent)]
    MonikerError(#[from] MonikerError),

    #[error(
        "source capability is void. \
        If the offer/expose declaration has `source_availability` set to `unknown`, \
        the source component instance likely isn't defined in the component declaration"
    )]
    SourceCapabilityIsVoid,

    #[error(
        "routes that do not set the `debug` flag are unsupported in the current configuration."
    )]
    NonDebugRoutesUnsupported,
}

impl Explain for RoutingError {
    /// Convert this error into its approximate `zx::Status` equivalent.
    fn as_zx_status(&self) -> zx::Status {
        match self {
            RoutingError::UseFromRootEnvironmentNotAllowed { .. } => zx::Status::ACCESS_DENIED,
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
            | RoutingError::BedrockWrongCapabilityType { .. }
            | RoutingError::BedrockRemoteCapability { .. }
            | RoutingError::BedrockNotCloneable { .. }
            | RoutingError::AvailabilityRoutingError(_) => zx::Status::NOT_FOUND,
            RoutingError::BedrockMemberAccessUnsupported { .. }
            | RoutingError::NonDebugRoutesUnsupported
            | RoutingError::DictionariesNotSupported { .. } => zx::Status::NOT_SUPPORTED,
            RoutingError::MonikerError(_) => zx::Status::INTERNAL,
            RoutingError::ComponentInstanceError(err) => err.as_zx_status(),
            RoutingError::RightsRoutingError(err) => err.as_zx_status(),
            RoutingError::PolicyError(err) => err.as_zx_status(),
            RoutingError::SourceCapabilityIsVoid => zx::Status::NOT_FOUND,
        }
    }
}

impl From<RoutingError> for RouterError {
    fn from(value: RoutingError) -> Self {
        Self::NotFound(Arc::new(value))
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

    pub fn unsupported_route_source(source: impl Into<String>) -> Self {
        Self::UnsupportedRouteSource { source_type: source.into() }
    }

    pub fn unsupported_capability_type(type_name: impl Into<CapabilityTypeName>) -> Self {
        Self::UnsupportedCapabilityType { type_name: type_name.into() }
    }
}

/// Errors produced during routing specific to events.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Error, Debug, Clone, PartialEq)]
pub enum EventsRoutingError {
    #[error("filter is not a subset")]
    InvalidFilter,

    #[error("event routes must end at source with a filter declaration")]
    MissingFilter,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum RightsRoutingError {
    #[error("requested rights ({requested}) greater than provided rights ({provided})")]
    Invalid { requested: Rights, provided: Rights },

    #[error("directory routes must end at source with a rights declaration")]
    MissingRightsSource,
}

impl RightsRoutingError {
    /// Convert this error into its approximate `zx::Status` equivalent.
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            RightsRoutingError::Invalid { .. } => zx::Status::ACCESS_DENIED,
            RightsRoutingError::MissingRightsSource => zx::Status::NOT_FOUND,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum AvailabilityRoutingError {
    #[error(
        "availability requested by the target has stronger guarantees than what \
    is being provided at the source"
    )]
    TargetHasStrongerAvailability,

    #[error("offer uses void source, but target requires the capability")]
    OfferFromVoidToRequiredTarget,

    #[error("expose uses void source, but target requires the capability")]
    ExposeFromVoidToRequiredTarget,
}

impl From<availability::TargetHasStrongerAvailability> for AvailabilityRoutingError {
    fn from(value: availability::TargetHasStrongerAvailability) -> Self {
        let availability::TargetHasStrongerAvailability = value;
        AvailabilityRoutingError::TargetHasStrongerAvailability
    }
}
