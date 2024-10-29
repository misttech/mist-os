// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::ComponentInstance;
use crate::sandbox_util::LaunchTaskOnReceive;
use fidl::endpoints::DiscoverableProtocolMarker;
use routing::capability_source::{CapabilitySource, FrameworkSource, InternalCapability};
use sandbox::Dict;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
};

pub mod binder;
pub mod capability_store;
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
    let mut framework_dictionary = Dict::new();
    add_hook_protocol::<fcomponent::BinderMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fsandbox::CapabilityStoreMarker>(component, &mut framework_dictionary);
    add_hook_protocol::<fsys::ConfigOverrideMarker>(component, &mut framework_dictionary);
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
