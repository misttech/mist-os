// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, ResolvedInstanceState, WeakComponentInstance},
            routing::router::{Request, Router},
            structured_dict::{ComponentEnvironment, ComponentInput, StructuredDictMap},
        },
        sandbox_util::{DictExt, LaunchTaskOnReceive, RoutableExt},
    },
    ::routing::{
        capability_source::{ComponentCapability, InternalCapability},
        component_instance::ComponentInstanceInterface,
        error::{ComponentInstanceError, RoutingError},
    },
    cm_rust::{
        CapabilityDecl, ExposeDeclCommon, OfferDeclCommon, SourceName, SourcePath, UseDeclCommon,
    },
    cm_types::{IterablePath, Name, SeparatedPath},
    fidl_fuchsia_component_decl as fdecl,
    futures::FutureExt,
    itertools::Itertools,
    moniker::{ChildName, ChildNameBase, MonikerBase},
    sandbox::{Capability, Dict, Unit},
    std::{collections::HashMap, fmt::Debug, sync::Arc},
    tracing::warn,
};

/// Once a component has been resolved and its manifest becomes known, this function produces the
/// various dicts the component needs based on the contents of its manifest.
pub fn build_component_sandbox(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    component_input: &ComponentInput,
    component_output_dict: &Dict,
    program_input_dict: &Dict,
    program_output_dict: &Dict,
    child_inputs: &mut StructuredDictMap<ComponentInput>,
    collection_inputs: &mut StructuredDictMap<ComponentInput>,
    environments: &mut StructuredDictMap<ComponentEnvironment>,
) {
    let declared_dictionaries = Dict::new();

    for environment_decl in &decl.environments {
        environments.insert(
            Name::new(&environment_decl.name).unwrap(),
            build_environment(component, children, component_input, environment_decl),
        );
    }

    for child in &decl.children {
        let environment;
        if let Some(environment_name) = child.environment.as_ref() {
            environment = environments.get(&Name::new(environment_name).unwrap()).expect(
                "child references nonexistent environment, \
                    this should be prevented in manifest validation",
            )
        } else {
            environment = component_input.environment();
        }
        let input = ComponentInput::new(environment);
        child_inputs.insert(Name::new(&child.name).unwrap(), input);
    }

    for collection in &decl.collections {
        let environment;
        if let Some(environment_name) = collection.environment.as_ref() {
            environment = environments.get(&Name::new(environment_name).unwrap()).expect(
                "collection references nonexistent environment, \
                    this should be prevented in manifest validation",
            )
        } else {
            environment = component_input.environment();
        }
        let input = ComponentInput::new(environment);
        collection_inputs.insert(collection.name.clone(), input);
    }

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

    for use_ in &decl.uses {
        extend_dict_with_use(component, children, component_input, program_input_dict, use_);
    }

    for offer in &decl.offers {
        // We only support protocol and dictionary capabilities right now
        if !is_supported_offer(offer) {
            continue;
        }
        let mut target_dict = match offer.target() {
            cm_rust::OfferTarget::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name = Name::new(&child_ref.name).unwrap();
                if child_inputs.get(&child_name).is_none() {
                    child_inputs.insert(child_name.clone(), Default::default());
                }
                child_inputs
                    .get(&child_name)
                    .expect("component input was just added")
                    .capabilities()
            }
            cm_rust::OfferTarget::Collection(name) => {
                if collection_inputs.get(name).is_none() {
                    collection_inputs.insert(name.clone(), Default::default());
                }
                collection_inputs.get(name).expect("collection input was just added").capabilities()
            }
            cm_rust::OfferTarget::Capability(name) => {
                let mut entries = declared_dictionaries.lock_entries();
                let dict = entries
                    .entry(name.clone())
                    .or_insert_with(|| Capability::Dictionary(Dict::new()))
                    .clone();
                let Capability::Dictionary(dict) = dict else {
                    panic!("wrong type in dict");
                };
                dict
            }
        };
        extend_dict_with_offer(component, children, component_input, offer, &mut target_dict);
    }

    for expose in &decl.exposes {
        extend_dict_with_expose(component, children, expose, component_output_dict);
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
            program_output_dict.insert_capability(capability.name(), router.into());
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
    let router = if let Some(source) = decl.source.as_ref() {
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
        };
        make_dict_extending_router(component.as_weak(), dict.clone(), source_dict_router)
    } else {
        Router::new_ok(dict.clone())
    };
    declared_dictionaries.insert_capability(&decl.name, dict.into());
    program_output_dict.insert_capability(&decl.name, router.into());
}

fn build_environment(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    environment_decl: &cm_rust::EnvironmentDecl,
) -> ComponentEnvironment {
    let mut environment = ComponentEnvironment::new();
    if environment_decl.extends == fdecl::EnvironmentExtends::Realm {
        environment = component_input.environment().shallow_copy();
    }
    for debug_registration in &environment_decl.debug_capabilities {
        let cm_rust::DebugRegistration::Protocol(debug_protocol) = debug_registration;
        let source_path = SeparatedPath {
            dirname: Default::default(),
            basename: debug_protocol.source_name.clone(),
        };
        let router = match &debug_protocol.source {
            cm_rust::RegistrationSource::Parent => {
                use_from_parent_router(component_input, source_path, component.as_weak())
            }
            cm_rust::RegistrationSource::Self_ => component.program_output().lazy_get(
                source_path.clone(),
                RoutingError::use_from_self_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                ),
            ),
            cm_rust::RegistrationSource::Child(child_name) => {
                let child_name = ChildName::parse(child_name).expect("invalid child name");
                let Some(child) = children.get(&child_name) else { continue };
                child.component_output().lazy_get(
                    source_path,
                    RoutingError::use_from_child_expose_not_found(
                        child.moniker.leaf().unwrap(),
                        &child.moniker.parent().unwrap(),
                        debug_protocol.source_name.clone(),
                    ),
                )
            }
        };
        environment.debug().insert_capability(&debug_protocol.target_name, router.into());
    }
    environment
}

/// Returns a [Router] that returns a [Dict].
/// The first time this router is called, it calls `source_dict_router` to get another [Dict],
/// and combines those entries with `dict``.
/// Each time after, this router returns `dict`` that has the combined entries.
/// NOTE: This function modifies `dict`!
fn make_dict_extending_router(
    component: WeakComponentInstance,
    dict: Dict,
    source_dict_router: Router,
) -> Router {
    let did_combine = Arc::new(std::sync::Mutex::new(false));
    let route_fn = move |_request: Request| {
        let source_dict_router = source_dict_router.clone();
        let dict = dict.clone();
        let did_combine = did_combine.clone();
        let component = component.clone();
        async move {
            // If we've already combined then return our dict.
            if *did_combine.lock().unwrap() {
                return Ok(dict.into());
            }

            // Otherwise combine the two.
            let source_request =
                Request { availability: cm_types::Availability::Required, target: component };
            let source_dict = match source_dict_router.route(source_request).await? {
                Capability::Dictionary(d) => d,
                _ => panic!("source_dict_router must return a Dict"),
            };
            {
                let mut entries = dict.lock_entries();
                let source_entries = source_dict.lock_entries();
                for source_key in source_entries.keys() {
                    if entries.contains_key(source_key.as_str()) {
                        return Err(RoutingError::BedrockSourceDictionaryCollision.into());
                    }
                }
                for (source_key, source_value) in &*source_entries {
                    entries.insert(source_key.clone(), source_value.clone());
                }
            }
            *did_combine.lock().unwrap() = true;
            Ok(dict.into())
        }
        .boxed()
    };
    Router::new(route_fn)
}

/// Extends the given dict based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_dict`.
pub fn extend_dict_with_offers(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_input: &mut ComponentInput,
) {
    for offer in dynamic_offers {
        extend_dict_with_offer(
            component,
            children,
            component_input,
            offer,
            &mut target_input.capabilities(),
        );
    }
}

fn supported_use(use_: &cm_rust::UseDecl) -> Option<&cm_rust::UseProtocolDecl> {
    match use_ {
        cm_rust::UseDecl::Protocol(p) => Some(p),
        _ => None,
    }
}

fn extend_dict_with_use(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    program_input_dict: &Dict,
    use_: &cm_rust::UseDecl,
) {
    let Some(use_protocol) = supported_use(use_) else {
        return;
    };

    let source_path = use_.source_path();
    let router = match use_.source() {
        cm_rust::UseSource::Parent => {
            use_from_parent_router(component_input, source_path.to_owned(), component.as_weak())
        }
        cm_rust::UseSource::Self_ => component.program_output().lazy_get(
            source_path.to_owned(),
            RoutingError::use_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::UseSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid child name");
            let Some(child) = children.get(&child_name) else {
                panic!("use declaration in manifest for component {} has a source of a nonexistent child {}, this should be prevented by manifest validation", component.moniker, child_name);
            };
            child.component_output().lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    use_.source_name().clone(),
                ),
            )
        }
        cm_rust::UseSource::Framework if use_.is_from_dictionary() => {
            Router::new_error(RoutingError::capability_from_framework_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ))
        }
        cm_rust::UseSource::Framework => {
            let source_name = use_.source_name().clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such framework capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::UseSource::Capability(_) => {
            let use_ = use_.clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Capability {
                    source_capability: ComponentCapability::Use(use_.clone()),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::UseSource::Debug => component_input.environment().debug().lazy_get(
            use_protocol.source_name.clone(),
            RoutingError::use_from_environment_not_found(
                &component.moniker,
                "protocol",
                &use_protocol.source_name,
            ),
        ),
        // UseSource::Environment is not used for protocol capabilities
        cm_rust::UseSource::Environment => return,
    };
    program_input_dict.insert_capability(
        &use_protocol.target_path,
        router.with_availability(*use_.availability()).into(),
    );
}

/// Builds a router that obtains a capability that the program uses from `parent`.
///
/// The capability is usually an entry in the `component_input.capabilities` dict unless it is
/// overridden by an eponymous capability in the `program_input_dict_additions` when started.
fn use_from_parent_router(
    component_input: &ComponentInput,
    source_path: impl IterablePath + 'static + Debug,
    weak_component: WeakComponentInstance,
) -> Router {
    let component_input_capability = component_input.capabilities().lazy_get(
        source_path.clone(),
        RoutingError::use_from_parent_not_found(
            &weak_component.moniker,
            source_path.iter_segments().join("/"),
        ),
    );

    Router::new(move |request| {
        let source_path = source_path.clone();
        let component_input_capability = component_input_capability.clone();
        let Ok(component) = weak_component.upgrade() else {
            return std::future::ready(Err(RoutingError::from(
                ComponentInstanceError::InstanceNotFound {
                    moniker: weak_component.moniker.clone(),
                },
            )
            .into()))
            .boxed();
        };
        async move {
            let router = {
                if let Some(resolved_state) = component.lock_state().await.get_resolved_state() {
                    let additions = resolved_state.program_input_dict_additions.as_ref();
                    match additions.and_then(|a| a.get_capability(&source_path)) {
                        // There's an addition to the program input dictionary for this
                        // capability, let's use it.
                        Some(Capability::Open(o)) => Router::new_ok(o),
                        // There's no addition to the program input dictionary for this
                        // capability, let's use the component input dictionary.
                        _ => component_input_capability,
                    }
                } else {
                    // If the component is not resolved and/or does not have additions to the
                    // program input dictionary, then route this capability without any
                    // additions.
                    //
                    // NOTE: there's a chance that the component is in the shutdown stage here.
                    // The stop action clears the program_input_dict_additions, so even if
                    // additions were set the last time the component was run they won't apply
                    // after the component has stopped.
                    component_input_capability
                }
            };
            router.route(request).await
        }
        .boxed()
    })
}

fn is_supported_offer(offer: &cm_rust::OfferDecl) -> bool {
    matches!(offer, cm_rust::OfferDecl::Protocol(_) | cm_rust::OfferDecl::Dictionary(_))
}

fn extend_dict_with_offer(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    offer: &cm_rust::OfferDecl,
    target_dict: &mut Dict,
) {
    // We only support protocol and dictionary capabilities right now
    if !is_supported_offer(offer) {
        return;
    }
    let source_path = offer.source_path();
    let target_name = offer.target_name();
    if target_dict.get_capability(&source_path).is_some() {
        warn!(
            "duplicate sources for protocol {} in a dict, unable to populate dict entry",
            target_name
        );
        target_dict.remove_capability(target_name);
        return;
    }
    let router = match offer.source() {
        cm_rust::OfferSource::Parent => component_input.capabilities().lazy_get(
            source_path.to_owned(),
            RoutingError::offer_from_parent_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::OfferSource::Self_ => component.program_output().lazy_get(
            source_path.to_owned(),
            RoutingError::offer_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            let Some(child) = children.get(&child_name) else { return };
            child.component_output().lazy_get(
                source_path.to_owned(),
                RoutingError::offer_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    offer.source_name().clone(),
                ),
            )
        }
        cm_rust::OfferSource::Framework => {
            if offer.is_from_dictionary() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = offer.source_name().clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such framework capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::OfferSource::Capability(_) => {
            let offer = offer.clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Capability {
                    source_capability: ComponentCapability::Offer(offer.clone()),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::OfferSource::Void => new_unit_router(),
        // This is only relevant for services, so this arm is never reached.
        cm_rust::OfferSource::Collection(_name) => return,
    };
    target_dict
        .insert_capability(target_name, router.with_availability(*offer.availability()).into());
}

pub fn is_supported_expose(expose: &cm_rust::ExposeDecl) -> bool {
    matches!(expose, cm_rust::ExposeDecl::Protocol(_) | cm_rust::ExposeDecl::Dictionary(_))
}

fn extend_dict_with_expose(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    expose: &cm_rust::ExposeDecl,
    target_dict: &Dict,
) {
    if !is_supported_expose(expose) {
        return;
    }
    // We only support exposing to the parent right now
    if expose.target() != &cm_rust::ExposeTarget::Parent {
        return;
    }
    let source_path = expose.source_path();
    let target_name = expose.target_name();

    let router = match expose.source() {
        cm_rust::ExposeSource::Self_ => component.program_output().lazy_get(
            source_path.to_owned(),
            RoutingError::expose_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::ExposeSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid static child name");
            if let Some(child) = children.get(&child_name) {
                child.component_output().lazy_get(
                    source_path.to_owned(),
                    RoutingError::expose_from_child_expose_not_found(
                        child.moniker.leaf().unwrap(),
                        &child.moniker.parent().unwrap(),
                        expose.source_name().clone(),
                    ),
                )
            } else {
                return;
            }
        }
        cm_rust::ExposeSource::Framework => {
            if expose.is_from_dictionary() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = expose.source_name().clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such framework capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::ExposeSource::Capability(_) => {
            let expose = expose.clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Capability {
                    source_capability: ComponentCapability::Expose(expose.clone()),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::ExposeSource::Void => new_unit_router(),
        // This is only relevant for services, so this arm is never reached.
        cm_rust::ExposeSource::Collection(_name) => return,
    };
    target_dict
        .insert_capability(target_name, router.with_availability(*expose.availability()).into());
}

fn new_unit_router() -> Router {
    Router::new_ok(Unit {})
}
