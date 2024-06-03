// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{model::component::ComponentInstance, sandbox_util::LaunchTaskOnReceive},
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
    routing::capability_source::{CapabilitySource, InternalCapability},
    sandbox::Dict,
    std::sync::Arc,
};

pub mod binder;
pub mod controller;
pub mod factory;
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
    let mut framework_dictionary = Dict::new();
    add_hook_protocol::<fcomponent::BinderMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fsandbox::FactoryMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fcomponent::IntrospectorMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fsys::LifecycleControllerMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fcomponent::NamespaceMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fcomponent::RealmMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fsys::RealmQueryMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fsys::RouteValidatorMarker>(component, &mut framework_dictionary);
    framework_dictionary
}

fn add_hook_protocol<P: DiscoverableProtocolMarker>(
    component: &Arc<ComponentInstance>,
    dict: &mut Dict,
) {
    dict.insert(
        P::PROTOCOL_NAME.parse().unwrap(),
        LaunchTaskOnReceive::new_hook_launch_task(
            component,
            CapabilitySource::Framework {
                capability: InternalCapability::Protocol(P::PROTOCOL_NAME.parse().unwrap()),
                component: component.into(),
            },
        )
        .into_router()
        .into(),
    )
    .unwrap();
}
