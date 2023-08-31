// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::sandbox_util::Sandbox,
    cm_rust::{self, OfferDeclCommon, SourceName, UseDeclCommon},
    cm_types::Name,
    std::collections::HashMap,
    tracing::debug,
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
        };
        extend_dict_with_offer(&output.sandbox_from_parent, offer, target_sandbox);
    }

    output
}

/// Extends the given sandbox based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_sandbox`.
pub fn extend_dict_with_offers(
    sandbox_from_parent: &Sandbox,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_sandbox: &mut Sandbox,
) {
    for offer in dynamic_offers {
        extend_dict_with_offer(sandbox_from_parent, offer, target_sandbox);
    }
}

fn extend_dict_with_offer(
    sandbox_from_parent: &Sandbox,
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
    match offer.source() {
        cm_rust::OfferSource::Parent => {
            if let Some(cap_sandbox) = sandbox_from_parent.get_protocol(source_name) {
                if let Some(sender) = cap_sandbox.get_sender() {
                    target_sandbox
                        .get_or_insert_protocol(target_name.clone())
                        .insert_sender(sender.clone());
                }
            }
        }
        cm_rust::OfferSource::Void => {
            // Intentionally do nothing, because we've been explicitly instructed to NOT grant
            // access to the target.
        }
        _ => (), // unsupported
    }
}
