// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, ResolvedInstanceState, WeakComponentInstance},
            routing::router::{Request, Router},
        },
        sandbox_util::{DictExt, LaunchTaskOnReceive},
    },
    ::routing::{
        capability_source::{ComponentCapability, InternalCapability},
        component_instance::ComponentInstanceInterface,
        error::{ComponentInstanceError, RoutingError},
    },
    bedrock_error::BedrockError,
    cm_rust::{ExposeDeclCommon, OfferDeclCommon, SourceName, SourcePath, UseDeclCommon},
    cm_types::{IterablePath, Name, SeparatedPath},
    fidl_fuchsia_component_decl as fdecl,
    futures::FutureExt,
    itertools::Itertools,
    moniker::{ChildName, ChildNameBase, MonikerBase},
    sandbox::{Capability, Dict, Unit},
    std::{collections::HashMap, iter, sync::Arc},
    tracing::warn,
};

/// The dicts a component receives from its parent.
#[derive(Default, Clone)]
pub struct ComponentInput {
    /// Capabilities offered to a component by its parent.
    pub capabilities: Dict,

    /// Capabilities available to a component through its environment.
    pub environment: ComponentEnvironment,
}

impl ComponentInput {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn new(
        capabilities: Dict,
        default_environment: &ComponentEnvironment,
        environments: &HashMap<Name, ComponentEnvironment>,
        environment_name: Option<Name>,
    ) -> Self {
        let environment = if let Some(environment_name) = environment_name {
            environments
                .get(&environment_name)
                .expect("child references nonexistent environment, this should be prevented in manifest validation")
                .clone()
        } else {
            default_environment.clone()
        };
        Self { capabilities, environment }
    }

    /// Creates a new ComponentInput with entries cloned from this ComponentInput.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn shallow_copy(&self) -> Self {
        Self {
            capabilities: self.capabilities.copy(),
            environment: self.environment.shallow_copy(),
        }
    }

    pub fn environment(&self) -> &ComponentEnvironment {
        &self.environment
    }

    pub fn insert_capability<'a>(
        &self,
        path: impl Iterator<Item = &'a str>,
        capability: Capability,
    ) {
        self.capabilities.insert_capability(path, capability.into())
    }
}

/// The capabilities a component has in its environment.
#[derive(Default, Clone)]
pub struct ComponentEnvironment {
    /// Capabilities listed in the `debug_capabilities` portion of its environment.
    debug_capabilities: Dict,
}

impl ComponentEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shallow_copy(&self) -> Self {
        Self { debug_capabilities: self.debug_capabilities.copy() }
    }
}

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
    child_inputs: &mut HashMap<Name, ComponentInput>,
    collection_inputs: &mut HashMap<Name, ComponentInput>,
    environments: &mut HashMap<Name, ComponentEnvironment>,
) {
    let declared_dictionaries = Dict::new();

    for environment_decl in &decl.environments {
        environments.insert(
            Name::new(&environment_decl.name).unwrap(),
            build_environment(
                component,
                children,
                component_input,
                program_output_dict,
                environment_decl,
            ),
        );
    }

    for child in &decl.children {
        let mut input = ComponentInput::empty();
        if let Some(environment_name) = &child.environment {
            input.environment = environments
                .get(&Name::new(environment_name.clone()).unwrap())
                .expect(
                    "child references nonexistent environment, \
                    this should be prevented in manifest validation",
                )
                .clone();
        } else {
            input.environment = component_input.environment.clone();
        }
        let child_name = Name::new(&child.name).unwrap();
        child_inputs.insert(child_name, input);
    }

    for collection in &decl.collections {
        let mut input = ComponentInput::empty();
        if let Some(environment_name) = &collection.environment {
            input.environment = environments
                .get(&Name::new(environment_name.clone()).unwrap())
                .expect(
                    "collection references nonexistent environment, \
                    this should be prevented in manifest validation",
                )
                .clone();
        } else {
            input.environment = component_input.environment.clone();
        }
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
        extend_dict_with_use(
            component,
            children,
            component_input,
            program_input_dict,
            program_output_dict,
            use_,
        );
    }

    for offer in &decl.offers {
        // We only support protocol and dictionary capabilities right now
        if !is_supported_offer(offer) {
            continue;
        }
        let mut _placeholder_dict = None;
        let target_dict = match offer.target() {
            cm_rust::OfferTarget::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name = Name::new(&child_ref.name).unwrap();
                &mut child_inputs.entry(child_name).or_insert(ComponentInput::empty()).capabilities
            }
            cm_rust::OfferTarget::Collection(name) => {
                &mut collection_inputs
                    .entry(name.clone())
                    .or_insert(ComponentInput::empty())
                    .capabilities
            }
            cm_rust::OfferTarget::Capability(name) => {
                let mut entries = declared_dictionaries.lock_entries();
                _placeholder_dict = Some(
                    entries
                        .entry(name.to_string())
                        .or_insert_with(|| Capability::Dictionary(Dict::new()))
                        .clone(),
                );
                let ref_: &mut Dict = match _placeholder_dict.as_mut().unwrap() {
                    Capability::Dictionary(d) => d,
                    _ => panic!("wrong type in dict"),
                };
                ref_
            }
        };
        extend_dict_with_offer(
            component,
            children,
            component_input,
            program_output_dict,
            offer,
            target_dict,
        );
    }

    for expose in &decl.exposes {
        extend_dict_with_expose(
            component,
            children,
            program_output_dict,
            expose,
            component_output_dict,
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
        cm_rust::CapabilityDecl::Protocol(p) => {
            let router = ResolvedInstanceState::start_component_on_request(
                component,
                decl,
                capability.name().clone(),
            );
            let router = router.with_policy_check(
                CapabilitySource::Component {
                    capability: ComponentCapability::Protocol(p.clone()),
                    component: component.as_weak(),
                },
                component.policy_checker().clone(),
            );
            program_output_dict
                .insert_capability(iter::once(capability.name().as_str()), router.into());
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
        _ => {
            // Capabilities not supported in bedrock yet.
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
            cm_rust::DictionarySource::Parent => component_input.capabilities.get_router_or_error(
                source_path.iter_segments(),
                RoutingError::use_from_parent_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                )
                .into(),
            ),
            cm_rust::DictionarySource::Self_ => program_output_dict.get_router_or_error(
                source_path.iter_segments(),
                RoutingError::use_from_self_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                )
                .into(),
            ),
            cm_rust::DictionarySource::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name =
                    ChildName::parse(child_ref.name.as_str()).expect("invalid child name");
                match children.get(&child_name) {
                    Some(child) => {
                        let child = child.as_weak();
                        new_forwarding_router_to_child(
                            child,
                            source_path.clone(),
                            RoutingError::BedrockSourceDictionaryExposeNotFound,
                        )
                    }
                    None => Router::new_error(
                        RoutingError::use_from_child_instance_not_found(
                            &child_name,
                            &component.moniker,
                            source_path.iter_segments().join("/"),
                        )
                        .into(),
                    ),
                }
            }
        };
        make_dict_extending_router(component.as_weak(), dict.clone(), source_dict_router)
    } else {
        Router::from_capability(dict.clone().into())
    };
    declared_dictionaries.insert_capability(iter::once(decl.name.as_str()), dict.into());
    program_output_dict.insert_capability(iter::once(decl.name.as_str()), router.into());
}

fn build_environment(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    environment_decl: &cm_rust::EnvironmentDecl,
) -> ComponentEnvironment {
    let mut environment = ComponentEnvironment::new();
    if environment_decl.extends == fdecl::EnvironmentExtends::Realm {
        environment = component_input.environment.shallow_copy();
    }
    for debug_registration in &environment_decl.debug_capabilities {
        let cm_rust::DebugRegistration::Protocol(debug_protocol) = debug_registration;
        let source_path =
            SeparatedPath { dirname: None, basename: debug_protocol.source_name.to_string() };
        let router = match &debug_protocol.source {
            cm_rust::RegistrationSource::Parent => {
                use_from_parent_router(component_input, source_path, component.as_weak())
            }
            cm_rust::RegistrationSource::Self_ => program_output_dict.get_router_or_error(
                source_path.iter_segments(),
                RoutingError::use_from_self_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                )
                .into(),
            ),
            cm_rust::RegistrationSource::Child(child_name) => {
                let child_name = ChildName::parse(child_name).expect("invalid child name");
                let Some(child) = children.get(&child_name) else { continue };
                let weak_child = WeakComponentInstance::new(child);
                new_forwarding_router_to_child(
                    weak_child,
                    source_path,
                    RoutingError::use_from_child_expose_not_found(
                        child.moniker.leaf().unwrap(),
                        &child.moniker.parent().unwrap(),
                        debug_protocol.source_name.clone(),
                    ),
                )
            }
        };
        environment
            .debug_capabilities
            .insert_capability(iter::once(debug_protocol.target_name.as_str()), router.into());
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
    program_output_dict: &Dict,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_input: &mut ComponentInput,
) {
    for offer in dynamic_offers {
        extend_dict_with_offer(
            component,
            children,
            component_input,
            program_output_dict,
            offer,
            &mut target_input.capabilities,
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
    program_output_dict: &Dict,
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
        cm_rust::UseSource::Self_ => program_output_dict.get_router_or_error(
            source_path.iter_segments(),
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
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                weak_child,
                source_path.to_owned(),
                RoutingError::use_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    use_.source_name().clone(),
                ),
            )
        }
        cm_rust::UseSource::Framework if use_.is_from_dictionary() => Router::new_error(
            RoutingError::capability_from_framework_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            )
            .into(),
        ),
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
        cm_rust::UseSource::Debug => {
            component_input.environment.debug_capabilities.get_router_or_error(
                iter::once(use_protocol.source_name.as_str()),
                RoutingError::use_from_environment_not_found(
                    &component.moniker,
                    "protocol",
                    &use_protocol.source_name,
                ),
            )
        }
        // UseSource::Environment is not used for protocol capabilities
        cm_rust::UseSource::Environment => return,
    };
    program_input_dict.insert_capability(
        use_protocol.target_path.iter_segments(),
        router.with_availability(*use_.availability()).into(),
    );
}

/// Builds a router that obtains a capability that the program uses from `parent`.
///
/// The capability is usually an entry in the `component_input.capabilities` dict unless it is
/// overridden by an eponymous capability in the `program_input_dict_additions` when started.
fn use_from_parent_router(
    component_input: &ComponentInput,
    source_path: impl IterablePath + 'static,
    weak_component: WeakComponentInstance,
) -> Router {
    let component_input_capability = component_input.capabilities.get_router_or_error(
        source_path.iter_segments(),
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
                let state = match component.lock_resolved_state().await {
                    Ok(state) => state,
                    Err(err) => {
                        return Err(RoutingError::from(ComponentInstanceError::resolve_failed(
                            component.moniker.clone(),
                            err,
                        ))
                        .into());
                    }
                };
                // Try to get the capability from the incoming dict, which was passed when the child was
                // started.
                //
                // Unlike the program input dict below that contains Routers created by
                // component manager, the incoming dict may contain capabilities created externally.
                // Currently there is no way to create a Router externally, so assume these
                // are Open capabilities and convert them to Router here.
                //
                // TODO(https://fxbug.dev/319542502): Convert from the external Router type, once it
                // exists.
                state
                    .program_input_dict_additions
                    .as_ref()
                    .and_then(|dict| match dict.get_capability(source_path.iter_segments()) {
                        Some(Capability::Open(o)) => Some(Router::from_capability(o.into())),
                        _ => None,
                    })
                    // Try to get the capability from the component input dict, created from static
                    // routes when the component was resolved.
                    .unwrap_or(component_input_capability)
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
    program_output_dict: &Dict,
    offer: &cm_rust::OfferDecl,
    target_dict: &mut Dict,
) {
    // We only support protocol and dictionary capabilities right now
    if !is_supported_offer(offer) {
        return;
    }
    let source_path = offer.source_path();
    let target_name = offer.target_name();
    if target_dict.get_capability(source_path.iter_segments()).is_some() {
        warn!(
            "duplicate sources for protocol {} in a dict, unable to populate dict entry",
            target_name
        );
        target_dict.remove_capability(iter::once(target_name.as_str()));
        return;
    }
    let router = match offer.source() {
        cm_rust::OfferSource::Parent => component_input.capabilities.get_router_or_error(
            source_path.iter_segments(),
            RoutingError::offer_from_parent_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::OfferSource::Self_ => program_output_dict.get_router_or_error(
            source_path.iter_segments(),
            RoutingError::offer_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            let Some(child) = children.get(&child_name) else { return };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                weak_child,
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
    target_dict.insert_capability(
        iter::once(target_name.as_str()),
        router.with_availability(*offer.availability()).into(),
    );
}

pub fn is_supported_expose(expose: &cm_rust::ExposeDecl) -> bool {
    matches!(expose, cm_rust::ExposeDecl::Protocol(_) | cm_rust::ExposeDecl::Dictionary(_))
}

fn extend_dict_with_expose(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    program_output_dict: &Dict,
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
        cm_rust::ExposeSource::Self_ => program_output_dict.get_router_or_error(
            source_path.iter_segments(),
            RoutingError::expose_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::ExposeSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid static child name");
            if let Some(child) = children.get(&child_name) {
                let weak_child = WeakComponentInstance::new(child);
                new_forwarding_router_to_child(
                    weak_child,
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
    target_dict.insert_capability(
        iter::once(target_name.as_str()),
        router.with_availability(*expose.availability()).into(),
    );
}

fn new_unit_router() -> Router {
    Router::new_non_async(|_: Request| Ok(Unit {}.into()))
}

fn new_forwarding_router_to_child(
    weak_child: WeakComponentInstance,
    capability_path: impl IterablePath + 'static,
    expose_not_found_error: RoutingError,
) -> Router {
    Router::new(move |request: Request| {
        forward_request_to_child(
            weak_child.clone(),
            capability_path.clone(),
            expose_not_found_error.clone(),
            request,
        )
        .boxed()
    })
}

async fn forward_request_to_child(
    weak_child: WeakComponentInstance,
    capability_path: impl IterablePath + 'static,
    expose_not_found_error: RoutingError,
    request: Request,
) -> Result<Capability, BedrockError> {
    let router = {
        let child = weak_child.upgrade().map_err(|e| RoutingError::ComponentInstanceError(e))?;
        let child_state = child.lock_resolved_state().await.map_err(|e| {
            RoutingError::ComponentInstanceError(ComponentInstanceError::resolve_failed(
                child.moniker.clone(),
                e,
            ))
        })?;
        child_state
            .component_output_dict
            .get_router_or_error(capability_path.iter_segments(), expose_not_found_error.clone())
    };
    router.route(request).await
}
