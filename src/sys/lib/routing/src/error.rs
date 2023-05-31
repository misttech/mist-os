// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{component_id_index::ComponentIdIndexError, policy::PolicyError, rights::Rights},
    clonable_error::ClonableError,
    cm_rust::CapabilityTypeName,
    cm_types::Name,
    fidl_fuchsia_component as fcomponent, fuchsia_zircon_status as zx,
    moniker::{AbsoluteMoniker, ChildMoniker, MonikerError},
    thiserror::Error,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Errors produced by `ComponentInstanceInterface`.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone)]
pub enum ComponentInstanceError {
    #[error("component instance {} not found", moniker)]
    InstanceNotFound { moniker: AbsoluteMoniker },
    #[error("component manager instance unavailable")]
    ComponentManagerInstanceUnavailable {},
    #[error("malformed url {} for component instance {}", url, moniker)]
    MalformedUrl { url: String, moniker: AbsoluteMoniker },
    #[error("url {} for component {} does not resolve to an absolute url", url, moniker)]
    NoAbsoluteUrl { url: String, moniker: AbsoluteMoniker },
    // The capability routing static analyzer never produces this error subtype, so we don't need
    // to serialize it.
    #[cfg_attr(feature = "serde", serde(skip))]
    #[error("Failed to resolve `{}`: {}", moniker, err)]
    ResolveFailed {
        moniker: AbsoluteMoniker,
        #[source]
        err: ClonableError,
    },
    // The capability routing static analyzer never produces this error subtype, so we don't need
    // to serialize it.
    #[cfg_attr(feature = "serde", serde(skip))]
    #[error("Failed to unresolve `{}`: {}", moniker, err)]
    UnresolveFailed {
        moniker: AbsoluteMoniker,
        #[source]
        err: ClonableError,
    },
}

impl ComponentInstanceError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            ComponentInstanceError::ResolveFailed { .. }
            | ComponentInstanceError::InstanceNotFound { .. } => zx::Status::NOT_FOUND,
            _ => zx::Status::UNAVAILABLE,
        }
    }

    pub fn instance_not_found(moniker: AbsoluteMoniker) -> ComponentInstanceError {
        ComponentInstanceError::InstanceNotFound { moniker }
    }

    pub fn cm_instance_unavailable() -> ComponentInstanceError {
        ComponentInstanceError::ComponentManagerInstanceUnavailable {}
    }

    pub fn resolve_failed(moniker: AbsoluteMoniker, err: impl Into<anyhow::Error>) -> Self {
        Self::ResolveFailed { moniker, err: err.into().into() }
    }

    pub fn unresolve_failed(moniker: AbsoluteMoniker, err: impl Into<anyhow::Error>) -> Self {
        Self::UnresolveFailed { moniker, err: err.into().into() }
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
        "Backing directory `{}` was not exposed to `{}` from child `#{}`.",
        capability_id,
        moniker,
        child_moniker
    )]
    StorageFromChildExposeNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    #[error(
        "`{}` tried to use a storage capability from `{}` but it is not in the component id index. \
        See: https://fuchsia.dev/go/components/instance-id.",
        target_moniker, source_moniker
    )]
    ComponentNotInIdIndex { source_moniker: AbsoluteMoniker, target_moniker: AbsoluteMoniker },

    #[error("`{}` is not a built-in capability.", capability_id)]
    UseFromComponentManagerNotFound { capability_id: String },

    #[error("`{}` is not a built-in capability.", capability_id)]
    RegisterFromComponentManagerNotFound { capability_id: String },

    #[error("`{}` is not a built-in capability.", capability_id)]
    OfferFromComponentManagerNotFound { capability_id: String },

    #[error("`{}` was not offered to `{}` by parent.", capability_id, moniker)]
    UseFromParentNotFound { moniker: AbsoluteMoniker, capability_id: String },

    #[error("`{}` does not have child `#{}`.", moniker, child_moniker)]
    UseFromChildInstanceNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    #[error("`{}` was not registered in environment of `{}`.", capability_name, moniker)]
    UseFromEnvironmentNotFound {
        moniker: AbsoluteMoniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error(
        "`{}` tried to use {} `{}` from the root environment. This is not allowed.",
        moniker,
        capability_type,
        capability_name
    )]
    UseFromRootEnvironmentNotAllowed {
        moniker: AbsoluteMoniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error("`{}` was not offered to `{}` by parent.", capability_name, moniker)]
    EnvironmentFromParentNotFound {
        moniker: AbsoluteMoniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error(
        "`{}` was not exposed to `{}` from child `#{}`.",
        capability_name,
        moniker,
        child_moniker
    )]
    EnvironmentFromChildExposeNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_type: String,
        capability_name: Name,
    },

    #[error("`{}` does not have child `#{}`.", moniker, child_moniker)]
    EnvironmentFromChildInstanceNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_name: Name,
        capability_type: String,
    },

    #[error("`{}` was not offered to `{}` by parent.", capability_id, moniker)]
    OfferFromParentNotFound { moniker: AbsoluteMoniker, capability_id: String },

    #[error("`{}` was not offered to `{}` by parent.", capability_id, moniker)]
    StorageFromParentNotFound { moniker: AbsoluteMoniker, capability_id: String },

    #[error("`{}` does not have child `#{}`.", moniker, child_moniker)]
    OfferFromChildInstanceNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    #[error("`{}` does not have collection `#{}`.", moniker, collection)]
    OfferFromCollectionNotFound { collection: String, moniker: AbsoluteMoniker, capability: Name },

    #[error(
        "`{}` was not exposed to `{}` from child `#{}`.",
        capability_id,
        moniker,
        child_moniker
    )]
    OfferFromChildExposeNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    // TODO: Could this be distinguished by use/offer/expose?
    #[error("`{}` is not a framework capability.", capability_id)]
    CapabilityFromFrameworkNotFound { moniker: AbsoluteMoniker, capability_id: String },

    #[error(
        "A capability was sourced to a base capability `{}` from `{}`, but this is unsupported.",
        capability_id,
        moniker
    )]
    CapabilityFromCapabilityNotFound { moniker: AbsoluteMoniker, capability_id: String },

    // TODO: Could this be distinguished by use/offer/expose?
    #[error("`{}` is not a framework capability.", capability_id)]
    CapabilityFromComponentManagerNotFound { capability_id: String },

    #[error(
        "A capability was sourced to storage capability `{}` with id `{}`, but no matching \
        capability was found.",
        storage_capability,
        capability_id
    )]
    CapabilityFromStorageCapabilityNotFound { storage_capability: String, capability_id: String },

    #[error("`{}` does not have child `#{}`.", moniker, child_moniker)]
    ExposeFromChildInstanceNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    #[error("`{}` does not have collection `#{}`.", moniker, collection)]
    ExposeFromCollectionNotFound { collection: String, moniker: AbsoluteMoniker, capability: Name },

    #[error(
        "`{}` was not exposed to `{}` from child `#{}`.",
        capability_id,
        moniker,
        child_moniker
    )]
    ExposeFromChildExposeNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    #[error(
        "`{}` tried to expose `{}` from the framework, but no such framework capability was found.",
        moniker,
        capability_id
    )]
    ExposeFromFrameworkNotFound { moniker: AbsoluteMoniker, capability_id: String },

    #[error(
        "`{}` was not exposed to `{}` from child `#{}`.",
        capability_id,
        moniker,
        child_moniker
    )]
    UseFromChildExposeNotFound {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability_id: String,
    },

    #[error(
        "`{}` was queried in an aggregate for `{}` at `{}` but it does not contain that child.",
        child_moniker,
        capability,
        moniker
    )]
    UnexpectedChildInAggregate {
        child_moniker: ChildMoniker,
        moniker: AbsoluteMoniker,
        capability: Name,
    },

    #[error("Routing a capability from an unsupported source type: {}.", source_type)]
    UnsupportedRouteSource { source_type: String },

    #[error("Routing a capability of an unsupported type: {}", type_name)]
    UnsupportedCapabilityType { type_name: CapabilityTypeName },

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
    ComponentIdIndexError(#[from] ComponentIdIndexError),

    #[error(transparent)]
    MonikerError(#[from] MonikerError),
}

impl RoutingError {
    /// Convert this error into its approximate `fuchsia.component.Error` equivalent.
    pub fn as_fidl_error(&self) -> fcomponent::Error {
        fcomponent::Error::ResourceUnavailable
    }

    /// Convert this error into its approximate `zx::Status` equivalent.
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            RoutingError::PolicyError(_) => zx::Status::ACCESS_DENIED,
            RoutingError::ComponentInstanceError(err) => err.as_zx_status(),
            _ => zx::Status::UNAVAILABLE,
        }
    }

    pub fn storage_from_child_expose_not_found(
        child_moniker: &ChildMoniker,
        moniker: &AbsoluteMoniker,
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

    pub fn use_from_parent_not_found(
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::UseFromParentNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn use_from_child_instance_not_found(
        child_moniker: &ChildMoniker,
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::UseFromChildInstanceNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn offer_from_parent_not_found(
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::OfferFromParentNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn storage_from_parent_not_found(
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::StorageFromParentNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn offer_from_child_instance_not_found(
        child_moniker: &ChildMoniker,
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::OfferFromChildInstanceNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn offer_from_child_expose_not_found(
        child_moniker: &ChildMoniker,
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::OfferFromChildExposeNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn expose_from_child_instance_not_found(
        child_moniker: &ChildMoniker,
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::ExposeFromChildInstanceNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn expose_from_child_expose_not_found(
        child_moniker: &ChildMoniker,
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::ExposeFromChildExposeNotFound {
            child_moniker: child_moniker.clone(),
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn capability_from_framework_not_found(
        moniker: &AbsoluteMoniker,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::CapabilityFromFrameworkNotFound {
            moniker: moniker.clone(),
            capability_id: capability_id.into(),
        }
    }

    pub fn capability_from_capability_not_found(
        moniker: &AbsoluteMoniker,
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

    pub fn capability_from_storage_capability_not_found(
        storage_capability: impl Into<String>,
        capability_id: impl Into<String>,
    ) -> Self {
        Self::CapabilityFromStorageCapabilityNotFound {
            storage_capability: storage_capability.into(),
            capability_id: capability_id.into(),
        }
    }

    pub fn expose_from_framework_not_found(
        moniker: &AbsoluteMoniker,
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
    #[error("Filter is not a subset")]
    InvalidFilter,

    #[error("Event routes must end at source with a filter declaration")]
    MissingFilter,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum RightsRoutingError {
    #[error("Requested rights ({requested}) greater than provided rights ({provided})")]
    Invalid { requested: Rights, provided: Rights },

    #[error("Directory routes must end at source with a rights declaration")]
    MissingRightsSource,
}

impl RightsRoutingError {
    /// Convert this error into its approximate `zx::Status` equivalent.
    pub fn as_zx_status(&self) -> zx::Status {
        zx::Status::UNAVAILABLE
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum AvailabilityRoutingError {
    #[error("Availability of target has stronger guarantees than what is being offered.")]
    TargetHasStrongerAvailability,

    #[error("Offer uses void source, but target requires the capability")]
    OfferFromVoidToRequiredTarget,

    #[error("Offer or expose uses void source, so the route cannot be completed")]
    RouteFromVoidToOptionalTarget,

    #[error("Expose uses void source, but target requires the capability")]
    ExposeFromVoidToRequiredTarget,

    #[error("Target optionally uses capability that was not available: {reason}")]
    FailedToRouteToOptionalTarget { reason: String },
}
