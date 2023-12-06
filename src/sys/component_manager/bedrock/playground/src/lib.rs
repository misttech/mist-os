// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use futures::{lock::Mutex, FutureExt};
use sandbox::{AnyCapability, Data, Dict, Lazy, Optional};
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use thiserror::Error;

mod decl;
mod fidl;
mod route2;

struct InterfaceInner {
    /// Capabilities offered to this interface, e.g. from the parent component.
    incoming: Dict,
    /// Capabilities made available outside of this interface, e.g. to the parent component.
    outgoing: Dict,
}

/// A container for capabilities, generalizing the component capability boundary.
///
/// Interfaces have an incoming and outgoing dictionary. These dictionaries contain all
/// capabilities going to and from the interface. There is no way to route capabilities through the
/// interface other than through these two dictionaries, to make analysis as obvious as possible.
///
/// Interfaces are dynamic and don't know anything about the static topology.
///
/// # Interfaces vs components
///
/// Components have a single "outer" interface that is the capability boundary between the component
/// and its parent.
///
///     * The incoming dictionary contains all capabilities routed from the parent,
///       i.e. from `use` and `offer` from parent declarations.
///     * The outgoing dictionary contains all capabilities routed to the parent,
///       i.e. from `expose` declarations.
///
/// Components may have a program, which has its own interface. A program's incoming dictionary
/// contains capabilities `use`d by the component, and its outgoing dictionary contains
/// capabilities routed from `self`. (This is a simplification because components can define
/// derived capabilities like storage that are not directly from the program)
///
/// Components may have children. These children each have their own interface.
///
/// # Routing
///
/// Routing is a capability transfer between two interfaces:
///
///     * `offer from parent to child` = Parent incoming   -> Child incoming
///     * `offer from child to child`  = Child outgoing    -> Child incoming
///     * `offer from self to child`   = Program outgoing  -> Child incoming
///     * `expose from child`          = Child outgoing    -> Parent outgoing
///     * `expose from self`           = Program outgoing  -> Parent outgoing
///     * `use from parent`            = Parent incoming   -> Program incoming
///     * `use from child`             = Child outgoing    -> Program incoming
///     * `use from self`              = Program outgoing  -> Program incoming
pub struct Interface {
    inner: Mutex<InterfaceInner>,
}

impl Interface {
    pub fn new() -> Self {
        let inner = InterfaceInner { incoming: Dict::new(), outgoing: Dict::new() };
        Self { inner: Mutex::new(inner) }
    }

    /// Put the capability into the incoming dictionary.
    pub async fn insert(&self, name: decl::CapabilityName, cap: AnyCapability) {
        let inner = self.inner.lock().await;
        inner.incoming.lock_entries().insert(name, cap);
    }

    /// Take the capability out of the outgoing dictionary.
    pub async fn remove(&self, name: &decl::CapabilityName) -> Option<AnyCapability> {
        let inner = self.inner.lock().await;
        let entry = inner.outgoing.lock_entries().remove(name);
        entry
    }

    /// Get a clone of the capability from the outgoing dictionary.
    pub async fn get(&self, name: &decl::CapabilityName) -> Option<AnyCapability> {
        let inner = self.inner.lock().await;
        let entry = inner.outgoing.lock_entries().get(name).cloned();
        entry
    }

    /// Get a clone of the capability from the incoming dictionary.
    /// Should only be used by the interface owner.
    async fn get_incoming(&self, name: &decl::CapabilityName) -> Option<AnyCapability> {
        let inner = self.inner.lock().await;
        let entry = inner.incoming.lock_entries().get(name).cloned();
        entry
    }

    /// Put the capability into the outgoing dictionary.
    /// Should only be used by the interface owner.
    async fn insert_outgoing(&self, name: decl::CapabilityName, cap: AnyCapability) {
        let inner = self.inner.lock().await;
        inner.outgoing.lock_entries().insert(name, cap);
    }
}

/// A node in the static topology.
pub enum Node {
    /// Placeholder for a component, representing an unresolved child.
    Unresolved(Unresolved),
    /// Represents a resolved component with capabilities routed according to its decl.
    Resolved(Resolved),
}

pub struct Unresolved {
    interface: Arc<Interface>,
}

impl Unresolved {
    fn new() -> Self {
        Unresolved { interface: Arc::new(Interface::new()) }
    }
}

pub struct Resolved {
    _interface: Arc<Interface>,
    program_interface: Arc<Interface>,
    children: HashMap<decl::ComponentName, Node>,
}

impl Resolved {
    async fn resolve_child(
        &mut self,
        child_name: decl::ComponentName,
        child_decl: decl::Component,
    ) -> Result<(), ResolveError> {
        let child = self.children.remove(&child_name).unwrap();
        let unresolved = match child {
            Node::Unresolved(unresolved) => unresolved,
            Node::Resolved(_) => panic!(),
        };
        let resolved = resolve(unresolved, child_decl).await?;
        self.children.insert(child_name, Node::Resolved(resolved));
        Ok(())
    }
}

#[derive(Error, Debug)]
pub enum ResolveError {
    #[error("could not get route source")]
    SourceError(#[from] SourceError),
    #[error("could not get route target")]
    TargetError(#[from] TargetError),
    #[error("invalid availability in route")]
    AvailabilityError(#[from] AvailabilityError),
}

#[derive(Error, Debug)]
pub enum SourceError {
    #[error("parent did not provide capability in the interface")]
    MissingFromIncoming,
    #[error("child {0} does not exist")]
    ChildDoesNotExist(decl::ComponentName),
}

#[derive(Error, Debug)]
pub enum TargetError {
    #[error("child {0} does not exist")]
    ChildDoesNotExist(decl::ComponentName),
}

pub async fn resolve(
    unresolved: Unresolved,
    decl: decl::Component,
) -> Result<Resolved, ResolveError> {
    let interface = unresolved.interface;

    // Contains capabilities used by the program and exposed from self.
    let program_interface = Interface::new();

    // Interfaces for each child declared by the component.
    let child_interfaces: HashMap<decl::ComponentName, Arc<Interface>> = decl
        .children
        .iter()
        .map(|child| (child.name.clone(), Arc::new(Interface::new())))
        .collect();

    // Returns a capability, `cap_name`, routed from `source`.
    async fn route_from(
        cap_name: &decl::CapabilityName,
        source: &decl::Ref,
        interface: &Interface,
        child_interfaces: &HashMap<decl::ComponentName, Arc<Interface>>,
    ) -> Result<AnyCapability, SourceError> {
        Ok(match source {
            decl::Ref::Hammerspace => {
                // Create the capability out of thin air.
                Box::new(Data::String(cap_name.clone())) as AnyCapability
            }
            decl::Ref::Parent => {
                // Get the capability from the incoming dictionary of the current interface.
                // The parent should have offered the capability to the unresolved node.
                interface.get_incoming(&cap_name).await.ok_or(SourceError::MissingFromIncoming)?
            }
            decl::Ref::Child(child_name) => {
                // Get a weak reference to the child.
                // If the child (and its interface) are later destroyed, then this use will fail.
                let child_interface = Arc::downgrade(
                    child_interfaces
                        .get(child_name)
                        .ok_or_else(|| SourceError::ChildDoesNotExist(child_name.clone()))?,
                );
                // Create a capability that will lazily route from the child's interface.
                // We can't get the capability out of the interface at this point because the child
                // is unresolved, so it doesn't expose anything and its outgoing dictionary is empty.
                Box::new(lazy_from_interface(child_interface, cap_name.clone()))
            }
        })
    }

    // Returns a capability, `cap_name`, routed from `source`, transformed with `availability`
    async fn route_from_with_availability(
        cap_name: &decl::CapabilityName,
        source: &decl::Ref,
        interface: &Interface,
        child_interfaces: &HashMap<decl::ComponentName, Arc<Interface>>,
        availability: &decl::Availability,
    ) -> Result<AnyCapability, ResolveError> {
        // Try to get the capability from the source. This may fail.
        let source_result = route_from(cap_name, source, interface, child_interfaces).await;
        // Transitional availability turns "broken" routes into void.
        let cap = match availability {
            decl::Availability::Transitional => {
                source_result.unwrap_or_else(|_| Box::new(Optional::void()) as AnyCapability)
            }
            _ => source_result?,
        };
        // Transform the capability to/from an Optional, according to availability.
        let cap = transform_availability(cap, availability)?;
        Ok(cap)
    }

    for use_ in decl.uses {
        let cap = route_from_with_availability(
            &use_.name,
            &use_.from,
            &interface,
            &child_interfaces,
            &use_.availability,
        )
        .await?;
        // Insert into the program interface under the same name.
        program_interface.insert(use_.name, cap).await;
    }

    for offer in decl.offers {
        let cap = route_from_with_availability(
            &offer.name,
            &offer.from,
            &interface,
            &child_interfaces,
            &offer.availability,
        )
        .await?;
        // Determine the target interface and insert the capability into it.
        let target_interface: &Interface = match offer.to {
            decl::Ref::Hammerspace => unimplemented!(),
            decl::Ref::Parent => todo!(),
            decl::Ref::Child(child_name) => child_interfaces
                .get(&child_name)
                .ok_or_else(|| TargetError::ChildDoesNotExist(child_name.clone()))?,
        };
        target_interface.insert(offer.name.clone(), cap).await;
    }

    for expose in decl.exposes {
        let cap = route_from_with_availability(
            &expose.name,
            &expose.from,
            &interface,
            &child_interfaces,
            &expose.availability,
        )
        .await?;
        // Insert into the outgoing dictionary of the interface under the same name.
        interface.insert_outgoing(expose.name.clone(), cap).await;
    }

    // All children are unresolved nodes at first.
    let children = child_interfaces
        .into_iter()
        .map(|(name, child_interface)| {
            (name, Node::Unresolved(Unresolved { interface: child_interface }))
        })
        .collect();

    Ok(Resolved { _interface: interface, program_interface: Arc::new(program_interface), children })
}

/// Returns a Lazy capability that resolves to a capability from the interface with the given name.
fn lazy_from_interface(interface: Weak<Interface>, name: decl::CapabilityName) -> Lazy {
    Lazy::new(move || {
        let interface = interface.clone();
        let name = name.clone();
        async move {
            let interface =
                interface.upgrade().ok_or_else(|| anyhow!("interface no longer exists"))?;
            interface.get(&name).await.context("capability does not exist")
        }
        .boxed()
    })
}

#[derive(Error, Debug)]
pub enum AvailabilityError {
    #[error("cannot route Optional value with required availability")]
    OptionalToRequired,
}

/// Transform the capability according to the given availability, for an incoming dictionary.
///
/// * Optional and Transitional converts cap to an Optional(Some(cap)).
/// * Required ensures that the capability is not an Optional, then returns as-is.
/// * SameAsTarget returns the capability through as-is.
fn transform_availability(
    cap: AnyCapability,
    availability: &decl::Availability,
) -> Result<AnyCapability, AvailabilityError> {
    fn transform(
        cap: AnyCapability,
        availability: &decl::Availability,
    ) -> Result<AnyCapability, AvailabilityError> {
        match availability {
            decl::Availability::Optional | decl::Availability::Transitional => {
                // Optional means the target expects an Optional, so convert `cap` to Optional if
                // it isn't one already.
                //
                // Transitional means the target doesn't care whether the source is an Optional
                // or not. Since that means it can accept an Optional, convert to that.
                Ok(Box::new(Optional::from_any(cap)))
            }
            decl::Availability::Required => {
                if cap.as_any().is::<Optional>() {
                    // Required is stronger than optional.
                    return Err(AvailabilityError::OptionalToRequired);
                }
                Ok(cap)
            }
            decl::Availability::SameAsTarget => Ok(cap),
        }
    }
    // For Lazy values, do a little dance to wrap with another Lazy that transforms the
    // underlying value. If we don't do this, intermediate transformations don't know whether
    // the capability is optional or not because it's routed as a Lazy, not an Optional.
    if cap.as_any().is::<Lazy>() {
        let lazy: Lazy = cap.try_into().unwrap();
        let availability = availability.to_owned();
        let lazy_transformed = Box::new(lazy.map(move |cap| {
            async move { transform(cap, &availability).map_err(Error::from) }.boxed()
        }));
        Ok(lazy_transformed)
    } else {
        transform(cap, availability)
    }
}

#[cfg(test)]
mod test {
    use crate::decl;
    use crate::{resolve, Lazy, Node, Optional, Unresolved};
    use sandbox::{AnyCapability, Data};

    #[fuchsia::test]
    async fn test_topology() {
        // Set up the topology:
        //           root
        //          /    \
        //     child_a   child_b
        //

        // child_a exposes capability "cap"
        let child_a_decl = decl::Component {
            uses: Vec::new(),
            offers: Vec::new(),
            exposes: vec![decl::Expose {
                name: "cap".to_string(),
                from: decl::Ref::Hammerspace,
                availability: decl::Availability::Required,
            }],
            children: Vec::new(),
        };

        // child_b uses "cap" from parent
        let child_b_decl = decl::Component {
            uses: vec![decl::Use {
                name: "cap".to_string(),
                from: decl::Ref::Parent,
                availability: decl::Availability::Required,
            }],
            offers: Vec::new(),
            exposes: Vec::new(),
            children: Vec::new(),
        };

        // root offers "cap" from child_a to child_b
        let root_decl = decl::Component {
            uses: Vec::new(),
            offers: vec![decl::Offer {
                name: "cap".to_string(),
                from: decl::Ref::Child("child_a".to_string()),
                to: decl::Ref::Child("child_b".to_string()),
                availability: decl::Availability::SameAsTarget,
            }],
            exposes: Vec::new(),
            children: vec![
                decl::Child { name: "child_a".to_string() },
                decl::Child { name: "child_b".to_string() },
            ],
        };

        // Resolve the root node
        let root_unresolved = Unresolved::new();
        let mut root = resolve(root_unresolved, root_decl).await.unwrap();

        // Resolve child_a
        root.resolve_child("child_a".to_string(), child_a_decl).await.unwrap();

        // Resolve child_b
        root.resolve_child("child_b".to_string(), child_b_decl).await.unwrap();

        // child_b should be able to use the capability from child_a
        let child_b = match root.children.get("child_b").unwrap() {
            Node::Unresolved(_) => panic!(),
            Node::Resolved(resolved) => resolved,
        };
        let lazy: Lazy = child_b
            .program_interface
            .get_incoming(&"cap".to_string())
            .await
            .expect("child_b doesn't have the capability in its program interface")
            .try_into()
            .expect("failed to convert to Lazy");
        let cap: Data = lazy.get().await.unwrap().try_into().expect("failed to convert to Data");
        assert_eq!(cap, Data::String("cap".to_string()));
    }

    // Tests that a capability exposed with optional availability can be used with
    // optional availability.
    #[fuchsia::test]
    async fn test_optional_use_optional() {
        // Set up the topology:
        //           root
        //          /    \
        //     child_a   child_b
        //

        // child_a exposes optional capability "cap"
        let child_a_decl = decl::Component {
            uses: Vec::new(),
            offers: Vec::new(),
            exposes: vec![decl::Expose {
                name: "cap".to_string(),
                from: decl::Ref::Hammerspace,
                availability: decl::Availability::Optional,
            }],
            children: Vec::new(),
        };

        // child_b uses "cap" from parent with optional availability
        let child_b_decl = decl::Component {
            uses: vec![decl::Use {
                name: "cap".to_string(),
                from: decl::Ref::Parent,
                availability: decl::Availability::Optional,
            }],
            offers: Vec::new(),
            exposes: Vec::new(),
            children: Vec::new(),
        };

        // root offers "cap" from child_a to child_b, with "same_as_target" availability
        let root_decl = decl::Component {
            uses: Vec::new(),
            offers: vec![decl::Offer {
                name: "cap".to_string(),
                from: decl::Ref::Child("child_a".to_string()),
                to: decl::Ref::Child("child_b".to_string()),
                availability: decl::Availability::SameAsTarget,
            }],
            exposes: Vec::new(),
            children: vec![
                decl::Child { name: "child_a".to_string() },
                decl::Child { name: "child_b".to_string() },
            ],
        };

        // Resolve the root node
        let root_unresolved = Unresolved::new();
        let mut root = resolve(root_unresolved, root_decl).await.unwrap();

        // Resolve child_a
        root.resolve_child("child_a".to_string(), child_a_decl).await.unwrap();

        // Resolve child_b
        root.resolve_child("child_b".to_string(), child_b_decl).await.unwrap();

        // child_b should be able to use the capability from child_a
        let child_b = match root.children.get("child_b").unwrap() {
            Node::Unresolved(_) => panic!(),
            Node::Resolved(resolved) => resolved,
        };
        let any: AnyCapability = child_b
            .program_interface
            .get_incoming(&"cap".to_string())
            .await
            .expect("child_b doesn't have the capability in its program interface");
        let lazy: Lazy = any.try_into().expect("failed to convert to Lazy");
        // child_b receives the capability as an Optional in its interface because it uses it
        // with `availability: optional`. So, it can see if the capability is Some or None.
        let optional: Optional =
            lazy.get().await.unwrap().try_into().expect("failed to convert to Optional");
        // The Optional should contain a value because child_a exposed it from a valid source.
        let any: AnyCapability = optional.0.expect("optional is missing value");
        let cap: Data = any.try_into().expect("failed to convert to Data");
        assert_eq!(cap, Data::String("cap".to_string()));
    }

    // Tests that a capability used with transitional availability from a source that does not
    // expose it results in void.
    #[fuchsia::test]
    async fn test_transitional_use() {
        // Set up the topology:
        //           root
        //          /
        //     child
        //

        // child uses "cap" from parent with transitional availability
        let child_decl = decl::Component {
            uses: vec![decl::Use {
                name: "cap".to_string(),
                from: decl::Ref::Parent,
                availability: decl::Availability::Transitional,
            }],
            offers: Vec::new(),
            exposes: Vec::new(),
            children: Vec::new(),
        };

        // root does not offer any capabilities to child.
        let root_decl = decl::Component {
            uses: Vec::new(),
            offers: vec![],
            exposes: Vec::new(),
            children: vec![decl::Child { name: "child".to_string() }],
        };

        // Resolve the root node
        let root_unresolved = Unresolved::new();
        let mut root = resolve(root_unresolved, root_decl).await.unwrap();

        // Resolve child
        root.resolve_child("child".to_string(), child_decl).await.unwrap();

        // child should get "cap" as void.
        let child = match root.children.get("child").unwrap() {
            Node::Unresolved(_) => panic!(),
            Node::Resolved(resolved) => resolved,
        };
        let any: AnyCapability = child
            .program_interface
            .get_incoming(&"cap".to_string())
            .await
            .expect("child doesn't have the capability in its program interface");
        // child receives the capability as an Optional in its interface because it uses it
        // with `availability: transitional`. So, it can see if the capability is Some or None.
        let optional: Optional = any.try_into().expect("failed to convert to Optional");
        // The Optional should not contain a value because root did not offer anything.
        assert!(optional.0.is_none());
    }
}
