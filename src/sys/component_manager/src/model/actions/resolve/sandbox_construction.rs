// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, WeakComponentInstance},
            routing::router::{Completer, Request, Router},
        },
        sandbox_util::{DictExt, LaunchTaskOnReceive},
    },
    ::routing::{
        capability_source::{ComponentCapability, InternalCapability},
        component_instance::ComponentInstanceInterface,
        error::{ComponentInstanceError, RoutingError},
    },
    cm_rust::{self, ExposeDeclCommon, OfferDeclCommon, SourceName, SourcePath, UseDeclCommon},
    cm_types::{Name, SeparatedPath},
    moniker::{ChildName, ChildNameBase, MonikerBase},
    sandbox::{Capability, Dict, ErasedCapability, Open, Unit},
    std::{collections::HashMap, iter, sync::Arc},
    tracing::warn,
};

pub struct CapabilitySourceFactory {
    factory_fn: Box<dyn FnOnce(WeakComponentInstance) -> CapabilitySource + Send + 'static>,
}

impl CapabilitySourceFactory {
    fn new<F>(factory_fn: F) -> Self
    where
        F: FnOnce(WeakComponentInstance) -> CapabilitySource + Send + 'static,
    {
        Self { factory_fn: Box::new(factory_fn) }
    }

    pub fn run(self, component: WeakComponentInstance) -> CapabilitySource {
        (self.factory_fn)(component)
    }
}

/// The dicts a component receives from its parent.
#[derive(Default, Clone)]
pub struct ComponentInput {
    capabilities: Dict,
}

impl ComponentInput {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn new(capabilities: Dict) -> Self {
        Self { capabilities }
    }

    pub fn insert_capability<'a, C>(&self, path: impl Iterator<Item = &'a str>, capability: C)
    where
        C: ErasedCapability + Capability,
    {
        self.capabilities.insert_capability(path, capability)
    }
}

/// The dicts a component holds once it has been resolved.
#[derive(Default)]
pub struct ComponentSandbox {
    /// Initial dicts for children and collections
    pub child_inputs: HashMap<Name, ComponentInput>,
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
    collection_dicts: &mut HashMap<Name, Dict>,
) -> ComponentSandbox {
    let mut output = ComponentSandbox::default();

    for child in &decl.children {
        let child_name = Name::new(&child.name).unwrap();
        output.child_inputs.insert(child_name, ComponentInput::empty());
    }

    for collection in &decl.collections {
        collection_dicts.insert(collection.name.clone(), Dict::new());
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
                &mut output
                    .child_inputs
                    .entry(child_name)
                    .or_insert(ComponentInput::empty())
                    .capabilities
            }
            cm_rust::OfferTarget::Collection(name) => {
                collection_dicts.entry(name.clone()).or_insert(Dict::new())
            }
            cm_rust::OfferTarget::Capability(name) => {
                let mut entries = program_output_dict.lock_entries();
                _placeholder_dict = Some(
                    entries
                        .entry(name.to_string())
                        .or_insert_with(|| Box::new(Dict::new()))
                        .clone(),
                );
                let ref_: &mut Dict =
                    _placeholder_dict.as_mut().unwrap().try_into().expect("wrong type in dict");
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

    output
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
        cm_rust::UseSource::Self_ => {
            if source_path.dirname.is_some() {
                let Some(router) =
                    program_output_dict.get_routable::<Dict>(source_path.iter_segments())
                else {
                    return;
                };
                router
            } else {
                let Some(router) =
                    program_output_dict.get_routable::<Router>(source_path.iter_segments())
                else {
                    return;
                };
                router
            }
        }
        cm_rust::UseSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid child name");
            let Some(child) = children.get(&child_name) else { return };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                component,
                weak_child,
                source_path.to_owned(),
                RoutingError::use_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    use_.source_name().clone(),
                ),
            )
        }
        cm_rust::UseSource::Framework => {
            if source_path.dirname.is_some() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = use_.source_name().clone();
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component,
                }),
            )
            .into_router()
        }
        cm_rust::UseSource::Capability(_) => {
            let use_ = use_.clone();
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Use(use_.clone()),
                    component,
                }),
            )
            .into_router()
        }
        // Unimplemented
        cm_rust::UseSource::Debug | cm_rust::UseSource::Environment => return,
    };
    program_input_dict.insert_capability(
        use_protocol.target_path.iter_segments(),
        router.with_availability(*use_.availability()),
    );
}

/// Builds a router that obtains a capability that the program uses from `parent`.
///
/// The capability is usually an entry in the `component_input.capabilities` dict
/// unless it is overridden by an eponymous capability in the `incoming_dict` when started.
fn use_from_parent_router(
    component_input: &ComponentInput,
    source_path: SeparatedPath,
    weak_component: WeakComponentInstance,
) -> Router {
    let component_input_capability = Router::from_routable(component_input.capabilities.clone())
        .with_path(source_path.iter_segments());

    Router::new(move |request, completer| {
        let source_path = source_path.clone();
        let component_input_capability = component_input_capability.clone();
        let Ok(component) = weak_component.upgrade() else {
            return completer.complete(Err(RoutingError::from(
                ComponentInstanceError::InstanceNotFound {
                    moniker: weak_component.moniker.clone(),
                },
            )
            .into()));
        };
        component.clone().blocking_task_group().spawn(async move {
            let state = match component.lock_resolved_state().await {
                Ok(state) => state,
                Err(err) => {
                    return completer.complete(Err(RoutingError::from(
                        ComponentInstanceError::resolve_failed(component.moniker.clone(), err),
                    )
                    .into()))
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
            let router = state
                .incoming_dict
                .as_ref()
                .and_then(|dict| {
                    dict.get_capability::<Open>(source_path.iter_segments())
                        .map(Router::from_routable)
                })
                // Try to get the capability from the component input dict, created from static
                // routes when the component was resolved.
                .unwrap_or(component_input_capability);
            router.route(request, completer);
        });
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
    if target_dict.get_routable::<Router>(source_path.iter_segments()).is_some() {
        warn!(
            "duplicate sources for protocol {} in a dict, unable to populate dict entry",
            target_name
        );
        target_dict.remove_capability(iter::once(target_name.as_str()));
        return;
    }
    let router = match offer.source() {
        cm_rust::OfferSource::Parent => {
            let Some(router) =
                component_input.capabilities.get_routable::<Router>(source_path.iter_segments())
            else {
                return;
            };
            router
        }
        cm_rust::OfferSource::Self_ => {
            if matches!(offer, cm_rust::OfferDecl::Dictionary(_)) || source_path.dirname.is_some() {
                let Some(router) =
                    program_output_dict.get_routable::<Dict>(source_path.iter_segments())
                else {
                    return;
                };
                router
            } else {
                let Some(router) =
                    program_output_dict.get_routable::<Router>(source_path.iter_segments())
                else {
                    return;
                };
                router
            }
        }
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            let Some(child) = children.get(&child_name) else { return };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                component,
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
            if source_path.dirname.is_some() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = offer.source_name().clone();
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component,
                }),
            )
            .into_router()
        }
        cm_rust::OfferSource::Capability(_) => {
            let offer = offer.clone();
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Offer(offer.clone()),
                    component,
                }),
            )
            .into_router()
        }
        cm_rust::OfferSource::Void => new_unit_router(),
        // This is only relevant for services, so this arm is never reached.
        cm_rust::OfferSource::Collection(_name) => return,
    };
    target_dict.insert_capability(
        iter::once(target_name.as_str()),
        router.with_availability(*offer.availability()),
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
        cm_rust::ExposeSource::Self_ => {
            if matches!(expose, cm_rust::ExposeDecl::Dictionary(_)) || source_path.dirname.is_some()
            {
                let Some(router) =
                    program_output_dict.get_routable::<Dict>(source_path.iter_segments())
                else {
                    return;
                };
                router
            } else {
                let Some(router) =
                    program_output_dict.get_routable::<Router>(source_path.iter_segments())
                else {
                    return;
                };
                router
            }
        }
        cm_rust::ExposeSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid static child name");
            if let Some(child) = children.get(&child_name) {
                let weak_child = WeakComponentInstance::new(child);
                new_forwarding_router_to_child(
                    component,
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
            if source_path.dirname.is_some() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = expose.source_name().clone();
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component,
                }),
            )
            .into_router()
        }
        cm_rust::ExposeSource::Capability(_) => {
            let expose = expose.clone();
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Expose(expose.clone()),
                    component,
                }),
            )
            .into_router()
        }
        cm_rust::ExposeSource::Void => new_unit_router(),
        // This is only relevant for services, so this arm is never reached.
        cm_rust::ExposeSource::Collection(_name) => return,
    };
    target_dict.insert_capability(
        iter::once(target_name.as_str()),
        router.with_availability(*expose.availability()),
    );
}

fn new_unit_router() -> Router {
    Router::new(|_: Request, completer: Completer| completer.complete(Ok(Box::new(Unit {}))))
}

fn new_forwarding_router_to_child(
    component: &Arc<ComponentInstance>,
    weak_child: WeakComponentInstance,
    capability_path: SeparatedPath,
    expose_not_found_error: RoutingError,
) -> Router {
    let task_group = component.nonblocking_task_group().as_weak();
    Router::new(move |request: Request, completer: Completer| {
        task_group.spawn(forward_request_to_child(
            weak_child.clone(),
            capability_path.clone(),
            expose_not_found_error.clone(),
            request,
            completer,
        ));
    })
}

async fn forward_request_to_child(
    weak_child: WeakComponentInstance,
    capability_path: SeparatedPath,
    expose_not_found_error: RoutingError,
    request: Request,
    completer: Completer,
) {
    let mut completer = Some(completer);
    let res: Result<(), RoutingError> = async {
        let child = weak_child.upgrade()?;
        let child_state = child
            .lock_resolved_state()
            .await
            .map_err(|e| ComponentInstanceError::resolve_failed(child.moniker.clone(), e))?;
        if let Some(router) = child_state
            .component_output_dict
            .get_routable::<Router>(capability_path.iter_segments())
        {
            router.route(request, completer.take().unwrap());
            return Ok(());
        }
        return Err(expose_not_found_error.clone().into());
    }
    .await;

    if let Err(err) = res {
        completer.take().unwrap().complete(Err(err.into()));
    }
}
