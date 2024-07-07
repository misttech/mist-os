// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::ComponentInstanceForAnalyzer;
use ::routing::component_instance::{
    ComponentInstanceInterface, WeakComponentInstanceInterface, WeakExtendedInstanceInterface,
};
use ::routing::error::RoutingError;
use ::routing::DictExt;
use async_trait::async_trait;
use cm_rust::ComponentDecl;
use cm_types::RelativePath;
use fidl::endpoints::DiscoverableProtocolMarker;
use moniker::ChildName;
use router_error::RouterError;
use sandbox::{Capability, Data, Dict, Request, Routable, Router, WeakInstanceToken};
use std::collections::HashMap;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
};

pub fn build_framework_dictionary() -> Dict {
    let framework_dict = Dict::new();
    for protocol_name in &[
        fcomponent::BinderMarker::PROTOCOL_NAME,
        fsandbox::FactoryMarker::PROTOCOL_NAME,
        fcomponent::IntrospectorMarker::PROTOCOL_NAME,
        fsys::LifecycleControllerMarker::PROTOCOL_NAME,
        fcomponent::NamespaceMarker::PROTOCOL_NAME,
        fcomponent::RealmMarker::PROTOCOL_NAME,
        fsys::RealmQueryMarker::PROTOCOL_NAME,
        fsys::RouteValidatorMarker::PROTOCOL_NAME,
    ] {
        framework_dict
            .insert_capability(
                &cm_types::Name::new(*protocol_name).unwrap(),
                // Note: a future commit will add a capability source of "program" and use it here.
                Router::new_ok(Data::String("TODO".to_string())).into(),
            )
            .expect("failed to insert framework capability into dictionary");
    }
    framework_dict
}

pub fn build_capability_sourced_capabilities_dictionary(decl: &cm_rust::ComponentDecl) -> Dict {
    let output = Dict::new();
    for capability in &decl.capabilities {
        if let cm_rust::CapabilityDecl::Storage(storage_decl) = capability {
            output
                .insert_capability(
                    &storage_decl.name,
                    // Note: a future commit will replace this with CapabilitySource::Capability
                    Router::new_ok(Data::String("TODO".to_string())).into(),
                )
                .expect("failed to insert capability backed capability into dictionary");
        }
    }
    output
}

pub fn new_program_router(
    _weak_component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
    _relative_path: RelativePath,
) -> Router {
    Router::new_ok(Data::String("TODO: this comes from a program".to_string()))
}

pub fn new_outgoing_dir_router(
    component: &Arc<ComponentInstanceForAnalyzer>,
    _decl: &cm_rust::ComponentDecl,
    _capability: &cm_rust::CapabilityDecl,
) -> Router {
    let weak_component_token = WeakInstanceToken {
        inner: Arc::new(WeakExtendedInstanceInterface::Component(
            WeakComponentInstanceInterface::new(component),
        )),
    };
    Router::new_ok(weak_component_token)
}

pub(crate) fn program_output_router(component: &Arc<ComponentInstanceForAnalyzer>) -> Router {
    struct ProgramOutputRouter {
        weak_component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
    }
    #[async_trait]
    impl Routable for ProgramOutputRouter {
        async fn route(&self, _request: Request) -> Result<Capability, RouterError> {
            let component =
                self.weak_component.upgrade().expect("part of component tree was dropped");
            let sandbox =
                component.component_sandbox().await.expect("getting sandbox should be infallible");
            Ok(sandbox.program_output_dict.clone().into())
        }
    }

    let weak_component = WeakComponentInstanceInterface::new(component);
    Router::new(ProgramOutputRouter { weak_component })
}

pub(crate) fn static_children_component_output_dictionary_routers(
    component: &Arc<ComponentInstanceForAnalyzer>,
    decl: &ComponentDecl,
) -> HashMap<ChildName, Router> {
    struct ChildrenComponentOutputRouters {
        weak_component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
        child_name: ChildName,
    }
    #[async_trait]
    impl Routable for ChildrenComponentOutputRouters {
        async fn route(&self, _request: Request) -> Result<Capability, RouterError> {
            let component =
                self.weak_component.upgrade().expect("part of component tree was dropped");
            let child = component
                .children
                .read()
                .expect("failed to get lock")
                .get(&self.child_name)
                .cloned()
                .ok_or(RouterError::NotFound(Arc::new(
                    RoutingError::BedrockNotPresentInDictionary {
                        name: format!("{}", &self.child_name),
                    },
                )))?;
            let component_output_dict = child.sandbox.component_output_dict.clone();
            Ok(component_output_dict.into())
        }
    }

    let weak_component = WeakComponentInstanceInterface::new(component);
    let mut output = HashMap::new();
    for child_decl in decl.children.iter() {
        let child_name = ChildName::new(child_decl.name.clone(), None);
        output.insert(
            child_name.clone(),
            Router::new(ChildrenComponentOutputRouters {
                weak_component: weak_component.clone(),
                child_name,
            }),
        );
    }
    output
}
