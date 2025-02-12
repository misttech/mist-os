// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod availability;
pub mod bedrock;
pub mod capability_source;
pub mod collection;
pub mod component_instance;
pub mod config;
pub mod environment;
pub mod error;
pub mod event;
pub mod legacy_router;
pub mod mapper;
pub mod path;
pub mod policy;
pub mod resolving;
pub mod rights;
pub mod walk_state;

use crate::bedrock::request_metadata::{
    dictionary_metadata, protocol_metadata, resolver_metadata, runner_metadata,
};
use crate::capability_source::{
    CapabilitySource, ComponentCapability, ComponentSource, InternalCapability, VoidSource,
};
use crate::component_instance::{
    ComponentInstanceInterface, ResolvedInstanceInterface, WeakComponentInstanceInterface,
};
use crate::environment::DebugRegistration;
use crate::error::RoutingError;
use crate::legacy_router::{
    CapabilityVisitor, ErrorNotFoundFromParent, ErrorNotFoundInChild, ExposeVisitor, NoopVisitor,
    OfferVisitor, RouteBundle, Sources,
};
use crate::mapper::DebugRouteMapper;
use crate::rights::RightsWalker;
use crate::walk_state::WalkState;
use cm_rust::{
    Availability, CapabilityTypeName, ExposeConfigurationDecl, ExposeDecl, ExposeDeclCommon,
    ExposeDirectoryDecl, ExposeProtocolDecl, ExposeResolverDecl, ExposeRunnerDecl,
    ExposeServiceDecl, ExposeSource, OfferConfigurationDecl, OfferDeclCommon, OfferDictionaryDecl,
    OfferDirectoryDecl, OfferEventStreamDecl, OfferProtocolDecl, OfferResolverDecl,
    OfferRunnerDecl, OfferServiceDecl, OfferSource, OfferStorageDecl, OfferTarget,
    RegistrationDeclCommon, RegistrationSource, ResolverRegistration, RunnerRegistration,
    SourceName, StorageDecl, StorageDirectorySource, UseConfigurationDecl, UseDecl, UseDeclCommon,
    UseDirectoryDecl, UseEventStreamDecl, UseProtocolDecl, UseRunnerDecl, UseServiceDecl,
    UseSource, UseStorageDecl,
};
use cm_types::{IterablePath, Name, RelativePath};
use from_enum::FromEnum;
use itertools::Itertools;
use moniker::{ChildName, ExtendedMoniker, Moniker, MonikerError};
use router_error::Explain;
use sandbox::{
    Capability, CapabilityBound, Connector, Data, Dict, Request, Routable, Router, RouterResponse,
};
use std::sync::Arc;
use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio, zx_status as zx};

pub use bedrock::dict_ext::{DictExt, GenericRouterResponse};
pub use bedrock::lazy_get::LazyGet;
pub use bedrock::weak_instance_token_ext::{test_invalid_instance_token, WeakInstanceTokenExt};
pub use bedrock::with_availability::WithAvailability;
pub use bedrock::with_default::WithDefault;
pub use bedrock::with_error_reporter::WithErrorReporter;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A request to route a capability, together with the data needed to do so.
#[derive(Clone, Debug)]
pub enum RouteRequest {
    // Route a capability from an ExposeDecl.
    ExposeDirectory(ExposeDirectoryDecl),
    ExposeProtocol(ExposeProtocolDecl),
    ExposeService(RouteBundle<ExposeServiceDecl>),
    ExposeRunner(ExposeRunnerDecl),
    ExposeResolver(ExposeResolverDecl),
    ExposeConfig(ExposeConfigurationDecl),

    // Route a capability from a realm's environment.
    Resolver(ResolverRegistration),

    // Route the directory capability that backs a storage capability.
    StorageBackingDirectory(StorageDecl),

    // Route a capability from a UseDecl.
    UseDirectory(UseDirectoryDecl),
    UseEventStream(UseEventStreamDecl),
    UseProtocol(UseProtocolDecl),
    UseService(UseServiceDecl),
    UseStorage(UseStorageDecl),
    UseRunner(UseRunnerDecl),
    UseConfig(UseConfigurationDecl),

    // Route a capability from an OfferDecl.
    OfferDirectory(OfferDirectoryDecl),
    OfferEventStream(OfferEventStreamDecl),
    OfferProtocol(OfferProtocolDecl),
    OfferService(RouteBundle<OfferServiceDecl>),
    OfferStorage(OfferStorageDecl),
    OfferRunner(OfferRunnerDecl),
    OfferResolver(OfferResolverDecl),
    OfferConfig(OfferConfigurationDecl),
    OfferDictionary(OfferDictionaryDecl),
}

impl From<UseDecl> for RouteRequest {
    fn from(decl: UseDecl) -> Self {
        match decl {
            UseDecl::Directory(decl) => Self::UseDirectory(decl),
            UseDecl::Protocol(decl) => Self::UseProtocol(decl),
            UseDecl::Service(decl) => Self::UseService(decl),
            UseDecl::Storage(decl) => Self::UseStorage(decl),
            UseDecl::EventStream(decl) => Self::UseEventStream(decl),
            UseDecl::Runner(decl) => Self::UseRunner(decl),
            UseDecl::Config(decl) => Self::UseConfig(decl),
        }
    }
}

impl RouteRequest {
    pub fn from_expose_decls(
        moniker: &Moniker,
        exposes: Vec<&ExposeDecl>,
    ) -> Result<Self, RoutingError> {
        let first_expose = exposes.first().expect("invalid empty expose list");
        let first_type_name = CapabilityTypeName::from(*first_expose);
        assert!(
            exposes.iter().all(|e| {
                let type_name: CapabilityTypeName = CapabilityTypeName::from(*e);
                first_type_name == type_name && first_expose.target_name() == e.target_name()
            }),
            "invalid expose input: {:?}",
            exposes
        );
        match first_expose {
            ExposeDecl::Protocol(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Ok(Self::ExposeProtocol(e.clone()))
            }
            ExposeDecl::Service(_) => {
                // Gather the exposes into a bundle. Services can aggregate, in which case
                // multiple expose declarations map to one expose directory entry.
                let exposes: Vec<_> = exposes
                    .into_iter()
                    .filter_map(|e| match e {
                        cm_rust::ExposeDecl::Service(e) => Some(e.clone()),
                        _ => None,
                    })
                    .collect();
                Ok(Self::ExposeService(RouteBundle::from_exposes(exposes)))
            }
            ExposeDecl::Directory(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Ok(Self::ExposeDirectory(e.clone()))
            }
            ExposeDecl::Runner(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Ok(Self::ExposeRunner(e.clone()))
            }
            ExposeDecl::Resolver(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Ok(Self::ExposeResolver(e.clone()))
            }
            ExposeDecl::Config(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Ok(Self::ExposeConfig(e.clone()))
            }
            ExposeDecl::Dictionary(_) => {
                // Only bedrock routing supports dictionaries, the legacy RouteRequest does not.
                Err(RoutingError::unsupported_capability_type(
                    moniker.clone(),
                    CapabilityTypeName::Dictionary,
                ))
            }
        }
    }

    /// Returns the availability of the RouteRequest if supported.
    pub fn availability(&self) -> Option<Availability> {
        use crate::RouteRequest::*;
        match self {
            UseDirectory(UseDirectoryDecl { availability, .. })
            | UseEventStream(UseEventStreamDecl { availability, .. })
            | UseProtocol(UseProtocolDecl { availability, .. })
            | UseService(UseServiceDecl { availability, .. })
            | UseConfig(UseConfigurationDecl { availability, .. })
            | UseStorage(UseStorageDecl { availability, .. }) => Some(*availability),

            ExposeDirectory(decl) => Some(*decl.availability()),
            ExposeProtocol(decl) => Some(*decl.availability()),
            ExposeService(decl) => Some(*decl.availability()),
            ExposeRunner(decl) => Some(*decl.availability()),
            ExposeResolver(decl) => Some(*decl.availability()),
            ExposeConfig(decl) => Some(*decl.availability()),

            OfferRunner(decl) => Some(*decl.availability()),
            OfferResolver(decl) => Some(*decl.availability()),
            OfferDirectory(decl) => Some(*decl.availability()),
            OfferEventStream(decl) => Some(*decl.availability()),
            OfferProtocol(decl) => Some(*decl.availability()),
            OfferConfig(decl) => Some(*decl.availability()),
            OfferStorage(decl) => Some(*decl.availability()),
            OfferDictionary(decl) => Some(*decl.availability()),

            OfferService(_) | Resolver(_) | StorageBackingDirectory(_) | UseRunner(_) => None,
        }
    }
}

impl std::fmt::Display for RouteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExposeDirectory(e) => {
                write!(f, "directory `{}`", e.target_name)
            }
            Self::ExposeProtocol(e) => {
                write!(f, "protocol `{}`", e.target_name)
            }
            Self::ExposeService(e) => {
                write!(f, "service {:?}", e)
            }
            Self::ExposeRunner(e) => {
                write!(f, "runner `{}`", e.target_name)
            }
            Self::ExposeResolver(e) => {
                write!(f, "resolver `{}`", e.target_name)
            }
            Self::ExposeConfig(e) => {
                write!(f, "config `{}`", e.target_name)
            }
            Self::Resolver(r) => {
                write!(f, "resolver `{}`", r.resolver)
            }
            Self::UseDirectory(u) => {
                write!(f, "directory `{}`", u.source_name)
            }
            Self::UseProtocol(u) => {
                write!(f, "protocol `{}`", u.source_name)
            }
            Self::UseService(u) => {
                write!(f, "service `{}`", u.source_name)
            }
            Self::UseStorage(u) => {
                write!(f, "storage `{}`", u.source_name)
            }
            Self::UseEventStream(u) => {
                write!(f, "event stream `{}`", u.source_name)
            }
            Self::UseRunner(u) => {
                write!(f, "runner `{}`", u.source_name)
            }
            Self::UseConfig(u) => {
                write!(f, "config `{}`", u.source_name)
            }
            Self::StorageBackingDirectory(u) => {
                write!(f, "storage backing directory `{}`", u.backing_dir)
            }
            Self::OfferDirectory(o) => {
                write!(f, "directory `{}`", o.target_name)
            }
            Self::OfferProtocol(o) => {
                write!(f, "protocol `{}`", o.target_name)
            }
            Self::OfferService(o) => {
                write!(f, "service {:?}", o)
            }
            Self::OfferEventStream(o) => {
                write!(f, "event stream `{}`", o.target_name)
            }
            Self::OfferStorage(o) => {
                write!(f, "storage `{}`", o.target_name)
            }
            Self::OfferResolver(o) => {
                write!(f, "resolver `{}`", o.target_name)
            }
            Self::OfferRunner(o) => {
                write!(f, "runner `{}`", o.target_name)
            }
            Self::OfferConfig(o) => {
                write!(f, "config `{}`", o.target_name)
            }
            Self::OfferDictionary(o) => {
                write!(f, "dictionary `{}`", o.target_name)
            }
        }
    }
}

/// The data returned after successfully routing a capability to its source.
#[derive(Debug)]
pub struct RouteSource {
    pub source: CapabilitySource,
    pub relative_path: RelativePath,
}

impl RouteSource {
    pub fn new(source: CapabilitySource) -> Self {
        Self { source, relative_path: Default::default() }
    }

    pub fn new_with_relative_path(source: CapabilitySource, relative_path: RelativePath) -> Self {
        Self { source, relative_path }
    }
}

/// Performs a debug route from the `target` for the capability defined in `request`. The source of
/// the route is returned if the route is valid, otherwise a routing error is returned.
///
/// If the capability is not allowed to be routed to the `target`, per the
/// [`crate::model::policy::GlobalPolicyChecker`], then an error is returned.
///
/// This function will only be used for developer tools once the bedrock routing refactor has been
/// completed, but for now it's the only way to route capabilities which are unsupported in
/// bedrock.
///
/// For capabilities which are not supported in bedrock, the `mapper` is invoked on every step in
/// the routing process and can be used to record the routing steps. Once all capabilities are
/// supported in bedrock routing, the `mapper` argument will be removed.
pub async fn route_capability<C>(
    request: RouteRequest,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match request {
        // Route from an ExposeDecl
        RouteRequest::ExposeDirectory(expose_directory_decl) => {
            route_directory_from_expose(expose_directory_decl, target, mapper).await
        }
        RouteRequest::ExposeProtocol(expose_protocol_decl) => {
            route_capability_inner::<Connector, _>(
                &target.component_sandbox().await?.component_output.capabilities(),
                &expose_protocol_decl.target_name,
                protocol_metadata(expose_protocol_decl.availability),
                target,
            )
            .await
        }
        RouteRequest::ExposeService(expose_bundle) => {
            route_service_from_expose(expose_bundle, target, mapper).await
        }
        RouteRequest::ExposeRunner(expose_runner_decl) => {
            route_capability_inner::<Connector, _>(
                &target.component_sandbox().await?.component_output.capabilities(),
                &expose_runner_decl.target_name,
                runner_metadata(Availability::Required),
                target,
            )
            .await
        }
        RouteRequest::ExposeResolver(expose_resolver_decl) => {
            route_capability_inner::<Connector, _>(
                &target.component_sandbox().await?.component_output.capabilities(),
                &expose_resolver_decl.target_name,
                resolver_metadata(Availability::Required),
                target,
            )
            .await
        }
        RouteRequest::ExposeConfig(expose_config_decl) => {
            route_config_from_expose(expose_config_decl, target, mapper).await
        }

        // Route a resolver or runner from an environment
        RouteRequest::Resolver(resolver_registration) => {
            let component_sandbox = target.component_sandbox().await?;
            let source_dictionary = match &resolver_registration.source {
                RegistrationSource::Parent => component_sandbox.component_input.capabilities(),
                RegistrationSource::Self_ => component_sandbox.program_output_dict.clone(),
                RegistrationSource::Child(static_name) => {
                    let child_name = ChildName::parse(static_name).expect(
                        "invalid child name, this should be prevented by manifest validation",
                    );
                    let child_component = target.lock_resolved_state().await?.get_child(&child_name).expect("resolver registration references nonexistent static child, this should be prevented by manifest validation");
                    let child_sandbox = child_component.component_sandbox().await?;
                    child_sandbox.component_output.capabilities().clone()
                }
            };
            route_capability_inner::<Connector, _>(
                &source_dictionary,
                &resolver_registration.resolver,
                resolver_metadata(Availability::Required),
                target,
            )
            .await
        }
        // Route the backing directory for a storage capability
        RouteRequest::StorageBackingDirectory(storage_decl) => {
            route_storage_backing_directory(storage_decl, target, mapper).await
        }

        // Route from a UseDecl
        RouteRequest::UseDirectory(use_directory_decl) => {
            route_directory(use_directory_decl, target, mapper).await
        }
        RouteRequest::UseEventStream(use_event_stream_decl) => {
            route_event_stream(use_event_stream_decl, target, mapper).await
        }
        RouteRequest::UseProtocol(use_protocol_decl) => {
            route_capability_inner::<Connector, _>(
                &target.component_sandbox().await?.program_input.namespace(),
                &use_protocol_decl.target_path,
                protocol_metadata(use_protocol_decl.availability),
                target,
            )
            .await
        }
        RouteRequest::UseService(use_service_decl) => {
            route_service(use_service_decl, target, mapper).await
        }
        RouteRequest::UseStorage(use_storage_decl) => {
            route_storage(use_storage_decl, target, mapper).await
        }
        RouteRequest::UseRunner(_use_runner_decl) => {
            let router =
                target.component_sandbox().await?.program_input.runner().expect("we have a use declaration for a runner but the program input dictionary has no runner, this should be impossible");
            perform_route::<Connector, _>(router, runner_metadata(Availability::Required), target)
                .await
        }
        RouteRequest::UseConfig(use_config_decl) => {
            route_config(use_config_decl, target, mapper).await
        }

        // Route from a OfferDecl
        RouteRequest::OfferProtocol(offer_protocol_decl) => {
            let target_dictionary =
                get_dictionary_for_offer_target(target, &offer_protocol_decl).await?;
            let metadata = protocol_metadata(offer_protocol_decl.availability);
            metadata
                .insert(
                    Name::new(crate::bedrock::with_policy_check::SKIP_POLICY_CHECKS).unwrap(),
                    Capability::Data(Data::Uint64(1)),
                )
                .unwrap();
            route_capability_inner::<Connector, _>(
                &target_dictionary,
                &offer_protocol_decl.target_name,
                metadata,
                target,
            )
            .await
        }
        RouteRequest::OfferDictionary(offer_dictionary_decl) => {
            let target_dictionary =
                get_dictionary_for_offer_target(target, &offer_dictionary_decl).await?;
            let metadata = dictionary_metadata(offer_dictionary_decl.availability);
            metadata
                .insert(
                    Name::new(crate::bedrock::with_policy_check::SKIP_POLICY_CHECKS).unwrap(),
                    Capability::Data(Data::Uint64(1)),
                )
                .unwrap();
            route_capability_inner::<Dict, _>(
                &target_dictionary,
                &offer_dictionary_decl.target_name,
                metadata,
                target,
            )
            .await
        }
        RouteRequest::OfferDirectory(offer_directory_decl) => {
            route_directory_from_offer(offer_directory_decl, target, mapper).await
        }
        RouteRequest::OfferStorage(offer_storage_decl) => {
            route_storage_from_offer(offer_storage_decl, target, mapper).await
        }
        RouteRequest::OfferService(offer_service_decl) => {
            route_service_from_offer(offer_service_decl, target, mapper).await
        }
        RouteRequest::OfferEventStream(offer_event_stream_decl) => {
            route_event_stream_from_offer(offer_event_stream_decl, target, mapper).await
        }
        RouteRequest::OfferRunner(offer_runner_decl) => {
            let target_dictionary =
                get_dictionary_for_offer_target(target, &offer_runner_decl).await?;
            let metadata = runner_metadata(Availability::Required);
            metadata
                .insert(
                    Name::new(crate::bedrock::with_policy_check::SKIP_POLICY_CHECKS).unwrap(),
                    Capability::Data(Data::Uint64(1)),
                )
                .unwrap();
            route_capability_inner::<Connector, _>(
                &target_dictionary,
                &offer_runner_decl.target_name,
                metadata,
                target,
            )
            .await
        }
        RouteRequest::OfferResolver(offer_resolver_decl) => {
            let target_dictionary =
                get_dictionary_for_offer_target(target, &offer_resolver_decl).await?;
            let metadata = resolver_metadata(Availability::Required);
            metadata
                .insert(
                    Name::new(crate::bedrock::with_policy_check::SKIP_POLICY_CHECKS).unwrap(),
                    Capability::Data(Data::Uint64(1)),
                )
                .unwrap();
            route_capability_inner::<Connector, _>(
                &target_dictionary,
                &offer_resolver_decl.target_name,
                metadata,
                target,
            )
            .await
        }
        RouteRequest::OfferConfig(offer) => route_config_from_offer(offer, target, mapper).await,
    }
}

pub enum Never {}

async fn route_capability_inner<T, C>(
    dictionary: &Dict,
    path: &impl IterablePath,
    metadata: Dict,
    target: &Arc<C>,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    T: CapabilityBound,
    Router<T>: TryFrom<Capability>,
{
    let router = dictionary
        .get_capability(path)
        .and_then(|c| Router::<T>::try_from(c).ok())
        .ok_or_else(|| RoutingError::BedrockNotPresentInDictionary {
            moniker: target.moniker().clone().into(),
            name: path.iter_segments().join("/"),
        })?;
    perform_route::<T, C>(router, metadata, target).await
}

async fn perform_route<T, C>(
    router: impl Routable<T>,
    metadata: Dict,
    target: &Arc<C>,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    T: CapabilityBound,
    Router<T>: TryFrom<Capability>,
{
    let request = Request { target: WeakComponentInstanceInterface::new(target).into(), metadata };
    let data = match router.route(Some(request), true).await? {
        RouterResponse::<T>::Debug(d) => d,
        _ => panic!("Debug route did not return a debug response"),
    };
    Ok(RouteSource::new(data.try_into().unwrap()))
}

async fn get_dictionary_for_offer_target<C, O>(
    target: &Arc<C>,
    offer: &O,
) -> Result<Dict, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    O: OfferDeclCommon,
{
    match offer.target() {
        OfferTarget::Child(child_ref) if child_ref.collection.is_none() => {
            // For static children we can find their inputs in the component's sandbox.
            let child_input_name = Name::new(child_ref.name.to_string())
                .map_err(MonikerError::InvalidMonikerPart)
                .expect("static child names must be short");
            let target_sandbox = target.component_sandbox().await?;
            let child_input = target_sandbox.child_inputs.get(&child_input_name).ok_or(
                RoutingError::OfferFromChildInstanceNotFound {
                    child_moniker: child_ref.clone().into(),
                    moniker: target.moniker().clone(),
                    capability_id: offer.target_name().clone().to_string(),
                },
            )?;
            Ok(child_input.capabilities())
        }
        OfferTarget::Child(child_ref) => {
            // Offers targeting dynamic children are trickier. The input to the dynamic
            // child wasn't created as part of the parent's sandbox, and dynamic offers
            // (like the one we're currently looking at) won't have their routes reflected
            // in the general component input for the collection. To work around this, we
            // look up the dynamic child from the parent and access its component input
            // from there. Unlike the code path for static children, this causes the child
            // to be resolved.
            let child =
                target.lock_resolved_state().await?.get_child(&child_ref.clone().into()).ok_or(
                    RoutingError::OfferFromChildInstanceNotFound {
                        child_moniker: child_ref.clone().into(),
                        moniker: target.moniker().clone(),
                        capability_id: offer.target_name().clone().to_string(),
                    },
                )?;
            Ok(child.component_sandbox().await?.component_input.capabilities())
        }
        OfferTarget::Collection(collection_name) => {
            // Offers targeting collections start at the component input generated for the
            // collection, which is in the component's sandbox.
            let target_sandbox = target.component_sandbox().await?;
            let collection_input = target_sandbox.collection_inputs.get(collection_name).ok_or(
                RoutingError::OfferFromCollectionNotFound {
                    collection: collection_name.to_string(),
                    moniker: target.moniker().clone(),
                    capability: offer.target_name().clone(),
                },
            )?;
            Ok(collection_input.capabilities())
        }
        OfferTarget::Capability(dictionary_name) => {
            // Offers targeting another capability are for adding the capability to a dictionary
            // declared by the same component. These dictionaries are stored in the target's
            // sandbox.
            let target_sandbox = target.component_sandbox().await?;
            let capability =
                target_sandbox.declared_dictionaries.get(dictionary_name).ok().flatten().ok_or(
                    RoutingError::BedrockNotPresentInDictionary {
                        name: dictionary_name.to_string(),
                        moniker: target.moniker().clone().into(),
                    },
                )?;
            match capability {
                Capability::Dictionary(dictionary) => Ok(dictionary),
                other_type => Err(RoutingError::BedrockWrongCapabilityType {
                    actual: other_type.debug_typename().to_string(),
                    expected: "Dictionary".to_string(),
                    moniker: target.moniker().clone().into(),
                }),
            }
        }
    }
}

/// Routes a Directory capability from `target` to its source, starting from `offer_decl`.
/// Returns the capability source, along with a `DirectoryState` accumulated from traversing
/// the route.
async fn route_directory_from_offer<C>(
    offer_decl: OfferDirectoryDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut state = DirectoryState {
        rights: WalkState::new(),
        subdir: Default::default(),
        availability_state: offer_decl.availability.into(),
    };
    let allowed_sources =
        Sources::new(CapabilityTypeName::Directory).framework().namespace().component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources,
        &mut state,
        mapper,
    )
    .await?;
    Ok(RouteSource::new_with_relative_path(source, state.subdir))
}

async fn route_service_from_offer<C>(
    offer_bundle: RouteBundle<OfferServiceDecl>,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    // TODO(https://fxbug.dev/42124541): Figure out how to set the availability when `offer_bundle` contains
    // multiple routes with different availabilities. It's possible that manifest validation should
    // disallow this. For now, just pick the first.
    let mut availability_visitor = offer_bundle.iter().next().unwrap().availability;
    let allowed_sources = Sources::new(CapabilityTypeName::Service).component().collection();
    let source = legacy_router::route_from_offer(
        offer_bundle.map(Into::into),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

/// Routes an EventStream capability from `target` to its source, starting from `offer_decl`.
async fn route_event_stream_from_offer<C>(
    offer_decl: OfferEventStreamDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = Sources::new(CapabilityTypeName::EventStream).builtin();

    let mut availability_visitor = offer_decl.availability;
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

async fn route_storage_from_offer<C>(
    offer_decl: OfferStorageDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = offer_decl.availability;
    let allowed_sources = Sources::new(CapabilityTypeName::Storage).component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

async fn route_config_from_offer<C>(
    offer_decl: OfferConfigurationDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = Sources::new(CapabilityTypeName::Config).builtin().component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources,
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

async fn route_config_from_expose<C>(
    expose_decl: ExposeConfigurationDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = Sources::new(CapabilityTypeName::Config).component().capability();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources,
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

async fn route_service<C>(
    use_decl: UseServiceDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match use_decl.source {
        UseSource::Self_ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources = Sources::new(CapabilityTypeName::Service).component();
            let source = legacy_router::route_from_self(
                use_decl.into(),
                target.clone(),
                allowed_sources,
                &mut availability_visitor,
                mapper,
            )
            .await?;
            Ok(RouteSource::new(source))
        }
        _ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources =
                Sources::new(CapabilityTypeName::Service).component().collection();
            let source = legacy_router::route_from_use(
                use_decl.into(),
                target.clone(),
                allowed_sources,
                &mut availability_visitor,
                mapper,
            )
            .await?;

            target.policy_checker().can_route_capability(&source, target.moniker())?;
            Ok(RouteSource::new(source))
        }
    }
}

async fn route_service_from_expose<C>(
    expose_bundle: RouteBundle<ExposeServiceDecl>,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = expose_bundle.availability().clone();
    let allowed_sources = Sources::new(CapabilityTypeName::Service).component().collection();
    let source = legacy_router::route_from_expose(
        expose_bundle.map(Into::into),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// The accumulated state of routing a Directory capability.
#[derive(Clone, Debug)]
pub struct DirectoryState {
    rights: WalkState<RightsWalker>,
    pub subdir: RelativePath,
    availability_state: Availability,
}

impl DirectoryState {
    fn new(rights: RightsWalker, subdir: RelativePath, availability: &Availability) -> Self {
        DirectoryState {
            rights: WalkState::at(rights),
            subdir,
            availability_state: availability.clone(),
        }
    }

    fn advance_with_offer(
        &mut self,
        moniker: &ExtendedMoniker,
        offer: &OfferDirectoryDecl,
    ) -> Result<(), RoutingError> {
        self.availability_state =
            availability::advance_with_offer(moniker, self.availability_state, offer)?;
        self.advance(moniker, offer.rights.clone(), offer.subdir.clone())
    }

    fn advance_with_expose(
        &mut self,
        moniker: &ExtendedMoniker,
        expose: &ExposeDirectoryDecl,
    ) -> Result<(), RoutingError> {
        self.availability_state =
            availability::advance_with_expose(moniker, self.availability_state, expose)?;
        self.advance(moniker, expose.rights.clone(), expose.subdir.clone())
    }

    fn advance(
        &mut self,
        moniker: &ExtendedMoniker,
        rights: Option<fio::Operations>,
        mut subdir: RelativePath,
    ) -> Result<(), RoutingError> {
        self.rights = self.rights.advance(rights.map(|r| RightsWalker::new(r, moniker.clone())))?;
        subdir.extend(self.subdir.clone());
        self.subdir = subdir;
        Ok(())
    }

    fn finalize(
        &mut self,
        rights: RightsWalker,
        mut subdir: RelativePath,
    ) -> Result<(), RoutingError> {
        self.rights = self.rights.finalize(Some(rights))?;
        subdir.extend(self.subdir.clone());
        self.subdir = subdir;
        Ok(())
    }
}

impl OfferVisitor for DirectoryState {
    fn visit(
        &mut self,
        moniker: &ExtendedMoniker,
        offer: &cm_rust::OfferDecl,
    ) -> Result<(), RoutingError> {
        match offer {
            cm_rust::OfferDecl::Directory(dir) => match dir.source {
                OfferSource::Framework => self.finalize(
                    RightsWalker::new(fio::RX_STAR_DIR, moniker.clone()),
                    dir.subdir.clone(),
                ),
                _ => self.advance_with_offer(moniker, dir),
            },
            _ => Ok(()),
        }
    }
}

impl ExposeVisitor for DirectoryState {
    fn visit(
        &mut self,
        moniker: &ExtendedMoniker,
        expose: &cm_rust::ExposeDecl,
    ) -> Result<(), RoutingError> {
        match expose {
            cm_rust::ExposeDecl::Directory(dir) => match dir.source {
                ExposeSource::Framework => self.finalize(
                    RightsWalker::new(fio::RX_STAR_DIR, moniker.clone()),
                    dir.subdir.clone(),
                ),
                _ => self.advance_with_expose(moniker, dir),
            },
            _ => Ok(()),
        }
    }
}

impl CapabilityVisitor for DirectoryState {
    fn visit(
        &mut self,
        moniker: &ExtendedMoniker,
        capability: &cm_rust::CapabilityDecl,
    ) -> Result<(), RoutingError> {
        match capability {
            cm_rust::CapabilityDecl::Directory(dir) => {
                self.finalize(RightsWalker::new(dir.rights, moniker.clone()), Default::default())
            }
            _ => Ok(()),
        }
    }
}

/// Routes a Directory capability from `target` to its source, starting from `use_decl`.
/// Returns the capability source, along with a `DirectoryState` accumulated from traversing
/// the route.
async fn route_directory<C>(
    use_decl: UseDirectoryDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match use_decl.source {
        UseSource::Self_ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources = Sources::new(CapabilityTypeName::Dictionary).component();
            let source = legacy_router::route_from_self(
                use_decl.into(),
                target.clone(),
                allowed_sources,
                &mut availability_visitor,
                mapper,
            )
            .await?;
            Ok(RouteSource::new(source))
        }
        _ => {
            let mut state = DirectoryState::new(
                RightsWalker::new(use_decl.rights, target.moniker().clone()),
                use_decl.subdir.clone(),
                &use_decl.availability,
            );
            if let UseSource::Framework = &use_decl.source {
                state.finalize(
                    RightsWalker::new(fio::RX_STAR_DIR, target.moniker().clone()),
                    Default::default(),
                )?;
            }
            let allowed_sources =
                Sources::new(CapabilityTypeName::Directory).framework().namespace().component();
            let source = legacy_router::route_from_use(
                use_decl.into(),
                target.clone(),
                allowed_sources,
                &mut state,
                mapper,
            )
            .await?;

            target.policy_checker().can_route_capability(&source, target.moniker())?;
            Ok(RouteSource::new_with_relative_path(source, state.subdir))
        }
    }
}

/// Routes a Directory capability from `target` to its source, starting from `expose_decl`.
/// Returns the capability source, along with a `DirectoryState` accumulated from traversing
/// the route.
async fn route_directory_from_expose<C>(
    expose_decl: ExposeDirectoryDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut state = DirectoryState {
        rights: WalkState::new(),
        subdir: Default::default(),
        availability_state: expose_decl.availability.into(),
    };
    let allowed_sources =
        Sources::new(CapabilityTypeName::Directory).framework().namespace().component();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources,
        &mut state,
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new_with_relative_path(source, state.subdir))
}

/// Verifies that the given component is in the index if its `storage_id` is StaticInstanceId.
/// - On success, Ok(()) is returned
/// - RoutingError::ComponentNotInIndex is returned on failure.
pub async fn verify_instance_in_component_id_index<C>(
    source: &CapabilitySource,
    instance: &Arc<C>,
) -> Result<(), RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let (storage_decl, source_moniker) = match source {
        CapabilitySource::Component(ComponentSource {
            capability: ComponentCapability::Storage(storage_decl),
            moniker,
        }) => (storage_decl, moniker.clone()),
        CapabilitySource::Void(VoidSource { .. }) => return Ok(()),
        _ => unreachable!("unexpected storage source"),
    };

    if storage_decl.storage_id == fdecl::StorageId::StaticInstanceId
        && instance.component_id_index().id_for_moniker(instance.moniker()).is_none()
    {
        return Err(RoutingError::ComponentNotInIdIndex {
            source_moniker,
            target_name: instance.moniker().leaf().cloned(),
        });
    }
    Ok(())
}

/// Routes a Storage capability from `target` to its source, starting from `use_decl`.
/// Returns the StorageDecl and the storage component's instance.
pub async fn route_to_storage_decl<C>(
    use_decl: UseStorageDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = use_decl.availability;
    let allowed_sources = Sources::new(CapabilityTypeName::Storage).component();
    let source = legacy_router::route_from_use(
        use_decl.into(),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await?;
    Ok(source)
}

/// Routes a Storage capability from `target` to its source, starting from `use_decl`.
/// The backing Directory capability is then routed to its source.
async fn route_storage<C>(
    use_decl: UseStorageDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let source = route_to_storage_decl(use_decl, &target, mapper).await?;
    verify_instance_in_component_id_index(&source, target).await?;
    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// Routes the backing Directory capability of a Storage capability from `target` to its source,
/// starting from `storage_decl`.
async fn route_storage_backing_directory<C>(
    storage_decl: StorageDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    // Storage rights are always READ+WRITE.
    let mut state = DirectoryState::new(
        RightsWalker::new(fio::RW_STAR_DIR, target.moniker().clone()),
        Default::default(),
        &Availability::Required,
    );
    let allowed_sources = Sources::new(CapabilityTypeName::Directory).component().namespace();
    let source = legacy_router::route_from_registration(
        StorageDeclAsRegistration::from(storage_decl.clone()),
        target.clone(),
        allowed_sources,
        &mut state,
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;

    Ok(RouteSource::new_with_relative_path(source, state.subdir))
}

/// Finds a Configuration capability that matches the given use.
async fn route_config<C>(
    use_decl: UseConfigurationDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = Sources::new(CapabilityTypeName::Config).component().capability();
    let mut availability_visitor = use_decl.availability().clone();
    let source = legacy_router::route_from_use(
        use_decl.clone().into(),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await;
    // If the route was not found, but it's a transitional availability then return
    // a successful Void capability.
    let source = match source {
        Ok(s) => s,
        Err(e) => {
            if *use_decl.availability() == Availability::Transitional
                && e.as_zx_status() == zx::Status::NOT_FOUND
            {
                CapabilitySource::Void(VoidSource {
                    capability: InternalCapability::Config(use_decl.source_name),
                    moniker: target.moniker().clone(),
                })
            } else {
                return Err(e);
            }
        }
    };

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// Routes an EventStream capability from `target` to its source, starting from `use_decl`.
///
/// If the capability is not allowed to be routed to the `target`, per the
/// [`crate::model::policy::GlobalPolicyChecker`], then an error is returned.
pub async fn route_event_stream<C>(
    use_decl: UseEventStreamDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = Sources::new(CapabilityTypeName::EventStream).builtin();
    let mut availability_visitor = use_decl.availability;
    let source = legacy_router::route_from_use(
        use_decl.into(),
        target.clone(),
        allowed_sources,
        &mut availability_visitor,
        mapper,
    )
    .await?;
    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// Intermediate type to masquerade as Registration-style routing start point for the storage
/// backing directory capability.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDeclAsRegistration {
    source: RegistrationSource,
    name: Name,
}

impl From<StorageDecl> for StorageDeclAsRegistration {
    fn from(decl: StorageDecl) -> Self {
        Self {
            name: decl.backing_dir,
            source: match decl.source {
                StorageDirectorySource::Parent => RegistrationSource::Parent,
                StorageDirectorySource::Self_ => RegistrationSource::Self_,
                StorageDirectorySource::Child(child) => RegistrationSource::Child(child),
            },
        }
    }
}

impl SourceName for StorageDeclAsRegistration {
    fn source_name(&self) -> &Name {
        &self.name
    }
}

impl RegistrationDeclCommon for StorageDeclAsRegistration {
    const TYPE: &'static str = "storage";

    fn source(&self) -> &RegistrationSource {
        &self.source
    }
}

/// An umbrella type for registration decls, making it more convenient to record route
/// maps for debug use.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(FromEnum, Debug, Clone, PartialEq, Eq)]
pub enum RegistrationDecl {
    Resolver(ResolverRegistration),
    Runner(RunnerRegistration),
    Debug(DebugRegistration),
    Directory(StorageDeclAsRegistration),
}

impl From<&RegistrationDecl> for cm_rust::CapabilityTypeName {
    fn from(registration: &RegistrationDecl) -> Self {
        match registration {
            RegistrationDecl::Directory(_) => Self::Directory,
            RegistrationDecl::Resolver(_) => Self::Resolver,
            RegistrationDecl::Runner(_) => Self::Runner,
            RegistrationDecl::Debug(_) => Self::Protocol,
        }
    }
}

// Error trait impls

impl ErrorNotFoundFromParent for cm_rust::UseDecl {
    fn error_not_found_from_parent(moniker: Moniker, capability_name: Name) -> RoutingError {
        RoutingError::UseFromParentNotFound { moniker, capability_id: capability_name.into() }
    }
}

impl ErrorNotFoundFromParent for DebugRegistration {
    fn error_not_found_from_parent(moniker: Moniker, capability_name: Name) -> RoutingError {
        RoutingError::EnvironmentFromParentNotFound {
            moniker,
            capability_name,
            capability_type: DebugRegistration::TYPE.to_string(),
        }
    }
}

impl ErrorNotFoundInChild for DebugRegistration {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::EnvironmentFromChildExposeNotFound {
            moniker,
            child_moniker,
            capability_name,
            capability_type: DebugRegistration::TYPE.to_string(),
        }
    }
}

impl ErrorNotFoundInChild for cm_rust::UseDecl {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::UseFromChildExposeNotFound {
            child_moniker,
            moniker,
            capability_id: capability_name.into(),
        }
    }
}

impl ErrorNotFoundInChild for cm_rust::ExposeDecl {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::ExposeFromChildExposeNotFound {
            moniker,
            child_moniker,
            capability_id: capability_name.into(),
        }
    }
}

impl ErrorNotFoundInChild for cm_rust::OfferDecl {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::OfferFromChildExposeNotFound {
            moniker,
            child_moniker,
            capability_id: capability_name.into(),
        }
    }
}

impl ErrorNotFoundFromParent for cm_rust::OfferDecl {
    fn error_not_found_from_parent(moniker: Moniker, capability_name: Name) -> RoutingError {
        RoutingError::OfferFromParentNotFound { moniker, capability_id: capability_name.into() }
    }
}

impl ErrorNotFoundInChild for StorageDeclAsRegistration {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::StorageFromChildExposeNotFound {
            moniker,
            child_moniker,
            capability_id: capability_name.into(),
        }
    }
}

impl ErrorNotFoundFromParent for StorageDeclAsRegistration {
    fn error_not_found_from_parent(moniker: Moniker, capability_name: Name) -> RoutingError {
        RoutingError::StorageFromParentNotFound { moniker, capability_id: capability_name.into() }
    }
}

impl ErrorNotFoundFromParent for RunnerRegistration {
    fn error_not_found_from_parent(moniker: Moniker, capability_name: Name) -> RoutingError {
        RoutingError::UseFromEnvironmentNotFound {
            moniker,
            capability_name,
            capability_type: "runner".to_string(),
        }
    }
}

impl ErrorNotFoundInChild for RunnerRegistration {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::EnvironmentFromChildExposeNotFound {
            moniker,
            child_moniker,
            capability_name,
            capability_type: "runner".to_string(),
        }
    }
}

impl ErrorNotFoundFromParent for ResolverRegistration {
    fn error_not_found_from_parent(moniker: Moniker, capability_name: Name) -> RoutingError {
        RoutingError::EnvironmentFromParentNotFound {
            moniker,
            capability_name,
            capability_type: "resolver".to_string(),
        }
    }
}

impl ErrorNotFoundInChild for ResolverRegistration {
    fn error_not_found_in_child(
        moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError {
        RoutingError::EnvironmentFromChildExposeNotFound {
            moniker,
            child_moniker,
            capability_name,
            capability_type: "resolver".to_string(),
        }
    }
}
