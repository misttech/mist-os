// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::instance::ResolvedInstanceState,
            component::{ComponentInstance, WeakComponentInstance},
            routing::router_ext::RouterExt,
        },
    },
    ::routing::{
        bedrock::structured_dict::ComponentInput,
        capability_source::ComponentCapability,
        component_instance::ComponentInstanceInterface,
        error::{ComponentInstanceError, RoutingError},
        DictExt, LazyGet,
    },
    async_trait::async_trait,
    cm_rust::CapabilityDecl,
    cm_types::{IterablePath, RelativePath},
    errors::{CapabilityProviderError, ComponentProviderError, OpenError, OpenOutgoingDirError},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    futures::FutureExt,
    itertools::Itertools,
    moniker::ChildName,
    router_error::RouterError,
    sandbox::Routable,
    sandbox::{Capability, Dict, Request, Router},
    std::{collections::HashMap, sync::Arc},
    tracing::warn,
    vfs::execution_scope::ExecutionScope,
};

pub fn build_program_output_dictionary(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
) {
    for capability in &decl.capabilities {
        extend_dict_with_capability(
            component,
            children,
            decl,
            capability,
            component_input,
            program_output_dict,
            &declared_dictionaries,
        );
    }
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
                        let open = component.get_outgoing();

                        let (inner_router, server_end) =
                            create_proxy::<fsandbox::RouterMarker>().unwrap();
                        open.open(
                            ExecutionScope::new(),
                            fio::OpenFlags::empty(),
                            vfs::path::Path::validate_and_split(self.source_path.to_string())
                                .expect("path must be valid"),
                            server_end.into_channel(),
                        );
                        let cap = inner_router
                            .route(request.into())
                            .await
                            .map_err(|e| open_error(OpenOutgoingDirError::Fidl(e)))?
                            .map_err(RouterError::from)?;
                        let cap = Capability::try_from(cap)
                            .map_err(|_| RoutingError::BedrockRemoteCapability)?;
                        if !matches!(cap, Capability::Dictionary(_)) {
                            Err(RoutingError::BedrockWrongCapabilityType {
                                actual: cap.debug_typename().into(),
                                expected: "Dictionary".into(),
                            })?;
                        }
                        Ok(cap)
                    }
                }
                Router::new(ProgramRouter {
                    component: component.as_weak(),
                    source_path: source_path.clone(),
                })
            }
        };
        router = make_dict_extending_router(dict.clone(), source_dict_router);
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
fn make_dict_extending_router(dict: Dict, source_dict_router: Router) -> Router {
    let route_fn = move |request: Request| {
        let source_dict_router = source_dict_router.clone();
        let dict = dict.clone();
        async move {
            let source_dict;
            match source_dict_router.route(request).await? {
                Capability::Dictionary(d) => {
                    source_dict = d;
                }
                // Optional from void.
                cap @ Capability::Unit(_) => return Ok(cap),
                cap => {
                    return Err(RoutingError::BedrockWrongCapabilityType {
                        actual: cap.debug_typename().into(),
                        expected: "Dictionary".into(),
                    }
                    .into())
                }
            }
            let out_dict = dict.shallow_copy();
            for (source_key, source_value) in source_dict.enumerate() {
                if let Err(_) = out_dict.insert(source_key.clone(), source_value.clone()) {
                    return Err(RoutingError::BedrockSourceDictionaryCollision.into());
                }
            }
            Ok(out_dict.into())
        }
        .boxed()
    };
    Router::new(route_fn)
}
