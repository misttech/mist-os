// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilitySource;
use crate::model::component::instance::ResolvedInstanceState;
use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::routing::router_ext::{RouterExt, WeakInstanceTokenExt};
use ::routing::bedrock::structured_dict::ComponentInput;
use ::routing::capability_source::ComponentCapability;
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::error::{ComponentInstanceError, RoutingError};
use ::routing::{DictExt, LazyGet};
use async_trait::async_trait;
use cm_rust::CapabilityDecl;
use cm_types::{IterablePath, RelativePath};
use errors::{CapabilityProviderError, ComponentProviderError, OpenError, OpenOutgoingDirError};
use fidl::endpoints::create_proxy;
use futures::FutureExt;
use itertools::Itertools;
use moniker::ChildName;
use router_error::RouterError;
use sandbox::{Capability, Dict, Request, Routable, Router, WeakInstanceToken};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

pub fn build_program_output_dictionary(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    component_input: &ComponentInput,
) -> (Dict, Dict) {
    let program_output_dict = Dict::new();
    let declared_dictionaries = Dict::new();
    for capability in &decl.capabilities {
        extend_dict_with_capability(
            component,
            children,
            decl,
            capability,
            component_input,
            &program_output_dict,
            &declared_dictionaries,
        );
    }
    (program_output_dict, declared_dictionaries)
}

/// Adds `capability` to the program output dict given the resolved `decl`. The program output dict
/// is a dict of routers, keyed by capability name.
fn extend_dict_with_capability(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    capability: &cm_rust::CapabilityDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
) {
    match capability {
        CapabilityDecl::Service(_)
        | CapabilityDecl::Protocol(_)
        | CapabilityDecl::Directory(_)
        | CapabilityDecl::Runner(_)
        | CapabilityDecl::Resolver(_) => {
            let path = capability.path().expect("must have path");
            let router = ResolvedInstanceState::make_program_outgoing_router(
                component, decl, capability, path,
            );
            let router = router.with_policy_check(
                CapabilitySource::Component {
                    capability: ComponentCapability::from(capability.clone()),
                    component: component.as_weak(),
                },
                component.policy_checker().clone(),
            );
            match program_output_dict.insert_capability(capability.name(), router.into()) {
                Ok(()) => (),
                Err(e) => {
                    warn!("failed to add {} to program output dict: {e:?}", capability.name())
                }
            }
        }
        cm_rust::CapabilityDecl::Dictionary(d) => {
            extend_dict_with_dictionary(
                component,
                children,
                d,
                component_input,
                program_output_dict,
                declared_dictionaries,
            );
        }
        CapabilityDecl::EventStream(_) | CapabilityDecl::Config(_) | CapabilityDecl::Storage(_) => {
            // Capabilities not supported in bedrock program output dict yet.
            return;
        }
    }
}

fn extend_dict_with_dictionary(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::DictionaryDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
) {
    let dict = Dict::new();
    let router;
    if let Some(source) = decl.source.as_ref() {
        let source_path = decl
            .source_dictionary
            .as_ref()
            .expect("source_dictionary must be set if source is set");
        let source_dict_router = match &source {
            cm_rust::DictionarySource::Parent => component_input.capabilities().lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_parent_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                ),
            ),
            cm_rust::DictionarySource::Self_ => component.program_output().lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_self_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                ),
            ),
            cm_rust::DictionarySource::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name =
                    ChildName::parse(child_ref.name.as_str()).expect("invalid child name");
                match children.get(&child_name) {
                    Some(child) => child.component_output().lazy_get(
                        source_path.to_owned(),
                        RoutingError::BedrockSourceDictionaryExposeNotFound,
                    ),
                    None => Router::new_error(RoutingError::use_from_child_instance_not_found(
                        &child_name,
                        &component.moniker,
                        source_path.iter_segments().join("/"),
                    )),
                }
            }
            cm_rust::DictionarySource::Program => {
                struct ProgramRouter {
                    component: WeakComponentInstance,
                    source_path: RelativePath,
                }
                #[async_trait]
                impl Routable for ProgramRouter {
                    async fn route(&self, request: Request) -> Result<Capability, RouterError> {
                        fn open_error(e: OpenOutgoingDirError) -> OpenError {
                            CapabilityProviderError::from(ComponentProviderError::from(e)).into()
                        }

                        let component = self.component.upgrade().map_err(|_| {
                            RoutingError::from(ComponentInstanceError::instance_not_found(
                                self.component.moniker.clone(),
                            ))
                        })?;
                        let dir_entry = component.get_outgoing();

                        let (inner_router, server_end) =
                            create_proxy::<fsandbox::RouterMarker>().unwrap();
                        dir_entry.open(
                            ExecutionScope::new(),
                            fio::OpenFlags::empty(),
                            vfs::path::Path::validate_and_split(self.source_path.to_string())
                                .expect("path must be valid"),
                            server_end.into_channel(),
                        );
                        let debug = request.debug;
                        let cap = inner_router
                            .route(request.into())
                            .await
                            .map_err(|e| open_error(OpenOutgoingDirError::Fidl(e)))?
                            .map_err(RouterError::from)?;
                        let capability = Capability::try_from(cap)
                            .map_err(|_| RoutingError::BedrockRemoteCapability)?;
                        if !matches!(capability, Capability::Dictionary(_)) {
                            Err(RoutingError::BedrockWrongCapabilityType {
                                actual: capability.debug_typename().into(),
                                expected: "Dictionary".into(),
                            })?;
                        }
                        if !debug {
                            Ok(capability)
                        } else {
                            let source = WeakInstanceToken::new_component(component.as_weak());
                            Ok(Capability::Instance(source))
                        }
                    }
                }
                Router::new(ProgramRouter {
                    component: component.as_weak(),
                    source_path: source_path.clone(),
                })
            }
        };
        router = make_dict_extending_router(
            dict.clone(),
            WeakInstanceToken::new_component(component.as_weak()),
            source_dict_router,
        );
    } else {
        router = Router::new_ok(dict.clone());
    }
    match declared_dictionaries.insert_capability(&decl.name, dict.into()) {
        Ok(()) => (),
        Err(e) => warn!("failed to add {} to declared dicts: {e:?}", decl.name),
    };
    match program_output_dict.insert_capability(&decl.name, router.into()) {
        Ok(()) => (),
        Err(e) => warn!("failed to add {} to program output dict: {e:?}", decl.name),
    }
}

/// Returns a [Router] that returns a [Dict] whose contents are these union of `dict` and the
/// [Dict] returned by `source_dict_router`.
///
/// This algorithm returns a new [Dict] each time, leaving `dict` unmodified.
fn make_dict_extending_router(
    dict: Dict,
    dict_source: WeakInstanceToken,
    source_dict_router: Router,
) -> Router {
    let route_fn = move |request: Request| {
        let source_dict_router = source_dict_router.clone();
        let dict = dict.clone();
        let dict_source = dict_source.clone();
        async move {
            let debug = request.debug;
            let source_dict = match source_dict_router.route(request).await? {
                Capability::Dictionary(d) => Some(d),
                // Optional from void.
                cap @ Capability::Unit(_) => return Ok(cap),
                // Debug source token.
                Capability::Instance(_) if debug => None,
                cap => {
                    return Err(RoutingError::BedrockWrongCapabilityType {
                        actual: cap.debug_typename().into(),
                        expected: "Dictionary".into(),
                    }
                    .into())
                }
            };
            if debug {
                return Ok(Capability::Instance(dict_source.clone()));
            }
            let source_dict = source_dict.unwrap();
            let out_dict = dict.shallow_copy().map_err(|_| RoutingError::BedrockNotCloneable)?;
            for (source_key, source_value) in source_dict.enumerate() {
                let Ok(source_value) = source_value else {
                    return Err(RoutingError::BedrockNotCloneable.into());
                };
                if let Err(_) = out_dict.insert(source_key.clone(), source_value) {
                    return Err(RoutingError::BedrockSourceDictionaryCollision.into());
                }
            }
            Ok(out_dict.into())
        }
        .boxed()
    };
    Router::new(route_fn)
}
