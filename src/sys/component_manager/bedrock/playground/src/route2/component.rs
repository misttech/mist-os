// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use cm_types::Availability;
use futures::channel::oneshot::{self};
use moniker::Moniker;
use replace_with::replace_with;
use sandbox::{AsRouter, Capability, Data, Dict, Request, Router};
use std::{
    collections::HashMap,
    fmt,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

use crate::{decl, ResolveError, SourceError, TargetError};

use super::program::{Program, Runner};

/// A component consumes a dictionary and provides an router object which let others obtain
/// capabilities from it.
pub struct Component(Arc<Inner>);

pub struct Inner {
    name: String,
    state: Mutex<ComponentState>,
    resolver: Resolver,
}

enum ComponentState {
    /// Temporary state until the input dict is populated.
    MissingInput,
    Unresolved(Unresolved),
    Resolved(Resolved),
}

pub struct Unresolved {
    input: Dict,
}

pub struct Resolved {
    output: Dict,
    program: Program,
    children: HashMap<decl::ComponentName, Component>,
}

impl Component {
    pub fn new(name: String, resolver: Resolver) -> Self {
        Component(Arc::new(Inner {
            name,
            state: Mutex::new(ComponentState::MissingInput),
            resolver,
        }))
    }
}

impl Deref for Component {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl Inner {
    /// Explicitly resolve the component.
    pub fn resolve(&self) -> Router {
        let mut state = self.state.lock().unwrap();
        replace_with(state.deref_mut(), |state| state.resolve(&self.name, self.resolver.clone()));
        match state.deref_mut() {
            ComponentState::MissingInput | ComponentState::Unresolved(_) => {
                unreachable!("just resolved the component")
            }
            ComponentState::Resolved(ref resolved) => resolved.output.as_router(),
        }
    }

    /// Explicitly start the component.
    pub fn start(&self) {
        self.resolve();
        let mut state = self.state.lock().unwrap();
        match state.deref_mut() {
            ComponentState::MissingInput | ComponentState::Unresolved(_) => {
                unreachable!("just resolved the component")
            }
            ComponentState::Resolved(ref resolved) => {
                resolved.program.start();
            }
        }
    }

    pub fn set_input(&self, input: Dict) {
        let mut state = self.state.lock().unwrap();
        replace_with(state.deref_mut(), move |state| state.set_input(input));
    }
}

impl ComponentState {
    fn resolve(self, name: &str, resolver: Resolver) -> ComponentState {
        match self {
            ComponentState::MissingInput => panic!("must set input dict before resolve"),
            ComponentState::Unresolved(unresolved) => ComponentState::Resolved(
                resolve_impl(name, unresolved, (resolver.0)(name)).unwrap(),
            ),
            ComponentState::Resolved(resolved) => ComponentState::Resolved(resolved),
        }
    }

    fn set_input(self, input: Dict) -> ComponentState {
        match self {
            ComponentState::MissingInput => ComponentState::Unresolved(Unresolved { input }),
            ComponentState::Unresolved(_) => unimplemented!(),
            ComponentState::Resolved(_) => unimplemented!(),
        }
    }
}

/// # Some routing examples
///
/// ## Route from child to child case
///
/// - Get a router from the source child output
/// - Make a router that opens the desired capability from previous router
/// - If availability is specified, make a router that wraps that router and
///   checks/transforms availability
/// - If subdir is specified, make a router that wraps that router and prepends that path
/// - If multiple segments inside "from" is specified (capability bundles), make a router
///   that wraps that router and prepends those segments
/// - Put the resulting router inside the input dictionary of the target child
///
/// ## Route from parent to child case
///
/// - Get a router from the component input dict
/// - The rest is the same
///
fn route_from(
    cap_name: &decl::CapabilityName,
    source: &decl::Ref,
    input: Router,
    program_output: Router,
    child_outputs: &HashMap<decl::ComponentName, Router>,
) -> Result<Router, SourceError> {
    let source = match source {
        decl::Ref::Hammerspace => program_output,
        decl::Ref::Parent => input,
        decl::Ref::Child(child_name) => {
            let child = child_outputs
                .get(child_name)
                .ok_or_else(|| SourceError::ChildDoesNotExist(child_name.clone()))?;
            child.clone()
        }
    };
    Ok(source.get(cap_name.clone()))
}

/// Resolving a component creates the program and children, and returns an output
/// dictionary, in effect "locking down" the set of exposed capabilities for this
/// particular resolution.
///
/// ## Creating the output dict during resolution
///
/// For each expose, add the corresponding router from the source to the dict.
///
fn resolve_impl(
    name: &str,
    unresolved: Unresolved,
    decl: decl::Component,
) -> Result<Resolved, ResolveError> {
    let mut program_input = Dict::new();

    // Hacky way to get some runner. In practice we'd want to lazily route it via a router.
    let runner = unresolved.input.entries.get("runner").unwrap().try_clone().unwrap();
    let runner: Data<Runner> = runner.try_into().unwrap();
    let program: Program = Program::new(name.to_owned(), runner.value.clone());

    // Hacky way to get some resolver. In practice we'd want to lazily route it via a router.
    let resolver = unresolved.input.entries.get("resolver").unwrap().try_clone().unwrap();
    let resolver: Data<Resolver> = resolver.try_into().unwrap();
    let children: HashMap<decl::ComponentName, Component> = decl
        .children
        .iter()
        .map(|child| {
            (child.name.clone(), Component::new(child.name.clone(), resolver.value.clone()))
        })
        .collect();

    // For now, all children get the same runner and resolver.
    let pass_runner_resolver = || {
        let mut dict = Dict::new();
        dict.entries.insert("runner".to_string(), Box::new(runner.clone()));
        dict.entries.insert("resolver".to_string(), Box::new(resolver.clone()));
        dict
    };
    let mut children_inputs: HashMap<decl::ComponentName, Dict> =
        children.iter().map(|(key, _value)| (key.clone(), pass_runner_resolver())).collect();

    let children_outputs: HashMap<decl::ComponentName, Router> =
        children.iter().map(|(key, value)| (key.clone(), value.as_router())).collect();

    for use_ in decl.uses {
        let router = route_from(
            &use_.name,
            &use_.from,
            unresolved.input.as_router(),
            program.as_router(),
            &children_outputs,
        )?;
        let cap = router.availability(use_.availability);
        program_input.entries.insert(use_.name, Box::new(cap));
    }

    for offer in decl.offers {
        let router = route_from(
            &offer.name,
            &offer.from,
            unresolved.input.as_router(),
            program.as_router(),
            &children_outputs,
        )?;
        let cap = router.availability(offer.availability);
        // Determine the target dict and insert the capability into it.
        let target_dict: &mut Dict = match offer.to {
            decl::Ref::Hammerspace => &mut program_input,
            decl::Ref::Parent => panic!("cannot offer to parent"),
            decl::Ref::Child(child_name) => children_inputs
                .get_mut(&child_name)
                .ok_or_else(|| TargetError::ChildDoesNotExist(child_name.clone()))?,
        };
        target_dict.entries.insert(offer.name.clone(), Box::new(cap));
    }

    let mut output = Dict::new();
    for expose in decl.exposes {
        let router = route_from(
            &expose.name,
            &expose.from,
            unresolved.input.as_router(),
            program.as_router(),
            &children_outputs,
        )?;
        let cap = router.availability(expose.availability);
        output.entries.insert(expose.name.clone(), Box::new(cap));
    }

    program.set_input(program_input);
    for (child_name, child) in children.iter() {
        child.set_input(children_inputs.remove(child_name).unwrap());
    }

    Ok(Resolved { output, program, children })
}

/// How to mint output router when a component is created:
///
/// - Create an async function will lazily ensure the component is resolved, then grab the
///   output dictionary, then erase the capability
/// - Return this router from the component
///
impl AsRouter for Component {
    fn as_router(&self) -> Router {
        let weak = Arc::downgrade(&self.0);
        let route_fn = move |request, completer| {
            match weak.upgrade() {
                Some(component) => {
                    // Resolve the component if not already, then forward the request to its output.
                    let router = component.resolve();
                    router.route(request, completer);
                }
                None => completer.complete(Err(anyhow!("component is destroyed"))),
            }
        };
        Router::new(route_fn)
    }
}

#[derive(Clone)]
pub struct Resolver(pub Arc<dyn Fn(&str) -> decl::Component + Send + Sync>);

impl fmt::Debug for Resolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Resolver").finish()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Context;
    use moniker::MonikerBase;

    async fn build_test_topology(
        availability_a: Availability,
        availability_b: Availability,
    ) -> Result<(Component, Dict)> {
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
                availability: availability_a,
            }],
            children: Vec::new(),
        };

        // child_b uses "cap" from parent
        let child_b_decl = decl::Component {
            uses: vec![decl::Use {
                name: "cap".to_string(),
                from: decl::Ref::Parent,
                availability: availability_b,
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

        let resolver = move |name: &str| -> decl::Component {
            match name {
                "root" => root_decl.clone(),
                "child_a" => child_a_decl.clone(),
                "child_b" => child_b_decl.clone(),
                _ => panic!("unknown component {name}"),
            }
        };
        let resolver = Resolver(Arc::new(resolver));

        let (child_b_started_sender, child_b_started) = oneshot::channel();
        let child_b_started_sender = Arc::new(Mutex::new(Some(child_b_started_sender)));
        let runner = move |name: &str, input: Dict| -> Router {
            match name {
                "root" => Dict::new().as_router(),
                "child_a" => {
                    // child_a should see a request for "cap".
                    let mut output = Dict::new();
                    let cap = Data::new("hello".to_owned());
                    output.entries.insert("cap".to_owned(), Box::new(cap));
                    output.as_router()
                }
                "child_b" => {
                    let sender = child_b_started_sender.clone().lock().unwrap().take().unwrap();
                    sender.send(input).unwrap();
                    Dict::new().as_router()
                }
                _ => panic!("unknown program {name}"),
            }
        };
        let runner = Runner(Arc::new(runner));

        let mut root_input = Dict::new();
        root_input.entries.insert("resolver".to_string(), Box::new(Data::new(resolver.clone())));
        root_input.entries.insert("runner".to_string(), Box::new(Data::new(runner.clone())));

        // Resolve the root component.
        let root = Component::new("root".to_string(), resolver.clone());
        root.set_input(root_input);
        let _router = root.resolve();

        // Resolve and run child_b.
        match root.state.lock().unwrap().deref() {
            ComponentState::MissingInput | ComponentState::Unresolved(_) => {
                panic!("should be resolved")
            }
            ComponentState::Resolved(ref resolved) => {
                let child_b = resolved.children.get("child_b").unwrap();
                child_b.start();
            }
        }

        // Wait for child_a runner to receive request.
        let input = child_b_started.await.context("should receive one start request")?;
        Ok((root, input))
    }

    async fn try_route(
        availability_a: Availability,
        availability_b: Availability,
        use_availability: Availability,
    ) -> Result<()> {
        let (_root, input) = build_test_topology(availability_a, availability_b)
            .await
            .context("building test topology")?;

        // Using the input dict of child_b, check we can obtain "cap".
        let router = input.as_router();
        let request = Request {
            rights: None,
            relative_path: sandbox::Path::new("cap"),
            target_moniker: Moniker::new(vec![
                "root".try_into().unwrap(),
                "child_b".try_into().unwrap(),
            ]),
            availability: use_availability,
        };
        let cap = sandbox::route(&router, request).await.context("route")?;
        eprintln!("Obtained capability {:?}", cap);
        let cap: Data<String> = cap.try_into().context("convert to Data")?;
        assert_eq!(&cap.value, "hello");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_topology_success_availability() {
        try_route(Availability::Required, Availability::Required, Availability::Required)
            .await
            .unwrap();
        try_route(Availability::Required, Availability::Required, Availability::Optional)
            .await
            .unwrap();
        try_route(Availability::Required, Availability::Optional, Availability::Optional)
            .await
            .unwrap();
        try_route(
            Availability::Transitional,
            Availability::Transitional,
            Availability::Transitional,
        )
        .await
        .unwrap();
    }

    #[fuchsia::test]
    async fn test_topology_failure_availability() {
        try_route(Availability::Optional, Availability::Required, Availability::Optional)
            .await
            .unwrap_err();
        try_route(Availability::Required, Availability::Optional, Availability::Required)
            .await
            .unwrap_err();
        try_route(Availability::Optional, Availability::Optional, Availability::Required)
            .await
            .unwrap_err();
        try_route(Availability::Transitional, Availability::Transitional, Availability::Optional)
            .await
            .unwrap_err();
    }
}
