// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, WeakComponentInstance},
            error::ModelError,
        },
        sandbox_util::{new_terminating_router, DictExt, Message},
    },
    ::routing::{
        capability_source::{ComponentCapability, InternalCapability},
        error::RoutingError,
        Completer, Request, Router,
    },
    anyhow::format_err,
    cm_rust::{self, ExposeDeclCommon, OfferDeclCommon, SourceName, UseDeclCommon},
    cm_types::Name,
    moniker::{ChildName, ChildNameBase, MonikerBase},
    sandbox::{Dict, Receiver},
    std::{collections::HashMap, sync::Arc},
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

/// The dicts a component holds once it has been resolved.
#[derive(Default)]
pub struct ComponentSandbox {
    /// Initial dicts for children and collections
    pub child_dicts: HashMap<Name, Dict>,
    /// Capability source factories and receivers for capabilities that are dispatched through the
    /// hook system.
    pub sources_and_receivers: Vec<(CapabilitySourceFactory, Receiver<Message>)>,
}

/// Once a component has been resolved and its manifest becomes known, this function produces the
/// various dicts the component needs based on the contents of its manifest.
pub fn build_component_sandbox(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    component_input_dict: &Dict,
    component_output_dict: &mut Dict,
    program_input_dict: &mut Dict,
    program_output_dict: &mut Dict,
    collection_dicts: &mut HashMap<Name, Dict>,
) -> ComponentSandbox {
    let mut output = ComponentSandbox::default();

    for child in &decl.children {
        let child_name = Name::new(&child.name).unwrap();
        output.child_dicts.insert(child_name, Dict::new());
    }

    for collection in &decl.collections {
        collection_dicts.insert(collection.name.clone(), Dict::new());
    }

    // All declared capabilities must have a receiver, unless we are non-executable.
    if decl.program.is_some() {
        for capability in &decl.capabilities {
            // We only support protocol capabilities right now
            match &capability {
                cm_rust::CapabilityDecl::Protocol(_) => (),
                _ => continue,
            }
            program_output_dict
                .get_or_insert_protocol_mut(capability.name())
                .insert_receiver(Receiver::new());
        }
    }

    for use_ in &decl.uses {
        extend_dict_with_use(
            component,
            children,
            &component_input_dict,
            program_input_dict,
            &program_output_dict,
            use_,
            &mut output.sources_and_receivers,
        );
    }

    for offer in &decl.offers {
        // We only support protocol capabilities right now
        match &offer {
            cm_rust::OfferDecl::Protocol(_) => (),
            _ => continue,
        }
        let target_dict = match offer.target() {
            cm_rust::OfferTarget::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name = Name::new(&child_ref.name).unwrap();
                output.child_dicts.entry(child_name).or_insert(Dict::new())
            }
            cm_rust::OfferTarget::Collection(name) => {
                collection_dicts.entry(name.clone()).or_insert(Dict::new())
            }
            cm_rust::OfferTarget::Capability(_name) => {
                // TODO(fxbug.dev/301674053): Support dictionary routing.
                continue;
            }
        };
        extend_dict_with_offer(
            component,
            children,
            &component_input_dict,
            &program_output_dict,
            offer,
            target_dict,
            &mut output.sources_and_receivers,
        );
    }

    for expose in &decl.exposes {
        extend_dict_with_expose(
            component,
            children,
            &program_output_dict,
            expose,
            component_output_dict,
            &mut output.sources_and_receivers,
        );
    }

    output
}

/// Extends the given dict based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_dict`.
pub fn extend_dict_with_offers(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input_dict: &Dict,
    program_output_dict: &Dict,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_dict: &mut Dict,
) -> Vec<(CapabilitySourceFactory, Receiver<Message>)> {
    let mut sources_and_receivers = vec![];
    for offer in dynamic_offers {
        extend_dict_with_offer(
            component,
            children,
            component_input_dict,
            program_output_dict,
            offer,
            target_dict,
            &mut sources_and_receivers,
        );
    }
    sources_and_receivers
}

fn extend_dict_with_use(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input_dict: &Dict,
    program_input_dict: &mut Dict,
    program_output_dict: &Dict,
    use_: &cm_rust::UseDecl,
    sources_and_receivers: &mut Vec<(CapabilitySourceFactory, Receiver<Message>)>,
) {
    // We only support protocol capabilities right now
    match &use_ {
        cm_rust::UseDecl::Protocol(_) => (),
        _ => return,
    }

    let source_name = use_.source_name();
    let cm_rust::UseDecl::Protocol(cm_rust::UseProtocolDecl { target_path, .. }) = use_ else {
        unreachable!();
    };
    let router = match use_.source() {
        cm_rust::UseSource::Parent => {
            let Some(cap_dict) = component_input_dict.get_protocol(source_name) else { return };
            let Some(parent_router) = cap_dict.get_router() else { return };
            parent_router.clone()
        }
        cm_rust::UseSource::Self_ => {
            let Some(cap_dict) = program_output_dict.get_protocol(source_name) else { return };
            let Some(receiver) = cap_dict.get_receiver().map(|r| r.clone()) else { return };
            new_terminating_router(receiver.new_sender())
        }
        cm_rust::UseSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid child name");
            let Some(child) = children.get(&child_name) else { return };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                component,
                weak_child,
                source_name.clone(),
                RoutingError::use_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    source_name.clone(),
                ),
            )
            .availability(*use_.availability())
        }
        cm_rust::UseSource::Framework => {
            let receiver = Receiver::new();
            let source_name = source_name.clone();
            let receiver_clone = receiver.clone();
            sources_and_receivers.push((
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name.clone()),
                    component,
                }),
                receiver_clone,
            ));
            new_terminating_router(receiver.new_sender())
        }
        cm_rust::UseSource::Capability(_) => {
            let receiver = Receiver::new();
            let use_ = use_.clone();
            let receiver_clone = receiver.clone();
            sources_and_receivers.push((
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Use(use_.clone()),
                    component,
                }),
                receiver_clone,
            ));
            new_terminating_router(receiver.new_sender())
        }
        _ => return, // unsupported
    };
    program_input_dict.insert_router(target_path.split(), router);
}

fn extend_dict_with_offer(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input_dict: &Dict,
    program_output_dict: &Dict,
    offer: &cm_rust::OfferDecl,
    target_dict: &mut Dict,
    sources_and_receivers: &mut Vec<(CapabilitySourceFactory, Receiver<Message>)>,
) {
    // We only support protocol capabilities right now
    match &offer {
        cm_rust::OfferDecl::Protocol(_) => (),
        _ => return,
    }
    let source_name = offer.source_name();
    let target_name = offer.target_name();
    if let Some(cap_dict) = target_dict.get_protocol_mut(target_name) {
        if cap_dict.get_router().is_some() {
            warn!(
                "duplicate sources for protocol {} in a dict, unable to populate dict entry",
                target_name
            );
            cap_dict.remove_router();
            return;
        }
    }
    let router = match offer.source() {
        cm_rust::OfferSource::Parent => {
            let Some(source_cap_dict) = component_input_dict.get_protocol(source_name) else {
                return;
            };
            let Some(parent_router) = source_cap_dict.get_router() else { return };
            parent_router.clone().availability(*offer.availability())
        }
        cm_rust::OfferSource::Self_ => {
            let Some(receiver) = program_output_dict
                .get_protocol(source_name)
                .and_then(|c| c.get_receiver().map(|r| r.clone()))
            else {
                return;
            };
            new_router_for_receiver(receiver, *offer.availability())
        }
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            let Some(child) = children.get(&child_name) else { return };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                component,
                weak_child,
                source_name.clone(),
                RoutingError::offer_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    source_name.clone(),
                ),
            )
            .availability(*offer.availability())
        }
        cm_rust::OfferSource::Framework => {
            let source_name = source_name.clone();
            new_router_for_cm_hosted_receiver(
                *offer.availability(),
                sources_and_receivers,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name.clone()),
                    component,
                }),
            )
        }
        cm_rust::OfferSource::Capability(_) => {
            let offer = offer.clone();
            new_router_for_cm_hosted_receiver(
                *offer.availability(),
                sources_and_receivers,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Offer(offer.clone()),
                    component,
                }),
            )
        }
        _ => return, // unsupported
    };
    target_dict.get_or_insert_protocol_mut(target_name).insert_router(router);
}

fn extend_dict_with_expose(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    program_output_dict: &Dict,
    expose: &cm_rust::ExposeDecl,
    target_dict: &mut Dict,
    sources_and_receivers: &mut Vec<(CapabilitySourceFactory, Receiver<Message>)>,
) {
    // We only support protocol capabilities right now
    match &expose {
        cm_rust::ExposeDecl::Protocol(_) => (),
        _ => return,
    }
    // We only support exposing to the parent right now
    if expose.target() != &cm_rust::ExposeTarget::Parent {
        return;
    }
    let source_name = expose.source_name();
    let target_name = expose.target_name();

    let router = match expose.source() {
        cm_rust::ExposeSource::Self_ => {
            if let Some(receiver) = program_output_dict
                .get_protocol(source_name)
                .and_then(|c| c.get_receiver().map(|r| r.clone()))
            {
                new_router_for_receiver(receiver, *expose.availability())
            } else {
                return;
            }
        }
        cm_rust::ExposeSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid static child name");
            if let Some(child) = children.get(&child_name) {
                let weak_child = WeakComponentInstance::new(child);
                new_forwarding_router_to_child(
                    component,
                    weak_child,
                    source_name.clone(),
                    RoutingError::expose_from_child_expose_not_found(
                        child.moniker.leaf().unwrap(),
                        &child.moniker.parent().unwrap(),
                        source_name.clone(),
                    ),
                )
                .availability(*expose.availability())
            } else {
                return;
            }
        }
        cm_rust::ExposeSource::Framework => {
            let source_name = source_name.clone();
            new_router_for_cm_hosted_receiver(
                *expose.availability(),
                sources_and_receivers,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name.clone()),
                    component,
                }),
            )
        }
        cm_rust::ExposeSource::Capability(_) => {
            let expose = expose.clone();
            new_router_for_cm_hosted_receiver(
                *expose.availability(),
                sources_and_receivers,
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Expose(expose.clone()),
                    component,
                }),
            )
        }
        _ => return, // unsupported
    };
    target_dict.get_or_insert_protocol_mut(target_name).insert_router(router);
}

fn new_router_for_cm_hosted_receiver(
    availability: cm_rust::Availability,
    sources_and_receivers: &mut Vec<(CapabilitySourceFactory, Receiver<Message>)>,
    cap_source_factory: CapabilitySourceFactory,
) -> Router {
    let receiver = Receiver::new();
    sources_and_receivers.push((cap_source_factory, receiver.clone()));
    new_router_for_receiver(receiver, availability)
}

fn new_router_for_receiver(
    receiver: Receiver<Message>,
    availability: cm_rust::Availability,
) -> Router {
    new_terminating_router(receiver.new_sender()).availability(availability)
}

fn new_forwarding_router_to_child(
    component: &Arc<ComponentInstance>,
    weak_child: WeakComponentInstance,
    capability_name: Name,
    expose_not_found_error: RoutingError,
) -> Router {
    let task_group = component.nonblocking_task_group().as_weak();
    Router::new(move |request: Request, completer: Completer| {
        task_group.spawn(forward_request_to_child(
            weak_child.clone(),
            capability_name.clone(),
            expose_not_found_error.clone(),
            request,
            completer,
        ));
    })
}

async fn forward_request_to_child(
    weak_child: WeakComponentInstance,
    capability_name: Name,
    expose_not_found_error: RoutingError,
    request: Request,
    completer: Completer,
) {
    let mut completer = Some(completer);
    let res: Result<(), ModelError> = async {
        let child = weak_child.upgrade()?;
        let child_state = child.lock_resolved_state().await?;
        if let Some(cap_dict) = child_state.component_output_dict.get_protocol(&capability_name) {
            if let Some(router) = cap_dict.get_router() {
                router.route(request, completer.take().unwrap());
                return Ok(());
            }
        }
        return Err(expose_not_found_error.clone().into());
    }
    .await;

    if let Err(err) = res {
        completer.take().unwrap().complete(Err(format_err!("failed to route: {:?}", err)));
    }
}
