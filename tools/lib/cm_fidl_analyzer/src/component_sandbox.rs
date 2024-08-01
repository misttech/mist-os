// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::{ComponentInstanceForAnalyzer, TopInstanceForAnalyzer};
use ::routing::bedrock::structured_dict::ComponentInput;
use ::routing::bedrock::with_policy_check::WithPolicyCheck;
use ::routing::capability_source::{CapabilitySource, ComponentCapability, InternalCapability};
use ::routing::component_instance::{ComponentInstanceInterface, WeakComponentInstanceInterface};
use ::routing::error::RoutingError;
use ::routing::policy::GlobalPolicyChecker;
use ::routing::DictExt;
use async_trait::async_trait;
use cm_config::RuntimeConfig;
use cm_rust::ComponentDecl;
use cm_types::RelativePath;
use fidl::endpoints::DiscoverableProtocolMarker;
use futures::{future, FutureExt};
use moniker::ChildName;
use router_error::RouterError;
use sandbox::{Capability, Dict, Request, Routable, Router};
use std::collections::HashMap;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
};

fn new_debug_only_router(source: CapabilitySource<ComponentInstanceForAnalyzer>) -> Router {
    let cap: Capability =
        source.try_into().expect("failed to convert capability source to dictionary");
    Router::new(move |request: Request| {
        if !request.debug {
            future::ready(Err(RouterError::NotFound(Arc::new(
                RoutingError::NonDebugRoutesUnsupported,
            ))))
            .boxed()
        } else {
            future::ready(Ok(cap.try_clone().unwrap())).boxed()
        }
    })
}

pub fn build_root_component_input(
    top_instance: &Arc<TopInstanceForAnalyzer>,
    runtime_config: &Arc<RuntimeConfig>,
    policy: &GlobalPolicyChecker,
) -> ComponentInput {
    let root_component_input = ComponentInput::default();
    let names_and_capability_sources = runtime_config
        .namespace_capabilities
        .iter()
        .filter_map(|capability_decl| match capability_decl {
            cm_rust::CapabilityDecl::Protocol(protocol_decl) => Some((
                protocol_decl.name.clone(),
                CapabilitySource::<ComponentInstanceForAnalyzer>::Namespace {
                    capability: ComponentCapability::Protocol(protocol_decl.clone()),
                    top_instance: Arc::downgrade(top_instance),
                },
            )),
            _ => None,
        })
        .chain(runtime_config.builtin_capabilities.iter().filter_map(|capability_decl| {
            match capability_decl {
                cm_rust::CapabilityDecl::Protocol(protocol_decl) => Some((
                    protocol_decl.name.clone(),
                    CapabilitySource::<ComponentInstanceForAnalyzer>::Builtin {
                        capability: InternalCapability::Protocol(protocol_decl.name.clone()),
                        top_instance: Arc::downgrade(top_instance),
                    },
                )),
                _ => None,
            }
        }));
    for (name, capability_source) in names_and_capability_sources {
        root_component_input
            .capabilities()
            .insert_capability(
                &name,
                Router::new_ok(Capability::Dictionary(
                    capability_source
                        .clone()
                        .try_into()
                        .expect("failed to convert builtin capability to dicttionary"),
                ))
                .with_policy_check(capability_source, policy.clone())
                .into(),
            )
            .expect("failed to insert builtin capability into dictionary");
    }
    root_component_input
}

pub fn build_framework_dictionary(component: &Arc<ComponentInstanceForAnalyzer>) -> Dict {
    let framework_dict = Dict::new();
    for protocol_name in &[
        fcomponent::BinderMarker::PROTOCOL_NAME,
        fsandbox::CapabilityStoreMarker::PROTOCOL_NAME,
        fcomponent::IntrospectorMarker::PROTOCOL_NAME,
        fsys::LifecycleControllerMarker::PROTOCOL_NAME,
        fcomponent::NamespaceMarker::PROTOCOL_NAME,
        fcomponent::RealmMarker::PROTOCOL_NAME,
        fsys::RealmQueryMarker::PROTOCOL_NAME,
        fsys::RouteValidatorMarker::PROTOCOL_NAME,
    ] {
        let name = cm_types::Name::new(*protocol_name).unwrap();
        framework_dict
            .insert_capability(
                &name,
                new_debug_only_router(CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(name.clone()),
                    moniker: component.moniker().clone(),
                })
                .into(),
            )
            .expect("failed to insert framework capability into dictionary");
    }
    framework_dict
}

pub fn build_capability_sourced_capabilities_dictionary(
    component: &Arc<ComponentInstanceForAnalyzer>,
    decl: &cm_rust::ComponentDecl,
) -> Dict {
    let output = Dict::new();
    for capability in &decl.capabilities {
        if let cm_rust::CapabilityDecl::Storage(storage_decl) = capability {
            output
                .insert_capability(
                    &storage_decl.name,
                    new_debug_only_router(CapabilitySource::Capability {
                        source_capability: ComponentCapability::Storage(storage_decl.clone()),
                        component: WeakComponentInstanceInterface::new(component),
                    })
                    .into(),
                )
                .expect("failed to insert capability backed capability into dictionary");
        }
    }
    output
}

pub fn new_program_router(
    component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
    _relative_path: RelativePath,
    capability: ComponentCapability,
) -> Router {
    let capability_source = CapabilitySource::<ComponentInstanceForAnalyzer>::Component {
        capability,
        moniker: component.moniker.clone(),
    };
    Router::new_ok(
        Capability::try_from(capability_source)
            .expect("failed to convert capability source to dictionary"),
    )
}

pub fn new_outgoing_dir_router(
    component: &Arc<ComponentInstanceForAnalyzer>,
    _decl: &cm_rust::ComponentDecl,
    capability: &cm_rust::CapabilityDecl,
) -> Router {
    new_debug_only_router(CapabilitySource::Component {
        capability: ComponentCapability::from(capability.clone()),
        moniker: component.moniker().clone(),
    })
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
