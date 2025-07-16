// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::{ComponentInstanceForAnalyzer, TopInstanceForAnalyzer};
use crate::component_model::DynamicDictionaryConfig;
use ::routing::bedrock::aggregate_router::AggregateSource;
use ::routing::bedrock::program_output_dict;
use ::routing::bedrock::structured_dict::ComponentInput;
use ::routing::bedrock::with_policy_check::WithPolicyCheck;
use ::routing::bedrock::with_porcelain::WithPorcelain;
use ::routing::capability_source::{
    BuiltinSource, CapabilitySource, CapabilityToCapabilitySource, ComponentCapability,
    ComponentSource, FrameworkSource, InternalCapability, NamespaceSource,
};
use ::routing::component_instance::{
    WeakComponentInstanceInterface, WeakExtendedInstanceInterface,
};
use ::routing::environment::RunnerRegistry;
use ::routing::error::{ErrorReporter, RouteRequestErrorInfo, RoutingError};
use ::routing::policy::GlobalPolicyChecker;
use ::routing::DictExt;
use async_trait::async_trait;
use cm_config::RuntimeConfig;
use cm_rust::{CapabilityTypeName, ComponentDecl, DeliveryType, DictionaryDecl, ProtocolDecl};
use cm_types::{Availability, Path};
use fidl::endpoints::DiscoverableProtocolMarker;
use futures::{future, FutureExt};
use moniker::{ChildName, Moniker};
use router_error::RouterError;
use sandbox::{
    CapabilityBound, Connector, Data, Dict, DirEntry, Request, Routable, Router, RouterResponse,
};
use std::collections::HashMap;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_sys2 as fsys,
};

fn new_debug_only_specific_router<T>(source: CapabilitySource) -> Router<T>
where
    T: CapabilityBound,
{
    let moniker = source.source_moniker();
    let data: Data = source.try_into().expect("failed to convert capability source to Data");
    Router::<T>::new(move |_request: Option<Request>, debug: bool| {
        if !debug {
            future::ready(Err(RouterError::NotFound(Arc::new(
                RoutingError::NonDebugRoutesUnsupported { moniker: moniker.clone() },
            ))))
            .boxed()
        } else {
            future::ready(Ok(RouterResponse::<T>::Debug(data.clone()))).boxed()
        }
    })
}

pub fn build_root_component_input(
    runtime_config: &Arc<RuntimeConfig>,
    top_instance: &Arc<TopInstanceForAnalyzer>,
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
                RouteRequestErrorInfo::from(capability_decl),
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
                        RouteRequestErrorInfo::from(capability_decl),
                    ))
                }
                _ => None,
            }
        }));
    for (name, capability_source, capability_type, route_request_info) in
        names_and_capability_sources
    {
        if capability_type == CapabilityTypeName::Runner
            && runner_registry.get_runner(&name).is_none()
        {
            // If a runner has been declared as a builtin capability but its not in the runner
            // registry, skip it.
            continue;
        }
        let data: sandbox::Data = capability_source
            .clone()
            .try_into()
            .expect("failed to convert capability source to Data");
        let router = match capability_type {
            CapabilityTypeName::Protocol | CapabilityTypeName::Runner => {
                let router = Router::<Connector>::new_debug(data)
                    .with_policy_check::<ComponentInstanceForAnalyzer>(
                        capability_source,
                        policy.clone(),
                    );
                WithPorcelain::<_, _, ComponentInstanceForAnalyzer>::with_porcelain_no_default(
                    router,
                    capability_type,
                )
                .availability(Availability::Required)
                .target_above_root(top_instance)
                .error_info(route_request_info)
                .error_reporter(NullErrorReporter {})
                .build()
            }
            _ => unreachable!("other types were filtered out above"),
        };
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

#[derive(Clone)]
struct NullErrorReporter {}
#[async_trait]
impl ErrorReporter for NullErrorReporter {
    async fn report(
        &self,
        _: &RouteRequestErrorInfo,
        _: &RouterError,
        _: sandbox::WeakInstanceToken,
    ) {
    }
}

pub(crate) fn build_framework_router(scope: &Arc<ComponentInstanceForAnalyzer>) -> Router<Dict> {
    Router::new(FrameworkRouter { scope: scope.moniker().clone() })
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
            .downcast_ref::<WeakExtendedInstanceInterface<ComponentInstanceForAnalyzer>>()
            .ok_or(RouterError::Unknown)?;
        let component = match target {
            WeakExtendedInstanceInterface::<ComponentInstanceForAnalyzer>::Component(c) => c,
            WeakExtendedInstanceInterface::<ComponentInstanceForAnalyzer>::AboveRoot(_) => {
                return Err(RouterError::InvalidArgs);
            }
        };
        let component = component.upgrade().map_err(RoutingError::from)?;
        if *component.moniker() != self.scope {
            return Err(RouterError::InvalidArgs);
        }

        let framework_dict = Dict::new();
        for protocol_name in &[
            fcomponent::BinderMarker::PROTOCOL_NAME,
            fsandbox::CapabilityStoreMarker::PROTOCOL_NAME,
            fcomponent::IntrospectorMarker::PROTOCOL_NAME,
            fcomponent::NamespaceMarker::PROTOCOL_NAME,
            fcomponent::RealmMarker::PROTOCOL_NAME,
            fsys::ConfigOverrideMarker::PROTOCOL_NAME,
            fsys::LifecycleControllerMarker::PROTOCOL_NAME,
            fsys::RealmQueryMarker::PROTOCOL_NAME,
            fsys::RouteValidatorMarker::PROTOCOL_NAME,
            "fuchsia.sys2.RealmExplorer",
        ] {
            let name = cm_types::Name::new(*protocol_name).unwrap();
            let router = new_debug_only_specific_router::<Connector>(CapabilitySource::Framework(
                FrameworkSource {
                    capability: InternalCapability::Protocol(name.clone()),
                    moniker: component.moniker().clone(),
                },
            ));
            framework_dict
                .insert_capability(&name, router.into())
                .expect("failed to insert framework capability into dictionary");
        }
        Ok(RouterResponse::Capability(framework_dict))
    }
}

pub fn build_capability_sourced_capabilities_dictionary(
    component: &Arc<ComponentInstanceForAnalyzer>,
    decl: &cm_rust::ComponentDecl,
) -> Dict {
    let output = Dict::new();
    for capability in &decl.capabilities {
        if let cm_rust::CapabilityDecl::Storage(storage_decl) = capability {
            let router = new_debug_only_specific_router::<Connector>(CapabilitySource::Capability(
                CapabilityToCapabilitySource {
                    source_capability: ComponentCapability::Storage(storage_decl.clone()),
                    moniker: component.moniker().clone(),
                },
            ));
            output
                .insert_capability(&storage_decl.name, router.into())
                .expect("failed to insert capability backed capability into dictionary");
        }
    }
    output
}

pub struct ProgramOutputGenerator {
    pub dynamic_dictionaries: Arc<DynamicDictionaryConfig>,
}

impl ProgramOutputGenerator {
    fn maybe_route_dynamic_dict(
        dynamic_dictionaries: &Arc<DynamicDictionaryConfig>,
        component: &WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
        capability: &ComponentCapability,
    ) -> Result<RouterResponse<Dict>, RouterError> {
        let ComponentCapability::Dictionary(DictionaryDecl { name: requested_name, .. }) =
            capability
        else {
            return Err(RouterError::NotFound(Arc::new(
                RoutingError::BedrockWrongCapabilityType {
                    actual: capability.type_name().to_string(),
                    expected: CapabilityTypeName::Dictionary.to_string(),
                    moniker: component.moniker.clone().into(),
                },
            )));
        };
        let Some(configs) = dynamic_dictionaries.get(&component.moniker) else {
            return Err(RouterError::NotFound(Arc::new(
                RoutingError::DynamicDictionariesNotAllowed {
                    moniker: component.moniker.clone().into(),
                },
            )));
        };
        let Some((_, capabilities)) = configs.into_iter().find(|(name, _)| *name == requested_name)
        else {
            return Err(RouterError::NotFound(Arc::new(
                RoutingError::DynamicDictionariesNotAllowed {
                    moniker: component.moniker.clone().into(),
                },
            )));
        };
        let dict = Dict::new();
        for (capability_type, capability_name) in capabilities {
            match capability_type {
                    CapabilityTypeName::Protocol => {
                        let router = new_debug_only_specific_router::<Connector>(
                            CapabilitySource::Component(ComponentSource {
                                capability: ComponentCapability::from(ProtocolDecl {
                                    name: capability_name.clone(),
                                    source_path: None,
                                    delivery: DeliveryType::Immediate,
                                }),
                                moniker: component.moniker.clone(),
                            }),
                        );
                        dict.insert_capability(&capability_name, router.into()).expect("can insert to dict");
                    }
                    _ => unreachable!("Only protocol capabilities are supported through scrutinity in dynamic dicts at the moment"),
                }
        }
        Ok(RouterResponse::<Dict>::Capability(dict))
    }
}

impl program_output_dict::ProgramOutputGenerator<ComponentInstanceForAnalyzer>
    for ProgramOutputGenerator
{
    fn new_program_dictionary_router(
        &self,
        component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
        _relative_path: Path,
        capability: ComponentCapability,
    ) -> Router<Dict> {
        let dynamic_dictionaries = self.dynamic_dictionaries.clone();
        Router::<Dict>::new(move |_request: Option<Request>, _debug: bool| {
            future::ready(Self::maybe_route_dynamic_dict(
                &dynamic_dictionaries,
                &component,
                &capability,
            ))
            .boxed()
        })
    }

    fn new_outgoing_dir_connector_router(
        &self,
        component: &Arc<ComponentInstanceForAnalyzer>,
        _decl: &cm_rust::ComponentDecl,
        capability: &cm_rust::CapabilityDecl,
    ) -> Router<Connector> {
        new_debug_only_specific_router::<Connector>(CapabilitySource::Component(ComponentSource {
            capability: ComponentCapability::from(capability.clone()),
            moniker: component.moniker().clone(),
        }))
    }

    fn new_outgoing_dir_dir_entry_router(
        &self,
        component: &Arc<ComponentInstanceForAnalyzer>,
        _decl: &cm_rust::ComponentDecl,
        capability: &cm_rust::CapabilityDecl,
    ) -> Router<DirEntry> {
        new_debug_only_specific_router::<DirEntry>(CapabilitySource::Component(ComponentSource {
            capability: ComponentCapability::from(capability.clone()),
            moniker: component.moniker().clone(),
        }))
    }
}

pub(crate) fn static_children_component_output_dictionary_routers(
    component: &Arc<ComponentInstanceForAnalyzer>,
    decl: &ComponentDecl,
) -> HashMap<ChildName, Router<Dict>> {
    struct ChildrenComponentOutputRouters {
        weak_component: WeakComponentInstanceInterface<ComponentInstanceForAnalyzer>,
        child_name: ChildName,
    }
    #[async_trait]
    impl Routable<Dict> for ChildrenComponentOutputRouters {
        async fn route(
            &self,
            _request: Option<Request>,
            _debug: bool,
        ) -> Result<RouterResponse<Dict>, RouterError> {
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
            let component_output_dict = child.sandbox.component_output.capabilities();
            Ok(RouterResponse::<Dict>::Capability(component_output_dict))
        }
    }

    let weak_component = WeakComponentInstanceInterface::new(component);
    let mut output = HashMap::new();
    for child_decl in decl.children.iter() {
        let child_name = ChildName::new(child_decl.name.clone(), None);
        output.insert(
            child_name.clone(),
            Router::<Dict>::new(ChildrenComponentOutputRouters {
                weak_component: weak_component.clone(),
                child_name,
            }),
        );
    }
    output
}

pub fn new_aggregate_router(
    _: Arc<ComponentInstanceForAnalyzer>,
    _: Vec<AggregateSource>,
    capability_source: CapabilitySource,
    _: CapabilityTypeName,
    _: Availability,
) -> Router<DirEntry> {
    new_debug_only_specific_router(capability_source)
}
