// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod availability;
pub mod capability_source;
pub mod collection;
pub mod component_instance;
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

use fuchsia_zircon_status as zx;
use {
    crate::{
        capability_source::{CapabilitySource, ComponentCapability, InternalCapability},
        component_instance::{
            ComponentInstanceInterface, ExtendedInstanceInterface, TopInstanceInterface,
        },
        environment::DebugRegistration,
        error::RoutingError,
        legacy_router::{
            AllowedSourcesBuilder, CapabilityVisitor, ErrorNotFoundFromParent,
            ErrorNotFoundInChild, ExposeVisitor, NoopVisitor, OfferVisitor, RouteBundle,
        },
        mapper::DebugRouteMapper,
        path::PathBufExt,
        rights::Rights,
        walk_state::WalkState,
    },
    bedrock_error::Explain,
    cm_rust::{
        Availability, CapabilityTypeName, ExposeConfigurationDecl, ExposeDecl, ExposeDeclCommon,
        ExposeDirectoryDecl, ExposeProtocolDecl, ExposeResolverDecl, ExposeRunnerDecl,
        ExposeServiceDecl, ExposeSource, OfferConfigurationDecl, OfferDirectoryDecl,
        OfferEventStreamDecl, OfferProtocolDecl, OfferResolverDecl, OfferRunnerDecl,
        OfferServiceDecl, OfferSource, OfferStorageDecl, RegistrationDeclCommon,
        RegistrationSource, ResolverRegistration, RunnerRegistration, SourceName, StorageDecl,
        StorageDirectorySource, UseConfigurationDecl, UseDecl, UseDeclCommon, UseDirectoryDecl,
        UseEventStreamDecl, UseProtocolDecl, UseRunnerDecl, UseServiceDecl, UseSource,
        UseStorageDecl,
    },
    cm_types::Name,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio,
    from_enum::FromEnum,
    moniker::{ChildName, Moniker},
    std::{path::PathBuf, sync::Arc},
    tracing::warn,
};

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

impl From<Vec<&ExposeDecl>> for RouteRequest {
    fn from(exposes: Vec<&ExposeDecl>) -> Self {
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
                Self::ExposeProtocol(e.clone())
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
                Self::ExposeService(RouteBundle::from_exposes(exposes))
            }
            ExposeDecl::Directory(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Self::ExposeDirectory(e.clone())
            }
            ExposeDecl::Runner(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Self::ExposeRunner(e.clone())
            }
            ExposeDecl::Resolver(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Self::ExposeResolver(e.clone())
            }
            ExposeDecl::Config(e) => {
                assert!(exposes.len() == 1, "multiple exposes");
                Self::ExposeConfig(e.clone())
            }
            ExposeDecl::Dictionary(_) => {
                todo!("https://fxbug.dev/301674053: implement dictionary routing");
            }
        }
    }
}

impl RouteRequest {
    /// Returns the availability of the RouteRequest if it is a `use` capability declaration
    /// and supports availability.
    pub fn target_use_availability(&self) -> Option<Availability> {
        use crate::RouteRequest::*;
        match self {
            UseDirectory(UseDirectoryDecl { availability, .. })
            | UseEventStream(UseEventStreamDecl { availability, .. })
            | UseProtocol(UseProtocolDecl { availability, .. })
            | UseService(UseServiceDecl { availability, .. })
            | UseConfig(UseConfigurationDecl { availability, .. })
            | UseStorage(UseStorageDecl { availability, .. }) => Some(*availability),

            OfferRunner(_)
            | OfferResolver(_)
            | ExposeDirectory(_)
            | ExposeProtocol(_)
            | ExposeService(_)
            | ExposeRunner(_)
            | ExposeResolver(_)
            | ExposeConfig(_)
            | Resolver(_)
            | StorageBackingDirectory(_)
            | UseRunner(_)
            | OfferDirectory(_)
            | OfferEventStream(_)
            | OfferProtocol(_)
            | OfferConfig(_)
            | OfferStorage(_)
            | OfferService(_) => None,
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
        }
    }
}

/// The data returned after successfully routing a capability to its source.
#[derive(Debug)]
pub struct RouteSource<C: ComponentInstanceInterface> {
    pub source: CapabilitySource<C>,
    pub relative_path: PathBuf,
}

impl<C: ComponentInstanceInterface> RouteSource<C> {
    pub fn new(source: CapabilitySource<C>) -> Self {
        Self { source, relative_path: "".into() }
    }

    pub fn new_with_relative_path(source: CapabilitySource<C>, relative_path: PathBuf) -> Self {
        Self { source, relative_path }
    }
}

/// Routes a capability to its source.
///
/// If the capability is not allowed to be routed to the `target`, per the
/// [`crate::model::policy::GlobalPolicyChecker`], then an error is returned.
///
/// The `mapper` is invoked on every step in the routing process and can
/// be used to record the routing steps.
pub async fn route_capability<C>(
    request: RouteRequest,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match request {
        // Route from an ExposeDecl
        RouteRequest::ExposeDirectory(expose_directory_decl) => {
            route_directory_from_expose(expose_directory_decl, target, mapper).await
        }
        RouteRequest::ExposeProtocol(expose_protocol_decl) => {
            route_protocol_from_expose(expose_protocol_decl, target, mapper).await
        }
        RouteRequest::ExposeService(expose_bundle) => {
            route_service_from_expose(expose_bundle, target, mapper).await
        }
        RouteRequest::ExposeRunner(expose_runner_decl) => {
            route_runner_from_expose(expose_runner_decl, target, mapper).await
        }
        RouteRequest::ExposeResolver(expose_resolver_decl) => {
            route_resolver_from_expose(expose_resolver_decl, target, mapper).await
        }
        RouteRequest::ExposeConfig(expose_config_decl) => {
            route_config_from_expose(expose_config_decl, target, mapper).await
        }

        // Route a resolver or runner from an environment
        RouteRequest::Resolver(resolver_registration) => {
            route_resolver(resolver_registration, target, mapper).await
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
            route_protocol(use_protocol_decl, target, mapper).await
        }
        RouteRequest::UseService(use_service_decl) => {
            route_service(use_service_decl, target, mapper).await
        }
        RouteRequest::UseStorage(use_storage_decl) => {
            route_storage(use_storage_decl, target, mapper).await
        }
        RouteRequest::UseRunner(use_runner_decl) => {
            route_runner(use_runner_decl, target, mapper).await
        }
        RouteRequest::UseConfig(use_config_decl) => {
            route_config(use_config_decl, target, mapper).await
        }

        // Route from a OfferDecl
        RouteRequest::OfferProtocol(offer_protocol_decl) => {
            route_protocol_from_offer(offer_protocol_decl, target, mapper).await
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
            route_runner_from_offer(offer_runner_decl, target, mapper).await
        }
        RouteRequest::OfferResolver(offer_resolver_decl) => {
            route_resolver_from_offer(offer_resolver_decl, target, mapper).await
        }
        RouteRequest::OfferConfig(offer) => route_config_from_offer(offer, target, mapper).await,
    }
}

pub enum Never {}

/// Routes a Protocol capability from `target` to its source, starting from `offer_decl`.
async fn route_protocol_from_offer<C>(
    offer_decl: OfferProtocolDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = offer_decl.availability;
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Protocol)
        .framework(InternalCapability::Protocol)
        .builtin()
        .namespace()
        .component()
        .capability();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut availability_visitor,
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

/// Routes a Directory capability from `target` to its source, starting from `offer_decl`.
/// Returns the capability source, along with a `DirectoryState` accumulated from traversing
/// the route.
async fn route_directory_from_offer<C>(
    offer_decl: OfferDirectoryDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut state = DirectoryState {
        rights: WalkState::new(),
        subdir: PathBuf::new(),
        availability_state: offer_decl.availability.into(),
    };
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Directory)
        .framework(InternalCapability::Directory)
        .namespace()
        .component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    // TODO(https://fxbug.dev/42124541): Figure out how to set the availability when `offer_bundle` contains
    // multiple routes with different availabilities. It's possible that manifest validation should
    // disallow this. For now, just pick the first.
    let mut availability_visitor = offer_bundle.iter().next().unwrap().availability;
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Service).component().collection();
    let source = legacy_router::route_from_offer(
        offer_bundle.map(Into::into),
        target.clone(),
        allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::EventStream).builtin();

    let mut availability_visitor = offer_decl.availability;
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = offer_decl.availability;
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Storage).component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut availability_visitor,
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

async fn route_runner_from_offer<C>(
    offer_decl: OfferRunnerDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Runner).builtin().component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

async fn route_resolver_from_offer<C>(
    offer_decl: OfferResolverDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Resolver).builtin().component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

async fn route_config_from_offer<C>(
    offer_decl: OfferConfigurationDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Config).builtin().component();
    let source = legacy_router::route_from_offer(
        RouteBundle::from_offer(offer_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;
    Ok(RouteSource::new(source))
}

/// Routes a Protocol capability from `target` to its source, starting from `use_decl`.
async fn route_protocol<C>(
    use_decl: UseProtocolDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Protocol)
        .framework(InternalCapability::Protocol)
        .builtin()
        .namespace()
        .component()
        .capability();
    match use_decl.source {
        UseSource::Debug => {
            // Find the component instance in which the debug capability was registered with the environment.
            let (env_component_instance, registration_decl) = match target
                .environment()
                .get_debug_capability(&use_decl.source_name)
                .map_err(|err| {
                    warn!(?use_decl, %err, "route_protocol error 1");
                    err
                })? {
                Some((
                    ExtendedInstanceInterface::Component(env_component_instance),
                    _env_name,
                    reg,
                )) => (env_component_instance, reg),
                Some((ExtendedInstanceInterface::AboveRoot(_), _, _)) => {
                    // Root environment.
                    return Err(RoutingError::UseFromRootEnvironmentNotAllowed {
                        moniker: target.moniker().clone(),
                        capability_name: use_decl.source_name.clone(),
                        capability_type: DebugRegistration::TYPE.to_string(),
                    }
                    .into());
                }
                None => {
                    return Err(RoutingError::UseFromEnvironmentNotFound {
                        moniker: target.moniker().clone(),
                        capability_name: use_decl.source_name.clone(),
                        capability_type: DebugRegistration::TYPE.to_string(),
                    }
                    .into());
                }
            };

            let mut availability_visitor = use_decl.availability;
            let source = legacy_router::route_from_registration(
                registration_decl,
                env_component_instance.clone(),
                allowed_sources.build(),
                &mut availability_visitor,
                mapper,
            )
            .await
            .map_err(|err| {
                warn!(?use_decl, %err, "route_protocol error 2");
                err
            })?;

            return Ok(RouteSource::new(source));
        }
        UseSource::Self_ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources =
                AllowedSourcesBuilder::new(CapabilityTypeName::Protocol).component();
            let source = legacy_router::route_from_self(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
                &mut availability_visitor,
                mapper,
            )
            .await?;
            Ok(RouteSource::new(source))
        }
        _ => {
            let mut availability_visitor = use_decl.availability;
            let source = legacy_router::route_from_use(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
                &mut availability_visitor,
                mapper,
            )
            .await?;

            target.policy_checker().can_route_capability(&source, target.moniker())?;
            Ok(RouteSource::new(source))
        }
    }
}

/// Routes a Protocol capability from `target` to its source, starting from `expose_decl`.
async fn route_protocol_from_expose<C>(
    expose_decl: ExposeProtocolDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = expose_decl.availability;
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Protocol)
        .framework(InternalCapability::Protocol)
        .builtin()
        .namespace()
        .component()
        .capability();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut availability_visitor,
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

async fn route_runner_from_expose<C>(
    expose_decl: ExposeRunnerDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Runner)
        .framework(InternalCapability::Runner)
        .builtin()
        .namespace()
        .component()
        .capability();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

async fn route_resolver_from_expose<C>(
    expose_decl: ExposeResolverDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Resolver)
        .framework(InternalCapability::Resolver)
        .builtin()
        .namespace()
        .component()
        .capability();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources.build(),
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

async fn route_config_from_expose<C>(
    expose_decl: ExposeConfigurationDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Config).component().capability();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match use_decl.source {
        UseSource::Self_ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources =
                AllowedSourcesBuilder::new(CapabilityTypeName::Service).component();
            let source = legacy_router::route_from_self(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
                &mut availability_visitor,
                mapper,
            )
            .await?;
            Ok(RouteSource::new(source))
        }
        _ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources =
                AllowedSourcesBuilder::new(CapabilityTypeName::Service).component().collection();
            let source = legacy_router::route_from_use(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = expose_bundle.availability().clone();
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Service).component().collection();
    let source = legacy_router::route_from_expose(
        expose_bundle.map(Into::into),
        target.clone(),
        allowed_sources.build(),
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
    rights: WalkState<Rights>,
    pub subdir: PathBuf,
    availability_state: Availability,
}

impl DirectoryState {
    fn new(
        operations: fio::Operations,
        subdir: Option<PathBuf>,
        availability: &Availability,
    ) -> Self {
        DirectoryState {
            rights: WalkState::at(operations.into()),
            subdir: subdir.unwrap_or_else(PathBuf::new),
            availability_state: availability.clone(),
        }
    }

    fn advance_with_offer(&mut self, offer: &OfferDirectoryDecl) -> Result<(), RoutingError> {
        self.availability_state = availability::advance_with_offer(self.availability_state, offer)?;
        self.advance(offer.rights.clone(), offer.subdir.clone())
    }

    fn advance_with_expose(&mut self, expose: &ExposeDirectoryDecl) -> Result<(), RoutingError> {
        self.availability_state =
            availability::advance_with_expose(self.availability_state, expose)?;
        self.advance(expose.rights.clone(), expose.subdir.clone())
    }

    fn advance(
        &mut self,
        rights: Option<fio::Operations>,
        subdir: Option<PathBuf>,
    ) -> Result<(), RoutingError> {
        self.rights = self.rights.advance(rights.map(Rights::from))?;
        let subdir = subdir.clone().unwrap_or_else(PathBuf::new);
        self.subdir = subdir.attach(&self.subdir);
        Ok(())
    }

    fn finalize(
        &mut self,
        rights: fio::Operations,
        subdir: Option<PathBuf>,
    ) -> Result<(), RoutingError> {
        self.rights = self.rights.finalize(Some(rights.into()))?;
        let subdir = subdir.clone().unwrap_or_else(PathBuf::new);
        self.subdir = subdir.attach(&self.subdir);
        Ok(())
    }
}

impl OfferVisitor for DirectoryState {
    fn visit(&mut self, offer: &cm_rust::OfferDecl) -> Result<(), RoutingError> {
        match offer {
            cm_rust::OfferDecl::Directory(dir) => match dir.source {
                OfferSource::Framework => self.finalize(fio::RW_STAR_DIR, dir.subdir.clone()),
                _ => self.advance_with_offer(dir),
            },
            _ => Ok(()),
        }
    }
}

impl ExposeVisitor for DirectoryState {
    fn visit(&mut self, expose: &cm_rust::ExposeDecl) -> Result<(), RoutingError> {
        match expose {
            cm_rust::ExposeDecl::Directory(dir) => match dir.source {
                ExposeSource::Framework => self.finalize(fio::RW_STAR_DIR, dir.subdir.clone()),
                _ => self.advance_with_expose(dir),
            },
            _ => Ok(()),
        }
    }
}

impl CapabilityVisitor for DirectoryState {
    fn visit(&mut self, capability: &cm_rust::CapabilityDecl) -> Result<(), RoutingError> {
        match capability {
            cm_rust::CapabilityDecl::Directory(dir) => self.finalize(dir.rights.clone(), None),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match use_decl.source {
        UseSource::Self_ => {
            let mut availability_visitor = use_decl.availability;
            let allowed_sources =
                AllowedSourcesBuilder::new(CapabilityTypeName::Dictionary).component();
            let source = legacy_router::route_from_self(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
                &mut availability_visitor,
                mapper,
            )
            .await?;
            Ok(RouteSource::new(source))
        }
        _ => {
            let mut state = DirectoryState::new(
                use_decl.rights.clone(),
                use_decl.subdir.clone(),
                &use_decl.availability,
            );
            if let UseSource::Framework = &use_decl.source {
                state.finalize(fio::RW_STAR_DIR, None)?;
            }
            let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Directory)
                .framework(InternalCapability::Directory)
                .namespace()
                .component();
            let source = legacy_router::route_from_use(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut state = DirectoryState {
        rights: WalkState::new(),
        subdir: PathBuf::new(),
        availability_state: expose_decl.availability.into(),
    };
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Directory)
        .framework(InternalCapability::Directory)
        .namespace()
        .component();
    let source = legacy_router::route_from_expose(
        RouteBundle::from_expose(expose_decl.into()),
        target.clone(),
        allowed_sources.build(),
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
pub fn verify_instance_in_component_id_index<C>(
    source: &CapabilitySource<C>,
    instance: &Arc<C>,
) -> Result<(), RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let (storage_decl, source_component) = match source {
        CapabilitySource::Component {
            capability: ComponentCapability::Storage(storage_decl),
            component,
        } => (storage_decl, component),
        CapabilitySource::Void { .. } => return Ok(()),
        _ => unreachable!("unexpected storage source"),
    };

    if storage_decl.storage_id == fdecl::StorageId::StaticInstanceId
        && instance.component_id_index().id_for_moniker(instance.moniker()).is_none()
    {
        return Err(RoutingError::ComponentNotInIdIndex {
            source_moniker: source_component.moniker.clone(),
            target_moniker: instance.moniker().clone(),
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
) -> Result<CapabilitySource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let mut availability_visitor = use_decl.availability;
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Storage).component();
    let source = legacy_router::route_from_use(
        use_decl.into(),
        target.clone(),
        allowed_sources.build(),
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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let source = route_to_storage_decl(use_decl, &target, mapper).await?;
    verify_instance_in_component_id_index(&source, target)?;
    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// Routes the backing Directory capability of a Storage capability from `target` to its source,
/// starting from `storage_decl`.
async fn route_storage_backing_directory<C>(
    storage_decl: StorageDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    // Storage rights are always READ+WRITE.
    let mut state = DirectoryState::new(fio::RW_STAR_DIR, None, &Availability::Required);
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Directory).component().namespace();
    let source = legacy_router::route_from_registration(
        StorageDeclAsRegistration::from(storage_decl.clone()),
        target.clone(),
        allowed_sources.build(),
        &mut state,
        mapper,
    )
    .await?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;

    Ok(RouteSource::new_with_relative_path(source, state.subdir))
}

/// Finds a Runner capability that matches `runner` in the `target`'s environment, and then
/// routes the Runner capability from the environment's component instance to its source.
async fn route_runner_from_environment<C>(
    runner: &Name,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Runner).builtin().component().build();
    let source = match target.environment().get_registered_runner(&runner)? {
        // The runner was registered in the environment of some component instance..
        Some((ExtendedInstanceInterface::Component(env_component_instance), registration_decl)) => {
            legacy_router::route_from_registration(
                registration_decl,
                env_component_instance,
                allowed_sources,
                &mut NoopVisitor::new(),
                mapper,
            )
            .await
        }
        // The runner was registered in the root environment.
        Some((ExtendedInstanceInterface::AboveRoot(top_instance), reg)) => {
            let internal_capability = allowed_sources
                .find_builtin_source(
                    reg.source_name(),
                    top_instance.builtin_capabilities(),
                    &mut NoopVisitor::new(),
                    mapper,
                )?
                .ok_or(RoutingError::register_from_component_manager_not_found(
                    reg.source_name().to_string(),
                ))?;
            Ok(CapabilitySource::Builtin {
                capability: internal_capability,
                top_instance: Arc::downgrade(&top_instance),
            })
        }
        None => Err(RoutingError::UseFromEnvironmentNotFound {
            moniker: target.moniker().clone(),
            capability_name: runner.clone(),
            capability_type: "runner".to_string(),
        }),
    }?;

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// Finds a Runner capability that matches the use of `runner`.
async fn route_runner<C>(
    use_decl: UseRunnerDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    match use_decl.source {
        UseSource::Environment => {
            route_runner_from_environment(use_decl.source_name(), target, mapper).await
        }
        _ => {
            let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::Runner)
                .framework(InternalCapability::Runner)
                .builtin()
                .capability()
                .component();
            let mut availability_visitor = use_decl.availability().clone();
            let source = legacy_router::route_from_use(
                use_decl.into(),
                target.clone(),
                allowed_sources.build(),
                &mut availability_visitor,
                mapper,
            )
            .await?;

            target.policy_checker().can_route_capability(&source, target.moniker())?;
            Ok(RouteSource::new(source))
        }
    }
}

/// Finds a Configuration capability that matches the given use.
async fn route_config<C>(
    use_decl: UseConfigurationDecl,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Config).component().capability();
    let mut availability_visitor = use_decl.availability().clone();
    let source = legacy_router::route_from_use(
        use_decl.clone().into(),
        target.clone(),
        allowed_sources.build(),
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
                CapabilitySource::Void {
                    capability: InternalCapability::Config(use_decl.source_name),
                    component: target.as_weak(),
                }
            } else {
                return Err(e);
            }
        }
    };

    target.policy_checker().can_route_capability(&source, target.moniker())?;
    Ok(RouteSource::new(source))
}

/// Routes a Resolver capability from `target` to its source, starting from `registration_decl`.
async fn route_resolver<C>(
    registration: ResolverRegistration,
    target: &Arc<C>,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources =
        AllowedSourcesBuilder::new(CapabilityTypeName::Resolver).builtin().component();
    let source = legacy_router::route_from_registration(
        registration,
        target.clone(),
        allowed_sources.build(),
        &mut NoopVisitor::new(),
        mapper,
    )
    .await?;

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
) -> Result<RouteSource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let allowed_sources = AllowedSourcesBuilder::new(CapabilityTypeName::EventStream).builtin();
    let mut availability_visitor = use_decl.availability;
    let source = legacy_router::route_from_use(
        use_decl.into(),
        target.clone(),
        allowed_sources.build(),
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
