// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::sandbox_util::Sandbox,
    cm_rust::{self, Availability, OfferDeclCommon, SourceName, UseDeclCommon},
    cm_types::Name,
    sandbox::Receiver,
    std::collections::HashMap,
    tracing::{debug, warn},
};

/// The sandboxes a component holds once it has been resolved.
#[derive(Debug, Default)]
pub struct ComponentSandboxes {
    pub sandbox_from_parent: Sandbox,
    pub program_sandbox: Sandbox,
    /// Initial sandboxes for children and collections
    pub child_sandboxes: HashMap<Name, Sandbox>,
    pub collection_sandboxes: HashMap<Name, Sandbox>,
}

impl ComponentSandboxes {
    fn new(sandbox_from_parent: Sandbox) -> Self {
        Self {
            sandbox_from_parent,
            program_sandbox: Sandbox::new(),
            child_sandboxes: HashMap::new(),
            collection_sandboxes: HashMap::new(),
        }
    }
}

/// Once a component has been resolved and its manifest becomes known, this function produces the
/// various sandboxes the component needs based on the contents of its manifest.
pub async fn build_component_sandboxes(
    decl: &cm_rust::ComponentDecl,
    sandbox_from_parent: Sandbox,
) -> ComponentSandboxes {
    let mut output = ComponentSandboxes::new(sandbox_from_parent);

    for child in &decl.children {
        let child_name = Name::new(&child.name).unwrap();
        output.child_sandboxes.insert(child_name, Sandbox::new());
    }

    for collection in &decl.collections {
        output.collection_sandboxes.insert(collection.name.clone(), Sandbox::new());
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
                .program_sandbox
                .get_or_insert_protocol(capability.name().clone())
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
                if let Some(cap_sandbox) = output.sandbox_from_parent.get_protocol(source_name) {
                    if let Some(sender) = cap_sandbox.get_sender() {
                        output
                            .program_sandbox
                            .get_or_insert_protocol(source_name.clone())
                            .insert_sender(sender.clone());
                    }
                } else {
                    debug!(
                        "unable to use from parent, parent sandbox does not have {}",
                        source_name
                    );
                }
            }
            cm_rust::UseSource::Self_ => {
                if let Some(mut cap_sandbox) = output.program_sandbox.get_protocol_mut(source_name)
                {
                    if let Some(sender) = cap_sandbox.get_receiver().map(|r| r.new_sender()) {
                        cap_sandbox.insert_sender(sender)
                    }
                }
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
        let target_sandbox = match offer.target() {
            cm_rust::OfferTarget::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name = Name::new(&child_ref.name).unwrap();
                let dict = output.child_sandboxes.entry(child_name).or_insert(Sandbox::new());
                dict
            }
            cm_rust::OfferTarget::Collection(name) => {
                let dict =
                    output.collection_sandboxes.entry(name.clone()).or_insert(Sandbox::new());
                dict
            }
            cm_rust::OfferTarget::Capability(_name) => {
                // TODO(fxbug.dev/301674053): Support dictionary routing.
                continue;
            }
        };
        extend_dict_with_offer(
            &output.sandbox_from_parent,
            &output.program_sandbox,
            offer,
            target_sandbox,
        );
    }

    output
}

/// Extends the given sandbox based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_sandbox`.
pub fn extend_dict_with_offers(
    sandbox_from_parent: &Sandbox,
    program_sandbox: &Sandbox,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_sandbox: &mut Sandbox,
) {
    for offer in dynamic_offers {
        extend_dict_with_offer(sandbox_from_parent, program_sandbox, offer, target_sandbox);
    }
}

fn extend_dict_with_offer(
    sandbox_from_parent: &Sandbox,
    program_sandbox: &Sandbox,
    offer: &cm_rust::OfferDecl,
    target_sandbox: &mut Sandbox,
) {
    // We only support protocol capabilities right now
    match &offer {
        cm_rust::OfferDecl::Protocol(_) => (),
        _ => return,
    }
    let source_name = offer.source_name();
    let target_name = offer.target_name();
    if let Some(mut cap_sandbox) = target_sandbox.get_protocol_mut(target_name) {
        if cap_sandbox.get_sender().is_some() {
            warn!(
                "duplicate sources for protocol {} in a sandbox, unable to populate sandbox entry",
                target_name
            );
            cap_sandbox.remove_sender();
            return;
        }
    }
    match offer.source() {
        cm_rust::OfferSource::Parent => {
            if let Some(source_cap_sandbox) = sandbox_from_parent.get_protocol(source_name) {
                if let Some(sender) = source_cap_sandbox.get_sender() {
                    let old_availability = source_cap_sandbox
                        .get_availability()
                        .expect("protocol dictionary is missing availability");
                    let new_availability = offer
                        .availability()
                        .expect("availability should always be set for protocols");
                    if let Some(new_availability) =
                        get_next_availability(*old_availability, *new_availability)
                    {
                        let mut target_cap_sandbox =
                            target_sandbox.get_or_insert_protocol(target_name.clone());
                        target_cap_sandbox.insert_sender(sender.clone());
                        target_cap_sandbox.insert_availability(new_availability);
                    }
                }
            }
        }
        cm_rust::OfferSource::Self_ => {
            if let Some(sender) = program_sandbox
                .get_protocol(source_name)
                .and_then(|c| c.get_receiver().map(|r| r.new_sender()))
            {
                let mut target_cap_sandbox =
                    target_sandbox.get_or_insert_protocol(source_name.clone());
                target_cap_sandbox.insert_sender(sender);
                target_cap_sandbox.insert_availability(
                    offer
                        .availability()
                        .expect("availability should always be set for protocols")
                        .clone(),
                );
            }
        }
        cm_rust::OfferSource::Void => {
            // Intentionally do nothing, because we've been explicitly instructed to NOT grant
            // access to the target.

            // TODO: We want to signify to the child that we've intentionally not given it the
            // capability. Setting something in the sandbox to this effect makes sense. What
            // exactly though?
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
        // as then `SameAsTarget` will be set in the target sandbox as we step down the tree until
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
            warn!(
                "not populating sandbox with capability because of invalid availability settings"
            );
            None
        }
    }
}
