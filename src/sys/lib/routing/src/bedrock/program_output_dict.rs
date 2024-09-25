// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::structured_dict::ComponentInput;
use crate::bedrock::with_policy_check::WithPolicyCheck;
use crate::capability_source::{CapabilitySource, ComponentCapability, ComponentSource};
use crate::component_instance::{ComponentInstanceInterface, WeakComponentInstanceInterface};
use crate::error::RoutingError;
use crate::{DictExt, LazyGet};
use async_trait::async_trait;
use cm_rust::NativeIntoFidl;
use cm_types::{IterablePath, Path, RelativePath};
use futures::{future, FutureExt};
use itertools::Itertools;
use moniker::ChildName;
use router_error::RouterError;
use sandbox::{Capability, Dict, Request, Routable, Router};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

pub type ProgramRouterFn<C> =
    dyn Fn(WeakComponentInstanceInterface<C>, Path, ComponentCapability) -> Router;
pub type OutgoingDirRouterFn<C> =
    dyn Fn(&Arc<C>, &cm_rust::ComponentDecl, &cm_rust::CapabilityDecl) -> Router;

pub fn build_program_output_dictionary<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    decl: &cm_rust::ComponentDecl,
    component_input: &ComponentInput,
    // This router should forward routing requests to a component's program
    new_program_router: &ProgramRouterFn<C>,
    new_outgoing_dir_router: &OutgoingDirRouterFn<C>,
) -> (Dict, Dict) {
    let program_output_dict = Dict::new();
    let declared_dictionaries = Dict::new();
    for capability in &decl.capabilities {
        extend_dict_with_capability(
            component,
            child_component_output_dictionary_routers,
            decl,
            capability,
            component_input,
            &program_output_dict,
            &declared_dictionaries,
            new_program_router,
            new_outgoing_dir_router,
        );
    }
    (program_output_dict, declared_dictionaries)
}

/// Adds `capability` to the program output dict given the resolved `decl`. The program output dict
/// is a dict of routers, keyed by capability name.
fn extend_dict_with_capability<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    decl: &cm_rust::ComponentDecl,
    capability: &cm_rust::CapabilityDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
    new_program_router: &ProgramRouterFn<C>,
    new_outgoing_dir_router: &OutgoingDirRouterFn<C>,
) {
    match capability {
        cm_rust::CapabilityDecl::Service(_)
        | cm_rust::CapabilityDecl::Protocol(_)
        | cm_rust::CapabilityDecl::Directory(_)
        | cm_rust::CapabilityDecl::Runner(_)
        | cm_rust::CapabilityDecl::Resolver(_) => {
            let router = new_outgoing_dir_router(component, decl, capability);
            let router = router.with_policy_check::<C>(
                CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::from(capability.clone()),
                    moniker: component.moniker().clone(),
                }),
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
                child_component_output_dictionary_routers,
                d,
                component_input,
                program_output_dict,
                declared_dictionaries,
                new_program_router,
            );
        }
        cm_rust::CapabilityDecl::Config(c) => {
            let data: sandbox::Capability =
                sandbox::Data::Bytes(fidl::persist(&c.value.clone().native_into_fidl()).unwrap())
                    .into();
            match program_output_dict.insert_capability(capability.name(), Router::new(data).into())
            {
                Ok(()) => (),
                Err(e) => {
                    warn!("failed to add {} to program output dict: {e:?}", capability.name())
                }
            }
        }
        cm_rust::CapabilityDecl::EventStream(_) | cm_rust::CapabilityDecl::Storage(_) => {
            // Capabilities not supported in bedrock program output dict yet.
            return;
        }
    }
}

fn extend_dict_with_dictionary<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    decl: &cm_rust::DictionaryDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
    new_program_router: &ProgramRouterFn<C>,
) {
    let router;
    let declared_dict;
    if let Some(source) = decl.source.as_ref() {
        // Dictionary derived from another dictionary (`extends` in cml).
        let source_path = decl
            .source_dictionary
            .as_ref()
            .expect("source_dictionary must be set if source is set");
        let dict = Dict::new();
        router = make_dict_extending_router(
            dict.clone(),
            source,
            source_path,
            component,
            child_component_output_dictionary_routers,
            decl,
            component_input,
        );
        declared_dict = Some(dict);
    } else if let Some(source_path) = decl.source_path.as_ref() {
        // Dictionary backed by program's outgoing directory.
        router = new_program_router(
            component.as_weak(),
            source_path.clone(),
            ComponentCapability::Dictionary(decl.clone()),
        );
        declared_dict = None;
    } else {
        let dict = Dict::new();
        router = make_simple_dict_router(dict.clone(), component, decl);
        declared_dict = Some(dict);
    }
    if let Some(dict) = declared_dict {
        match declared_dictionaries.insert_capability(&decl.name, dict.into()) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {} to declared dicts: {e:?}", decl.name),
        };
    }
    match program_output_dict.insert_capability(&decl.name, router.into()) {
        Ok(()) => (),
        Err(e) => warn!("failed to add {} to program output dict: {e:?}", decl.name),
    }
}

/// Makes a [Router] that always returns the given dictionary.
fn make_simple_dict_router<C: ComponentInstanceInterface + 'static>(
    dict: Dict,
    component: &Arc<C>,
    decl: &cm_rust::DictionaryDecl,
) -> Router {
    struct DictRouter {
        dict: Dict,
        source: CapabilitySource,
    }
    #[async_trait]
    impl Routable for DictRouter {
        async fn route(
            &self,
            _request: Option<Request>,
            debug: bool,
        ) -> Result<Capability, RouterError> {
            if debug {
                Ok(self
                    .source
                    .clone()
                    .try_into()
                    .expect("failed to convert capability source to dictionary"))
            } else {
                Ok(self.dict.clone().into())
            }
        }
    }
    let source = CapabilitySource::Component(ComponentSource {
        capability: ComponentCapability::Dictionary(decl.clone()),
        moniker: component.moniker().clone(),
    });
    Router::new(DictRouter { dict, source })
}

/// Returns a [Router] that returns a [Dict] whose contents are these union of `dict` and the
/// [Dict] returned by `source_dict_router`.
///
/// This algorithm returns a new [Dict] each time, leaving `dict` unmodified.
fn make_dict_extending_router<C: ComponentInstanceInterface + 'static>(
    dict: Dict,
    source: &cm_rust::DictionarySource,
    source_path: &RelativePath,
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    decl: &cm_rust::DictionaryDecl,
    component_input: &ComponentInput,
) -> Router {
    let source_dict_router = match source {
        cm_rust::DictionarySource::Parent => component_input.capabilities().lazy_get(
            source_path.to_owned(),
            RoutingError::use_from_parent_not_found(
                component.moniker(),
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::DictionarySource::Self_ => {
            weak_reference_program_output_router(component.as_weak()).lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_self_not_found(
                    component.moniker(),
                    source_path.iter_segments().join("/"),
                ),
            )
        }
        cm_rust::DictionarySource::Child(child_ref) => {
            assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
            let child_name = ChildName::parse(child_ref.name.as_str()).expect("invalid child name");
            match child_component_output_dictionary_routers.get(&child_name) {
                Some(output_dictionary_router) => output_dictionary_router.clone().lazy_get(
                    source_path.to_owned(),
                    RoutingError::BedrockSourceDictionaryExposeNotFound {
                        moniker: component.moniker().clone(),
                    },
                ),
                None => Router::new_error(RoutingError::use_from_child_instance_not_found(
                    &child_name,
                    component.moniker(),
                    source_path.iter_segments().join("/"),
                )),
            }
        }
    };
    make_dict_extending_router_inner(
        dict,
        source_dict_router,
        CapabilitySource::Component(ComponentSource {
            capability: ComponentCapability::Dictionary(decl.clone()),
            moniker: component.moniker().clone(),
        }),
    )
}

fn make_dict_extending_router_inner(
    dict: Dict,
    source_dict_router: Router,
    source: CapabilitySource,
) -> Router {
    let route_fn = move |request: Option<Request>, debug: bool| {
        if debug {
            return future::ok(
                source
                    .clone()
                    .try_into()
                    .expect("failed to convert capability source to dictionary"),
            )
            .boxed();
        }
        let source_dict_router = source_dict_router.clone();
        let dict = dict.clone();
        let source_moniker = source.source_moniker();
        async move {
            let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
            let source_dict = match source_dict_router.route(Some(request), debug).await? {
                Capability::Dictionary(d) => Some(d),
                // Optional from void.
                cap @ Capability::Unit(_) => return Ok(cap),
                cap => {
                    return Err(RoutingError::BedrockWrongCapabilityType {
                        moniker: source_moniker,
                        actual: cap.debug_typename().into(),
                        expected: "Dictionary".into(),
                    }
                    .into())
                }
            };
            let source_dict = source_dict.unwrap();
            let out_dict = dict.shallow_copy().map_err(|_| RoutingError::BedrockNotCloneable {
                moniker: source_moniker.clone(),
            })?;
            for (source_key, source_value) in source_dict.enumerate() {
                let Ok(source_value) = source_value else {
                    return Err(
                        RoutingError::BedrockNotCloneable { moniker: source_moniker }.into()
                    );
                };
                if let Err(_) = out_dict.insert(source_key.clone(), source_value) {
                    return Err(RoutingError::BedrockSourceDictionaryCollision {
                        moniker: source_moniker,
                    }
                    .into());
                }
            }
            Ok(out_dict.into())
        }
        .boxed()
    };
    Router::new(route_fn)
}

fn weak_reference_program_output_router<C: ComponentInstanceInterface + 'static>(
    weak_component: WeakComponentInstanceInterface<C>,
) -> Router {
    Router::new(move |request: Option<Request>, debug: bool| {
        let weak_component = weak_component.clone();
        async move {
            let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
            let component = weak_component.upgrade().map_err(RoutingError::from)?;
            let sandbox = component.component_sandbox().await.map_err(RoutingError::from)?;
            sandbox.program_output_dict.route(Some(request), debug).await
        }
        .boxed()
    })
}
