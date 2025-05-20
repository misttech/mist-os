// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::sandbox_util::LaunchTaskOnReceive;
use fidl::endpoints::DiscoverableProtocolMarker;
use futures::future::BoxFuture;
use routing::capability_source::{CapabilitySource, FrameworkSource, InternalCapability};
use routing::component_instance::ComponentInstanceInterface;
use sandbox::Dict;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_internal as finternal,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_sys2 as fsys,
};

pub mod binder;
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

/// Returns a dictionary containing routers for all of the framework capabilities scoped to the
/// given component.
pub fn build_framework_dictionary(component: &Arc<ComponentInstance>) -> Dict {
    let framework_dictionary = Dict::new();
    add_hook_protocol::<fcomponent::BinderMarker>(component, &framework_dictionary);
    add_hook_protocol::<fsandbox::CapabilityStoreMarker>(component, &framework_dictionary);
    add_hook_protocol::<fsys::ConfigOverrideMarker>(component, &framework_dictionary);
    add_hook_protocol::<fcomponent::IntrospectorMarker>(component, &framework_dictionary);
    add_hook_protocol::<fsys::LifecycleControllerMarker>(component, &framework_dictionary);
    add_hook_protocol::<fcomponent::NamespaceMarker>(component, &framework_dictionary);
    add_hook_protocol::<fcomponent::RealmMarker>(component, &framework_dictionary);
    add_hook_protocol::<fsys::RealmQueryMarker>(component, &framework_dictionary);
    add_hook_protocol::<fsys::RouteValidatorMarker>(component, &framework_dictionary);
    add_protocol::<finternal::ComponentSandboxRetrieverMarker>(
        component,
        &framework_dictionary,
        component_sandbox_retriever::serve,
    );
    framework_dictionary
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
