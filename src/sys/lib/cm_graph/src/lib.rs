// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use directed_graph::DirectedGraph;
use fidl_fuchsia_component_decl as fdecl;
use std::fmt;

#[cfg(fuchsia_api_level_at_least = "25")]
macro_rules! get_source_dictionary {
    ($decl:ident) => {
        $decl.source_dictionary.as_ref()
    };
}
#[cfg(fuchsia_api_level_less_than = "25")]
macro_rules! get_source_dictionary {
    ($decl:ident) => {
        None
    };
}

/// A node in the DependencyGraph. The first string describes the type of node and the second
/// string is the name of the node.
#[derive(Copy, Clone, Hash, Ord, Debug, PartialOrd, PartialEq, Eq)]
pub enum DependencyNode<'a> {
    Self_,
    Child(&'a str, Option<&'a str>),
    Collection(&'a str),
    Environment(&'a str),
    Capability(&'a str),
}

impl<'a> fmt::Display for DependencyNode<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencyNode::Self_ => write!(f, "self"),
            DependencyNode::Child(name, None) => write!(f, "child {}", name),
            DependencyNode::Child(name, Some(collection)) => {
                write!(f, "child {}:{}", collection, name)
            }
            DependencyNode::Collection(name) => write!(f, "collection {}", name),
            DependencyNode::Environment(name) => write!(f, "environment {}", name),
            DependencyNode::Capability(name) => write!(f, "capability {}", name),
        }
    }
}

fn ref_to_dependency_node<'a>(ref_: Option<&'a fdecl::Ref>) -> Option<DependencyNode<'a>> {
    match ref_? {
        fdecl::Ref::Self_(_) => Some(DependencyNode::Self_),
        fdecl::Ref::Child(fdecl::ChildRef { name, collection }) => {
            Some(DependencyNode::Child(name, collection.as_ref().map(|s| s.as_str())))
        }
        fdecl::Ref::Collection(fdecl::CollectionRef { name }) => {
            Some(DependencyNode::Collection(name))
        }
        fdecl::Ref::Capability(fdecl::CapabilityRef { name }) => {
            Some(DependencyNode::Capability(name))
        }
        fdecl::Ref::Framework(_)
        | fdecl::Ref::Parent(_)
        | fdecl::Ref::Debug(_)
        | fdecl::Ref::VoidType(_) => None,
        #[cfg(fuchsia_api_level_at_least = "HEAD")]
        fdecl::Ref::Environment(_) => None,
        _ => None,
    }
}

// Generates the edges of the graph that are from a components `uses`.
fn get_dependencies_from_uses<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
) {
    if let Some(uses) = decl.uses.as_ref() {
        for use_ in uses.iter() {
            #[allow(unused_variables)]
            let (dependency_type, source, source_name, dict) = match use_ {
                fdecl::Use::Service(u) => {
                    (u.dependency_type, &u.source, &u.source_name, get_source_dictionary!(u))
                }
                fdecl::Use::Protocol(u) => {
                    (u.dependency_type, &u.source, &u.source_name, get_source_dictionary!(u))
                }
                fdecl::Use::Directory(u) => {
                    (u.dependency_type, &u.source, &u.source_name, get_source_dictionary!(u))
                }
                fdecl::Use::EventStream(u) => (
                    Some(fdecl::DependencyType::Strong),
                    &u.source,
                    &u.source_name,
                    None::<&String>,
                ),
                #[cfg(fuchsia_api_level_at_least = "HEAD")]
                fdecl::Use::Runner(u) => (
                    Some(fdecl::DependencyType::Strong),
                    &u.source,
                    &u.source_name,
                    get_source_dictionary!(u),
                ),
                #[cfg(fuchsia_api_level_at_least = "HEAD")]
                fdecl::Use::Config(u) => (
                    Some(fdecl::DependencyType::Strong),
                    &u.source,
                    &u.source_name,
                    get_source_dictionary!(u),
                ),
                // Storage can only be used from parent, which we don't track.
                fdecl::Use::Storage(_) => continue,
                _ => continue,
            };
            if dependency_type != Some(fdecl::DependencyType::Strong) {
                continue;
            }

            let dependency_nodes = match &source {
                Some(fdecl::Ref::Child(fdecl::ChildRef { name, collection })) => {
                    vec![DependencyNode::Child(name, collection.as_ref().map(|s| s.as_str()))]
                }
                Some(fdecl::Ref::Self_(_)) => {
                    #[cfg(fuchsia_api_level_at_least = "25")]
                    if dict.as_ref().is_some() {
                        if let Some(source_name) = source_name.as_ref() {
                            vec![DependencyNode::Capability(source_name)]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }

                    #[cfg(fuchsia_api_level_less_than = "25")]
                    vec![]
                }
                Some(fdecl::Ref::Collection(fdecl::CollectionRef { name })) => {
                    let mut nodes = vec![];
                    if let Some(children) = decl.children.as_ref() {
                        for child in children {
                            if let Some(child_name) = child.name.as_ref() {
                                nodes.push(DependencyNode::Child(child_name, Some(name)));
                            }
                        }
                    }
                    nodes
                }
                _ => vec![],
            };

            for source_node in dependency_nodes {
                strong_dependencies.add_edge(source_node, DependencyNode::Self_);
            }
        }
    }
}

fn get_dependencies_from_capabilities<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
) {
    if let Some(capabilities) = decl.capabilities.as_ref() {
        for cap in capabilities {
            match cap {
                #[cfg(fuchsia_api_level_at_least = "25")]
                fdecl::Capability::Dictionary(dictionary) => {
                    if dictionary.source_path.as_ref().is_some() {
                        if let Some(name) = dictionary.name.as_ref() {
                            // If `source_path` is set that means the dictionary is provided by the program,
                            // which implies a dependency from `self` to the dictionary declaration.
                            strong_dependencies
                                .add_edge(DependencyNode::Self_, DependencyNode::Capability(name));
                        }
                    }
                }
                fdecl::Capability::Storage(storage) => {
                    if let (Some(name), Some(_backing_dir)) =
                        (storage.name.as_ref(), storage.backing_dir.as_ref())
                    {
                        if let Some(source_node) = ref_to_dependency_node(storage.source.as_ref()) {
                            strong_dependencies
                                .add_edge(source_node, DependencyNode::Capability(name));
                        }
                    }
                }
                _ => continue,
            }
        }
    }
}

fn get_dependencies_from_environments<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
) {
    if let Some(environment) = decl.environments.as_ref() {
        for environment in environment {
            if let Some(name) = &environment.name {
                let target = DependencyNode::Environment(name);
                if let Some(debugs) = environment.debug_capabilities.as_ref() {
                    for debug in debugs {
                        if let fdecl::DebugRegistration::Protocol(o) = debug {
                            if let Some(source_node) = ref_to_dependency_node(o.source.as_ref()) {
                                strong_dependencies.add_edge(source_node, target);
                            }
                        }
                    }
                }
                if let Some(runners) = environment.runners.as_ref() {
                    for runner in runners {
                        if let Some(source_node) = ref_to_dependency_node(runner.source.as_ref()) {
                            strong_dependencies.add_edge(source_node, target);
                        }
                    }
                }
                if let Some(resolvers) = environment.resolvers.as_ref() {
                    for resolver in resolvers {
                        if let Some(source_node) = ref_to_dependency_node(resolver.source.as_ref())
                        {
                            strong_dependencies.add_edge(source_node, target);
                        }
                    }
                }
            }
        }
    }
}

fn get_dependencies_from_children<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
) {
    if let Some(children) = decl.children.as_ref() {
        for child in children {
            if let Some(name) = child.name.as_ref() {
                if let Some(env) = child.environment.as_ref() {
                    let source = DependencyNode::Environment(env.as_str());
                    let target = DependencyNode::Child(name, None);
                    strong_dependencies.add_edge(source, target);
                }
            }
        }
    }
}

fn get_dependencies_from_collections<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
) {
    if let Some(collections) = decl.collections.as_ref() {
        for collection in collections {
            if let Some(env) = collection.environment.as_ref() {
                if let Some(name) = collection.name.as_ref() {
                    let source = DependencyNode::Environment(env.as_str());
                    let target = DependencyNode::Collection(name.as_str());
                    strong_dependencies.add_edge(source, target);
                }
            }
        }
    }
}

fn find_offer_node<'a>(
    offer: &'a fdecl::Offer,
    source: Option<&'a fdecl::Ref>,
    source_name: &'a Option<String>,
    _dictionary: Option<&'a String>,
) -> Option<DependencyNode<'a>> {
    if source.is_none() {
        return None;
    }

    match source? {
        fdecl::Ref::Child(fdecl::ChildRef { name, collection }) => {
            Some(DependencyNode::Child(name, collection.as_ref().map(|s| s.as_str())))
        }
        #[cfg(fuchsia_api_level_at_least = "25")]
        fdecl::Ref::Self_(_) if _dictionary.is_some() => {
            let root_dict = _dictionary.unwrap().split('/').next().unwrap();
            return Some(DependencyNode::Capability(root_dict));
        }
        fdecl::Ref::Self_(_) => {
            if let Some(source_name) = source_name {
                #[cfg(fuchsia_api_level_at_least = "25")]
                if matches!(offer, fdecl::Offer::Dictionary(_)) {
                    return Some(DependencyNode::Capability(source_name));
                }
                if matches!(offer, fdecl::Offer::Storage(_)) {
                    return Some(DependencyNode::Capability(source_name));
                }
            }

            Some(DependencyNode::Self_)
        }
        fdecl::Ref::Collection(fdecl::CollectionRef { name }) => {
            Some(DependencyNode::Collection(name))
        }
        fdecl::Ref::Capability(fdecl::CapabilityRef { name }) => {
            Some(DependencyNode::Capability(name))
        }
        fdecl::Ref::Parent(_) | fdecl::Ref::Framework(_) | fdecl::Ref::VoidType(_) => None,
        _ => None,
    }
}

fn dynamic_children_in_collection<'a>(
    dynamic_children: &Vec<(&'a str, &'a str)>,
    collection: &'a str,
) -> Vec<&'a str> {
    dynamic_children
        .iter()
        .filter_map(|(n, c)| if *c == collection { Some(*n) } else { None })
        .collect()
}

fn add_offer_edges<'a>(
    source_node: Option<DependencyNode<'a>>,
    target_node: Option<DependencyNode<'a>>,
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    dynamic_children: &Vec<(&'a str, &'a str)>,
) {
    if source_node.is_none() {
        return;
    }

    let source = source_node.unwrap();

    if let DependencyNode::Collection(name) = source {
        for child_name in dynamic_children_in_collection(dynamic_children, &name) {
            strong_dependencies.add_edge(
                DependencyNode::Child(&child_name, Some(&name)),
                DependencyNode::Collection(name),
            );
        }
    }

    if target_node.is_none() {
        return;
    }

    let target = target_node.unwrap();

    strong_dependencies.add_edge(source, target);

    if let DependencyNode::Collection(name) = target {
        for child_name in dynamic_children_in_collection(dynamic_children, &name) {
            strong_dependencies.add_edge(source, DependencyNode::Child(child_name, Some(&name)));
        }
    }
}

// Populates a dependency graph of a component's `offers`.
fn get_dependencies_from_offers<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
    dynamic_children: &Vec<(&'a str, &'a str)>,
    dynamic_offers: Option<&'a Vec<fdecl::Offer>>,
) {
    let mut all_offers: Vec<&fdecl::Offer> = vec![];

    if let Some(dynamic_offers) = dynamic_offers.as_ref() {
        for dynamic_offer in dynamic_offers.iter() {
            all_offers.push(dynamic_offer);
        }
    }

    if let Some(offers) = decl.offers.as_ref() {
        for offer in offers.iter() {
            all_offers.push(offer);
        }
    }

    for offer in all_offers {
        let (source_node, target_node) = match offer {
            fdecl::Offer::Protocol(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );

                if let Some(fdecl::DependencyType::Strong) = o.dependency_type.as_ref() {
                    let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                    (source_node, target_node)
                } else {
                    continue;
                }
            }
            #[cfg(fuchsia_api_level_at_least = "25")]
            fdecl::Offer::Dictionary(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );

                if let Some(fdecl::DependencyType::Strong) = o.dependency_type.as_ref() {
                    let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                    (source_node, target_node)
                } else {
                    continue;
                }
            }
            fdecl::Offer::Directory(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );
                if let Some(fdecl::DependencyType::Strong) = o.dependency_type.as_ref() {
                    let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                    (source_node, target_node)
                } else {
                    continue;
                }
            }
            fdecl::Offer::Service(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );

                #[cfg(fuchsia_api_level_at_least = "HEAD")]
                {
                    if &fdecl::DependencyType::Strong
                        == o.dependency_type.as_ref().unwrap_or(&fdecl::DependencyType::Strong)
                    {
                        let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                        (source_node, target_node)
                    } else {
                        continue;
                    }
                }

                #[cfg(fuchsia_api_level_less_than = "HEAD")]
                {
                    let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                    (source_node, target_node)
                }
            }
            fdecl::Offer::Storage(o) => {
                let source_node = find_offer_node(offer, o.source.as_ref(), &o.source_name, None);

                let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                (source_node, target_node)
            }
            fdecl::Offer::Runner(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );

                let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                (source_node, target_node)
            }
            fdecl::Offer::Resolver(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );

                let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                (source_node, target_node)
            }
            fdecl::Offer::Config(o) => {
                let source_node = find_offer_node(
                    offer,
                    o.source.as_ref(),
                    &o.source_name,
                    get_source_dictionary!(o),
                );

                let target_node = find_offer_node(offer, o.target.as_ref(), &None, None);

                (source_node, target_node)
            }
            _ => continue,
        };

        add_offer_edges(source_node, target_node, strong_dependencies, dynamic_children);
    }
}

// Populates a dependency graph of the disjoint sets of graphs.
pub fn generate_dependency_graph<'a>(
    strong_dependencies: &mut DirectedGraph<DependencyNode<'a>>,
    decl: &'a fdecl::Component,
    dynamic_children: &Vec<(&'a str, &'a str)>,
    dynamic_offers: Option<&'a Vec<fdecl::Offer>>,
) {
    get_dependencies_from_uses(strong_dependencies, decl);
    get_dependencies_from_offers(strong_dependencies, decl, dynamic_children, dynamic_offers);
    get_dependencies_from_capabilities(strong_dependencies, decl);
    get_dependencies_from_environments(strong_dependencies, decl);
    get_dependencies_from_children(strong_dependencies, decl);
    get_dependencies_from_collections(strong_dependencies, decl);
}
