// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::component::WeakComponentInstance,
        sandbox_util::{DictExt, Message},
    },
    ::routing::capability_source::{ComponentCapability, InternalCapability},
    cm_rust::{self, Availability, OfferDeclCommon, SourceName, UseDeclCommon},
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
                    if let Some(sender) = cap_dict.get_sender() {
                        output
                            .program_dict
                            .get_or_insert_protocol_mut(source_name)
                            .insert_sender(sender.clone());
                    }
                } else {
                    debug!("unable to use from parent, parent dict does not have {}", source_name);
                }
            }
            cm_rust::UseSource::Self_ => {
                if let Some(mut cap_dict) = output.program_dict.get_protocol_mut(source_name) {
                    if let Some(sender) = cap_dict.get_receiver().map(|r| r.new_sender()) {
                        cap_dict.insert_sender(sender)
                    }
                }
            }
            cm_rust::UseSource::Framework => {
                let receiver = Receiver::new();
                output
                    .program_dict
                    .get_or_insert_protocol_mut(source_name)
                    .insert_sender(receiver.new_sender());
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
                    .insert_sender(receiver.new_sender());
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
        if cap_dict.get_sender().is_some() {
            warn!(
                "duplicate sources for protocol {} in a dict, unable to populate dict entry",
                target_name
            );
            cap_dict.remove_sender();
            return;
        }
    }
    match offer.source() {
        cm_rust::OfferSource::Parent => {
            if let Some(source_cap_dict) = dict_from_parent.get_protocol(source_name) {
                if let Some(sender) = source_cap_dict.get_sender() {
                    let old_availability = source_cap_dict
                        .get_availability()
                        .expect("protocol dictionary is missing availability");
                    let new_availability = offer.availability();
                    if let Some(new_availability) =
                        get_next_availability(*old_availability, *new_availability)
                    {
                        let mut target_cap_dict =
                            target_dict.get_or_insert_protocol_mut(target_name);
                        target_cap_dict.insert_sender(sender.clone());
                        target_cap_dict.insert_availability(new_availability);
                    }
                }
            }
        }
        cm_rust::OfferSource::Self_ => {
            if let Some(sender) = program_dict
                .get_protocol(source_name)
                .and_then(|c| c.get_receiver().map(|r| r.new_sender()))
            {
                let mut target_cap_dict = target_dict.get_or_insert_protocol_mut(target_name);
                target_cap_dict.insert_sender(sender);
                target_cap_dict.insert_availability(offer.availability().clone());
            }
        }
        cm_rust::OfferSource::Framework => {
            let receiver = Receiver::new();
            let mut target_cap_dict = target_dict.get_or_insert_protocol_mut(target_name);
            target_cap_dict.insert_sender(receiver.new_sender());
            target_cap_dict.insert_availability(offer.availability().clone());
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
            let mut target_cap_dict = target_dict.get_or_insert_protocol_mut(target_name);
            target_cap_dict.insert_sender(receiver.new_sender());
            target_cap_dict.insert_availability(offer.availability().clone());
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

fn get_next_availability(
    source: cm_rust::Availability,
    target: cm_rust::Availability,
) -> Option<cm_rust::Availability> {
    match (source, target) {
        // This is only possible if the uppermost offer in a route chain is set to `SameAsTarget`,
        // as then `SameAsTarget` will be set in the target dict as we step down the tree until
        // we encounter a concrete availability.
        (Availability::SameAsTarget, _) => Some(target),

        // If our availability doesn't change, there's nothing to do.
        (Availability::Required, Availability::Required)
        | (Availability::Optional, Availability::Optional)
        | (Availability::Transitional, Availability::Transitional) => Some(target),

        // If the next availability is explicitly a pass-through, let's mark the availability the
        // same as the source.
        (Availability::Required, Availability::SameAsTarget)
        | (Availability::Optional, Availability::SameAsTarget)
        | (Availability::Transitional, Availability::SameAsTarget) => Some(source),

        // Decreasing the strength of availability as we travel toward the target is allowed.
        (Availability::Required, Availability::Optional)
        | (Availability::Required, Availability::Transitional)
        | (Availability::Optional, Availability::Transitional) => Some(target),

        // Increasing the strength of availability as we travel toward the target is not allowed,
        // as that could lead to unsanctioned broken routes.
        (Availability::Transitional, Availability::Optional)
        | (Availability::Transitional, Availability::Required)
        | (Availability::Optional, Availability::Required) => {
            warn!("not populating dict with capability because of invalid availability settings");
            None
        }
    }
}
