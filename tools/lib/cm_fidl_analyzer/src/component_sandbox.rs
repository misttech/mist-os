// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::ComponentInstanceForAnalyzer;
use ::routing::bedrock::structured_dict::ComponentInput;
use ::routing::bedrock::with_policy_check::WithPolicyCheck;
use ::routing::bedrock::with_porcelain_type::WithPorcelainType;
use ::routing::capability_source::{
    BuiltinSource, CapabilitySource, CapabilityToCapabilitySource, ComponentCapability,
    ComponentSource, FrameworkSource, InternalCapability, NamespaceSource,
};
use ::routing::component_instance::WeakComponentInstanceInterface;
use ::routing::environment::RunnerRegistry;
use ::routing::error::RoutingError;
use ::routing::policy::GlobalPolicyChecker;
use ::routing::DictExt;
use async_trait::async_trait;
use cm_config::RuntimeConfig;
use cm_rust::{CapabilityTypeName, ComponentDecl};
use cm_types::Path;
use fidl::endpoints::DiscoverableProtocolMarker;
use futures::{future, FutureExt};
use moniker::{ChildName, ExtendedMoniker};
use router_error::RouterError;
use sandbox::{Capability, Dict, Request, Routable, Router};
use std::collections::HashMap;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
};

fn new_debug_only_router(source: CapabilitySource) -> Router {
    let moniker = source.source_moniker();
    let cap: Capability =
        source.try_into().expect("failed to convert capability source to dictionary");
    Router::new(move |_request: Option<Request>, debug: bool| {
        if !debug {
            future::ready(Err(RouterError::NotFound(Arc::new(
                RoutingError::NonDebugRoutesUnsupported { moniker: moniker.clone() },
            ))))
            .boxed()
        } else {
            future::ready(Ok(cap.try_clone().unwrap())).boxed()
        }
    })
}

pub fn build_root_component_input(
    runtime_config: &Arc<RuntimeConfig>,
    policy: &GlobalPolicyChecker,
    runner_registry: RunnerRegistry,
) -> ComponentInput {
    let root_component_input = ComponentInput::default();
    let names_and_capability_sources = runtime_config
        .namespace_capabilities
        .iter()
        .filter_map(|capability_decl| match capability_decl {
            cm_rust::CapabilityDecl::Protocol(_) | cm_rust::CapabilityDecl::Runner(_) => Some((
                capability_decl.name().clone(),
                CapabilitySource::Namespace(NamespaceSource {
                    capability: capability_decl.clone().into(),
                }),
                CapabilityTypeName::from(capability_decl),
            )),
            _ => None,
        })
        .chain(runtime_config.builtin_capabilities.iter().filter_map(|capability_decl| {
            match capability_decl {
                cm_rust::CapabilityDecl::Protocol(_) | cm_rust::CapabilityDecl::Runner(_) => {
                    Some((
                        capability_decl.name().clone(),
                        CapabilitySource::Builtin(BuiltinSource {
                            capability: capability_decl.clone().into(),
                        }),
                        CapabilityTypeName::from(capability_decl),
                    ))
                }
                _ => None,
            }
        }));
    for (name, capability_source, capability_type) in names_and_capability_sources {
        if capability_type == CapabilityTypeName::Runner
            && runner_registry.get_runner(&name).is_none()
        {
            // If a runner has been declared as a builtin capability but its not in the runner
            // registry, skip it.
            continue;
        }
        let capability: sandbox::Capability = capability_source
            .clone()
            .try_into()
            .expect("failed to convert capability source to bedrock capability");
        let router = Router::new_ok(capability)
            .with_policy_check::<ComponentInstanceForAnalyzer>(capability_source, policy.clone())
            .with_porcelain_type(capability_type, ExtendedMoniker::ComponentManager);
        root_component_input
            .capabilities()
            .insert_capability(&name, router.clone().into())
            .expect("failed to insert builtin capability into dictionary");
        if capability_type == CapabilityTypeName::Runner {
            root_component_input
                .environment()
                .runners()
                .insert_capability(&name, router.into())
                .expect("failed to insert builtin runner into dictionary");
        }
    }
    root_component_input
}

pub fn build_framework_dictionary(component: &Arc<ComponentInstanceForAnalyzer>) -> Dict {
    let framework_dict = Dict::new();
    for protocol_name in &[
        fcomponent::BinderMarker::PROTOCOL_NAME,
        fsandbox::CapabilityStoreMarker::PROTOCOL_NAME,
        fcomponent::IntrospectorMarker::PROTOCOL_NAME,
        fcomponent::NamespaceMarker::PROTOCOL_NAME,
        fcomponent::RealmMarker::PROTOCOL_NAME,
        fsys::LifecycleControllerMarker::PROTOCOL_NAME,
        fsys::RealmQueryMarker::PROTOCOL_NAME,
        fsys::RouteValidatorMarker::PROTOCOL_NAME,
        "fuchsia.sys2.RealmExplorer",
    ] {
        let name = cm_types::Name::new(*protocol_name).unwrap();
        framework_dict
            .insert_capability(
                &name,
                new_debug_only_router(CapabilitySource::Framework(FrameworkSource {
                    capability: InternalCapability::Protocol(name.clone()),
                    moniker: component.moniker().clone(),
                }))
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
                    new_debug_only_router(CapabilitySource::Capability(
                        CapabilityToCapabilitySource {
                            source_capability: ComponentCapability::Storage(storage_decl.clone()),
                            moniker: component.moniker().clone(),
                        },
                    ))
                    .into(),
                )
                .expect("failed to insert capability backed capability into dictionary");
        }
    }
    output
}

pub fn new_program_router(
    component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
    _relative_path: Path,
    capability: ComponentCapability,
) -> Router {
    let capability_source = CapabilitySource::Component(ComponentSource {
        capability,
        moniker: component.moniker.clone(),
    });
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
    new_debug_only_router(CapabilitySource::Component(ComponentSource {
        capability: ComponentCapability::from(capability.clone()),
        moniker: component.moniker().clone(),
    }))
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
        async fn route(
            &self,
            _request: Option<Request>,
            _debug: bool,
        ) -> Result<Capability, RouterError> {
            let component =
                self.weak_component.upgrade().expect("part of component tree was dropped");
            let child = component
                .children
                .read()
                .expect("failed to get lock")
                .get(&self.child_name)
                .cloned()
                .ok_or(RouterError::NotFound(Arc::new(
                    RoutingError::offer_from_child_instance_not_found(
                        &self.child_name,
                        &self.weak_component.moniker,
                        "component output dictionary",
                    ),
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
