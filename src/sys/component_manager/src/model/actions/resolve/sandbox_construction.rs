// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::component::WeakComponentInstance,
        sandbox_util::{new_terminating_router, DictExt, Message},
    },
    ::routing::capability_source::{ComponentCapability, InternalCapability},
    cm_rust::{self, OfferDeclCommon, SourceName, UseDeclCommon},
    cm_types::Name,
    sandbox::{Dict, Receiver},
    std::collections::HashMap,
    tracing::{debug, warn},
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
    pub dict_from_parent: Dict,
    pub program_dict: Dict,
    /// Initial dicts for children and collections
    pub child_dicts: HashMap<Name, Dict>,
    pub collection_dicts: HashMap<Name, Dict>,
    pub sources_and_receivers: Vec<(CapabilitySourceFactory, Receiver<Message>)>,
}

impl ComponentSandbox {
    fn new(dict_from_parent: Dict) -> Self {
        Self { dict_from_parent, ..Self::default() }
    }
}

/// Once a component has been resolved and its manifest becomes known, this function produces the
/// various dicts the component needs based on the contents of its manifest.
pub async fn build_component_sandbox(
    decl: &cm_rust::ComponentDecl,
    dict_from_parent: Dict,
) -> ComponentSandbox {
    let mut output = ComponentSandbox::new(dict_from_parent);

    for child in &decl.children {
        let child_name = Name::new(&child.name).unwrap();
        output.child_dicts.insert(child_name, Dict::new());
    }

    for collection in &decl.collections {
        output.collection_dicts.insert(collection.name.clone(), Dict::new());
    }

    // All declared capabilities must have a receiver, unless we are non-executable.
    if decl.program.is_some() {
        for capability in &decl.capabilities {
            // We only support protocol capabilities right now
            match &capability {
                cm_rust::CapabilityDecl::Protocol(_) => (),
                _ => continue,
            }
            output
                .program_dict
                .get_or_insert_protocol_mut(capability.name())
                .insert_receiver(Receiver::new());
        }
    }

    for use_ in &decl.uses {
        // We only support protocol capabilities right now
        match &use_ {
            cm_rust::UseDecl::Protocol(_) => (),
            _ => continue,
        }

        let source_name = use_.source_name();
        match use_.source() {
            cm_rust::UseSource::Parent => {
                if let Some(cap_dict) = output.dict_from_parent.get_protocol(source_name) {
                    if let Some(parent_router) = cap_dict.get_router() {
                        output
                            .program_dict
                            .get_or_insert_protocol_mut(source_name)
                            .insert_router(parent_router.clone());
                    }
                } else {
                    debug!("unable to use from parent, parent dict does not have {}", source_name);
                }
            }
            cm_rust::UseSource::Self_ => {
                if let Some(mut cap_dict) = output.program_dict.get_protocol_mut(source_name) {
                    if let Some(receiver) = cap_dict.get_receiver().map(|r| r.clone()) {
                        cap_dict.insert_router(new_terminating_router(receiver));
                    }
                }
            }
            cm_rust::UseSource::Framework => {
                let receiver = Receiver::new();
                output
                    .program_dict
                    .get_or_insert_protocol_mut(source_name)
                    .insert_router(new_terminating_router(receiver.clone()));
                let source_name = source_name.clone();
                output.sources_and_receivers.push((
                    CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                        capability: InternalCapability::Protocol(source_name.clone()),
                        component,
                    }),
                    receiver,
                ));
            }
            cm_rust::UseSource::Capability(_) => {
                let receiver = Receiver::new();
                output
                    .program_dict
                    .get_or_insert_protocol_mut(source_name)
                    .insert_router(new_terminating_router(receiver.clone()));
                let use_ = use_.clone();
                output.sources_and_receivers.push((
                    CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                        source_capability: ComponentCapability::Use(use_.clone()),
                        component,
                    }),
                    receiver,
                ));
            }
            _ => (), // unsupported
        }
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
                output.collection_dicts.entry(name.clone()).or_insert(Dict::new())
            }
            cm_rust::OfferTarget::Capability(_name) => {
                // TODO(fxbug.dev/301674053): Support dictionary routing.
                continue;
            }
        };
        extend_dict_with_offer(
            &output.dict_from_parent,
            &output.program_dict,
            offer,
            target_dict,
            &mut output.sources_and_receivers,
        );
    }

    output
}

/// Extends the given dict based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_dict`.
pub fn extend_dict_with_offers(
    dict_from_parent: &Dict,
    program_dict: &Dict,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_dict: &mut Dict,
) -> Vec<(CapabilitySourceFactory, Receiver<Message>)> {
    let mut sources_and_receivers = vec![];
    for offer in dynamic_offers {
        extend_dict_with_offer(
            dict_from_parent,
            program_dict,
            offer,
            target_dict,
            &mut sources_and_receivers,
        );
    }
    sources_and_receivers
}

fn extend_dict_with_offer(
    dict_from_parent: &Dict,
    program_dict: &Dict,
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
    if let Some(mut cap_dict) = target_dict.get_protocol_mut(target_name) {
        if cap_dict.get_router().is_some() {
            warn!(
                "duplicate sources for protocol {} in a dict, unable to populate dict entry",
                target_name
            );
            cap_dict.remove_router();
            return;
        }
    }
    match offer.source() {
        cm_rust::OfferSource::Parent => {
            if let Some(source_cap_dict) = dict_from_parent.get_protocol(source_name) {
                if let Some(parent_router) = source_cap_dict.get_router() {
                    target_dict
                        .get_or_insert_protocol_mut(target_name)
                        .insert_router(parent_router.clone().availability(*offer.availability()));
                }
            }
        }
        cm_rust::OfferSource::Self_ => {
            if let Some(receiver) = program_dict
                .get_protocol(source_name)
                .and_then(|c| c.get_receiver().map(|r| r.clone()))
            {
                insert_receiver_into_target_dict(
                    receiver,
                    target_dict,
                    target_name,
                    *offer.availability(),
                );
            }
        }
        cm_rust::OfferSource::Framework => {
            let receiver = Receiver::new();
            insert_receiver_into_target_dict(
                receiver.clone(),
                target_dict,
                target_name,
                *offer.availability(),
            );
            let source_name = source_name.clone();
            sources_and_receivers.push((
                CapabilitySourceFactory::new(move |component| CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name.clone()),
                    component,
                }),
                receiver,
            ));
        }
        cm_rust::OfferSource::Capability(_) => {
            let receiver = Receiver::new();
            insert_receiver_into_target_dict(
                receiver.clone(),
                target_dict,
                target_name,
                *offer.availability(),
            );
            let offer = offer.clone();
            sources_and_receivers.push((
                CapabilitySourceFactory::new(move |component| CapabilitySource::Capability {
                    source_capability: ComponentCapability::Offer(offer.clone()),
                    component,
                }),
                receiver,
            ));
        }
        _ => (), // unsupported
    }
}

fn insert_receiver_into_target_dict(
    receiver: Receiver<Message>,
    target_dict: &mut Dict,
    target_name: &Name,
    availability: cm_rust::Availability,
) {
    let terminating_router = new_terminating_router(receiver);
    let forwarding_router = terminating_router.availability(availability);
    target_dict.get_or_insert_protocol_mut(target_name).insert_router(forwarding_router);
}
