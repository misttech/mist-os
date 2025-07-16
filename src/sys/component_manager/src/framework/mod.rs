// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, WeakComponentInstance, WeakExtendedInstance};
use crate::sandbox_util::LaunchTaskOnReceive;
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::error::RoutingError;
use async_trait::async_trait;
use fidl::endpoints::DiscoverableProtocolMarker;
use futures::future::BoxFuture;
use moniker::Moniker;
use router_error::RouterError;
use routing::capability_source::{CapabilitySource, FrameworkSource, InternalCapability};
use sandbox::{Dict, Request, Routable, Router, RouterResponse};
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_internal as finternal,
    fidl_fuchsia_component_runtime as fruntime, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
};

pub mod binder;
pub mod capability_factory;
pub mod capability_store;
pub mod component_sandbox_retriever;
pub mod config_override;
pub mod controller;
pub mod introspector;
pub mod lifecycle_controller;
pub mod namespace;
pub mod pkg_dir;
pub mod realm;
pub mod realm_query;
pub mod route_validator;

/// Returns a router that returns a dictionary containing routers for all of the framework
/// capabilities scoped to the component `scope`. Making this a Router instead of a Dict
/// saves memory compared to generating a Dict of framework capabilities for each component up
/// front.
pub(crate) fn get_framework_router(scope: &Arc<ComponentInstance>) -> Router<Dict> {
    Router::new(FrameworkRouter { scope: scope.moniker.clone() })
}

struct FrameworkRouter {
    scope: Moniker,
}

#[async_trait]
impl Routable<Dict> for FrameworkRouter {
    async fn route(
        &self,
        request: Option<Request>,
        _debug: bool,
    ) -> Result<RouterResponse<Dict>, RouterError> {
        let request = request.ok_or(RouterError::InvalidArgs)?;
        let target = request
            .target
            .inner
            .as_any()
            .downcast_ref::<WeakExtendedInstance>()
            .ok_or(RouterError::Unknown)?;
        let component = match target {
            WeakExtendedInstance::Component(c) => c,
            WeakExtendedInstance::AboveRoot(_) => return Err(RouterError::InvalidArgs),
        };
        let component = component.upgrade().map_err(RoutingError::from)?;
        if component.moniker != self.scope {
            return Err(RouterError::InvalidArgs);
        }

        let framework_dictionary = Dict::new();
        add_hook_protocol::<fcomponent::BinderMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fsandbox::CapabilityStoreMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fsys::ConfigOverrideMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fcomponent::IntrospectorMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fsys::LifecycleControllerMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fcomponent::NamespaceMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fcomponent::RealmMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fsys::RealmQueryMarker>(&component, &framework_dictionary);
        add_hook_protocol::<fsys::RouteValidatorMarker>(&component, &framework_dictionary);
        add_protocol::<finternal::ComponentSandboxRetrieverMarker>(
            &component,
            &framework_dictionary,
            component_sandbox_retriever::serve,
        );
        add_protocol::<fruntime::CapabilityFactoryMarker>(
            &component,
            &framework_dictionary,
            capability_factory::serve,
        );
        Ok(RouterResponse::Capability(framework_dictionary))
    }
}

fn add_hook_protocol<P: DiscoverableProtocolMarker>(
    component: &Arc<ComponentInstance>,
    dict: &Dict,
) {
    dict.insert(
        P::PROTOCOL_NAME.parse().unwrap(),
        LaunchTaskOnReceive::new_hook_launch_task(
            component,
            CapabilitySource::Framework(FrameworkSource {
                capability: InternalCapability::Protocol(P::PROTOCOL_NAME.parse().unwrap()),
                moniker: component.moniker.clone(),
            }),
        )
        .into_router()
        .into(),
    )
    .unwrap();
}

fn add_protocol<P: DiscoverableProtocolMarker>(
    component: &Arc<ComponentInstance>,
    dict: &Dict,
    task_to_launch: impl Fn(
            zx::Channel,
            /*target: */ WeakComponentInstance,
            /*scope: */ WeakComponentInstance,
        ) -> BoxFuture<'static, Result<(), anyhow::Error>>
        + Sync
        + Send
        + 'static,
) {
    let capability_source = CapabilitySource::Framework(FrameworkSource {
        capability: InternalCapability::Protocol(P::PROTOCOL_NAME.parse().unwrap()),
        moniker: component.moniker.clone(),
    });
    // Dictionary inserts succeed even when they return an error.
    let source = component.as_weak();
    dict.insert(
        P::PROTOCOL_NAME.parse().unwrap(),
        LaunchTaskOnReceive::new(
            capability_source,
            component.nonblocking_task_group().as_weak(),
            format!("framework dispatcher for {}", P::PROTOCOL_NAME),
            Some(component.context.policy().clone()),
            Arc::new(move |chan, target| task_to_launch(chan, target, source.clone())),
        )
        .into_router()
        .into(),
    )
    .unwrap();
}
