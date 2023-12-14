// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use cm_types::Availability;
use futures::channel::oneshot::{self};
use replace_with::replace_with;
use routing::{Routable, Router};
use sandbox::{Data, Dict, Opaque, Open};
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

    pub fn output(&self) -> Router {
        Router::from_routable(Arc::downgrade(&self.0))
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
            ComponentState::Resolved(ref resolved) => {
                Router::from_routable(resolved.output.clone())
            }
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
    Ok(source.with_name(cap_name.clone()))
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
    let runner = unresolved.input.lock_entries().get("runner").unwrap().clone();
    let runner: Opaque<Runner> = runner.try_into().unwrap();
    let program: Program = Program::new(name.to_owned(), runner.0.clone());

    // Hacky way to get some resolver. In practice we'd want to lazily route it via a router.
    let resolver = unresolved.input.lock_entries().get("resolver").unwrap().clone();
    let resolver: Opaque<Resolver> = resolver.try_into().unwrap();
    let children: HashMap<decl::ComponentName, Component> = decl
        .children
        .iter()
        .map(|child| (child.name.clone(), Component::new(child.name.clone(), resolver.0.clone())))
        .collect();

    // For now, all children get the same runner and resolver.
    let pass_runner_resolver = || {
        let dict = Dict::new();
        {
            let mut entries = dict.lock_entries();
            entries.insert("runner".to_string(), Box::new(runner.clone()));
            entries.insert("resolver".to_string(), Box::new(resolver.clone()));
        }
        dict
    };
    let mut children_inputs: HashMap<decl::ComponentName, Dict> =
        children.iter().map(|(key, _value)| (key.clone(), pass_runner_resolver())).collect();

    let children_outputs: HashMap<decl::ComponentName, Router> =
        children.iter().map(|(key, value)| (key.clone(), value.output())).collect();

    let input = Router::from_routable(unresolved.input);
    let program_output = program.output();

    for use_ in decl.uses {
        let router = route_from(
            &use_.name,
            &use_.from,
            input.clone(),
            program_output.clone(),
            &children_outputs,
        )?;
        let cap = router.with_availability(use_.availability);
        program_input.lock_entries().insert(use_.name, Box::new(cap));
    }

    for offer in decl.offers {
        let router = route_from(
            &offer.name,
            &offer.from,
            input.clone(),
            program_output.clone(),
            &children_outputs,
        )?;
        let cap = router.with_availability(offer.availability);
        // Determine the target dict and insert the capability into it.
        let target_dict: &mut Dict = match offer.to {
            decl::Ref::Hammerspace => &mut program_input,
            decl::Ref::Parent => panic!("cannot offer to parent"),
            decl::Ref::Child(child_name) => children_inputs
                .get_mut(&child_name)
                .ok_or_else(|| TargetError::ChildDoesNotExist(child_name.clone()))?,
        };
        target_dict.lock_entries().insert(offer.name.clone(), Box::new(cap));
    }

    let output = Dict::new();
    for expose in decl.exposes {
        let router = route_from(
            &expose.name,
            &expose.from,
            input.clone(),
            program_output.clone(),
            &children_outputs,
        )?;
        let cap = router.with_availability(expose.availability);
        output.lock_entries().insert(expose.name.clone(), Box::new(cap));
    }

    program.set_input(program_input);
    for (child_name, child) in children.iter() {
        child.set_input(children_inputs.remove(child_name).unwrap());
    }

    Ok(Resolved { output, program, children })
}

impl Routable for Inner {
    fn route(&self, request: routing::Request, completer: routing::Completer) {
        // Resolve the component if not already, then forward the request to its output.
        let router = self.resolve();
        router.route(request, completer);
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
    use fidl::endpoints::ClientEnd;
    use fidl_fidl_examples_routing_echo::{EchoMarker, EchoRequest, EchoRequestStream};
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use futures::{channel::mpsc, StreamExt};
    use routing::component_instance::AnyWeakComponentInstance;
    use serve_processargs::{ignore_not_found, NamespaceBuilder};
    use vfs::{
        directory::{entry::DirectoryEntry, helper::DirectlyMutable, immutable::simple as pfs},
        execution_scope::ExecutionScope,
        service,
    };

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
        let runner = move |name: &str, input: Dict| -> Dict {
            match name {
                "root" => Dict::new(),
                "child_a" => {
                    // child_a should see a request for "cap".
                    let output = Dict::new();
                    let cap = Data::String("hello".to_owned());
                    output.lock_entries().insert("cap".to_owned(), Box::new(cap));
                    output
                }
                "child_b" => {
                    let sender = child_b_started_sender.clone().lock().unwrap().take().unwrap();
                    sender.send(input).unwrap();
                    Dict::new()
                }
                _ => panic!("unknown program {name}"),
            }
        };
        let runner = Runner(Arc::new(runner));

        let root_input = Dict::new();
        {
            let mut entries = root_input.lock_entries();
            entries.insert("resolver".to_string(), Box::new(Opaque(resolver.clone())));
            entries.insert("runner".to_string(), Box::new(Opaque(runner.clone())));
        }

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
        let router = Router::from_routable(input);
        let request = routing::Request {
            rights: None,
            relative_path: sandbox::Path::new("cap"),
            availability: use_availability,
            target: AnyWeakComponentInstance::invalid_for_tests(),
        };
        let cap = routing::route(&router, request).await.context("route")?;
        eprintln!("Obtained capability {:?}", cap);
        let cap: Data = cap.try_into().context("convert to Data")?;
        assert_eq!(cap, Data::String("hello".to_string()));
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

    async fn echo_server(mut requests: EchoRequestStream) {
        loop {
            match requests.next().await {
                None => break,
                Some(Err(err)) => {
                    panic!("echo_server(): failed to receive a message: {}", err);
                }
                Some(Ok(EchoRequest::EchoString { value, responder })) => {
                    responder
                        .send(value.as_ref().map(|s| &**s))
                        .expect("echo_server(): responder.send() failed");
                }
            }
        }
    }

    /// Tests the integration with a fuchsia.io outgoing directory.
    ///
    /// It builds the following topology:
    ///
    ///           root
    ///          /    \
    ///     child_a   child_b
    ///
    /// child_a will expose a protocol named `fuchsia.echo.Echo`, located at the path
    /// `/svc/echo` in the outgoing directory of the program of child_a, for simplicity.
    ///
    /// child_b will use the `fuchsia.echo.Echo` protocol, located at the path
    /// `/svc/echo2` in the incoming namespace of the program of child_b, to show that
    /// these paths are independent.
    #[fuchsia::test]
    async fn test_expose_protocol_from_outgoing_directory() -> Result<()> {
        // child_a exposes capability "fuchsia.echo.Echo"
        let child_a_decl = decl::Component {
            uses: Vec::new(),
            offers: Vec::new(),
            exposes: vec![decl::Expose {
                name: "fuchsia.echo.Echo".to_string(),
                from: decl::Ref::Hammerspace,
                availability: Availability::Required,
            }],
            children: Vec::new(),
        };

        // child_b uses "fuchsia.echo.Echo" from parent
        let child_b_decl = decl::Component {
            uses: vec![decl::Use {
                name: "fuchsia.echo.Echo".to_string(),
                from: decl::Ref::Parent,
                availability: Availability::Required,
            }],
            offers: Vec::new(),
            exposes: Vec::new(),
            children: Vec::new(),
        };

        // root offers "fuchsia.echo.Echo" from child_a to child_b.
        let root_decl = decl::Component {
            uses: Vec::new(),
            offers: vec![decl::Offer {
                name: "fuchsia.echo.Echo".to_string(),
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

        let runner = move |name: &str, input: Dict| -> Dict {
            match name {
                "root" => Dict::new(),
                "child_a" => {
                    // Serve an outgoing directory in child_a that contains:
                    //
                    // - /svc
                    // -    /echo -> an echo protocol
                    //
                    let root = pfs::simple();
                    let svc = pfs::simple();
                    let echo = service::host(echo_server);
                    svc.add_entry("echo", echo).unwrap();
                    root.add_entry("svc", svc).unwrap();

                    let scope = ExecutionScope::new();
                    let (outgoing, server_end) = fidl::endpoints::create_endpoints();
                    root.open(
                        scope,
                        fio::OpenFlags::RIGHT_READABLE,
                        ".".try_into().unwrap(),
                        server_end,
                    );
                    let outgoing: ClientEnd<fio::DirectoryMarker> = outgoing.into_channel().into();

                    // Represent the outgoing directory of this program as an Open.
                    let outgoing = Open::from(outgoing);

                    // Create an Open that opens `svc/echo` from the outgoing directory. The
                    // "svc/echo" corresponds to the "path" field of an ExposeDecl
                    // (program interface).
                    let echo = outgoing.downscope_path(sandbox::Path::new("svc/echo"));

                    // Add the Open to the output dictionary. "fuchsia.echo.Echo" is the name of
                    // the capability in the output dictionary of the component
                    // (component-to-component interface).
                    let output = Dict::new();
                    output.lock_entries().insert("fuchsia.echo.Echo".to_string(), Box::new(echo));
                    output
                }
                "child_b" => {
                    let sender = child_b_started_sender.clone().lock().unwrap().take().unwrap();

                    // Given the input dict of the child_b component, make a namespace for a
                    // hypothetical child_b program, then send it to the main test function.
                    let input = Router::from_routable(input);
                    let (errors, errors_receiver) = mpsc::unbounded();

                    // Makes an Open that will request the `fuchsia.echo.Echo` capability
                    // from the `input` router every time someone opens it.
                    //
                    // This corresponds to a UseDecl that uses "fuchsia.echo.Echo" from the
                    // input dictionary (component-to-component contract).
                    let echo = input.into_open(
                        routing::Request {
                            rights: None,
                            relative_path: sandbox::Path::new("fuchsia.echo.Echo"),
                            availability: Availability::Required,
                            target: AnyWeakComponentInstance::invalid_for_tests(),
                        },
                        fio::DirentType::Service,
                        errors,
                    );

                    // Install the Open into a namespace. The "/svc/echo2" corresponds to the
                    // "path" in a UseDecl (program contract).
                    let mut namespace = NamespaceBuilder::new(ignore_not_found());
                    namespace
                        .add_object(
                            Box::new(echo),
                            &namespace::Path::try_from("/svc/echo2").unwrap(),
                        )
                        .unwrap();
                    sender.send(namespace.serve().unwrap()).map_err(|_| ()).unwrap();

                    // Log errors when routing capabilities.
                    fasync::Task::spawn(async move {
                        let mut errors_receiver = errors_receiver;
                        while let Some(err) = errors_receiver.next().await {
                            tracing::error!("Error during routing: {err}");
                        }
                    })
                    .detach();

                    // child_b outputs no capabilities.
                    Dict::new()
                }
                _ => panic!("unknown program {name}"),
            }
        };
        let runner = Runner(Arc::new(runner));

        let root_input = Dict::new();
        root_input
            .lock_entries()
            .insert("resolver".to_string(), Box::new(Opaque(resolver.clone())));
        root_input.lock_entries().insert("runner".to_string(), Box::new(Opaque(runner.clone())));

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

        // Obtain the namespace of child_b when it is started.
        let (namespace, fut) = child_b_started.await.context("should receive one start request")?;

        // Serve the namespace in the background.
        fasync::Task::spawn(fut).detach();

        // Connect to "/svc/echo2" in the namespace.
        // We should reach the `echo_server` in child_a.
        let mut entries = namespace.flatten();
        assert_eq!(entries.len(), 1);
        let svc = entries.pop().unwrap();
        assert_eq!(svc.path.as_str(), "/svc");
        let svc = svc.directory;
        let echo = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<EchoMarker>(
            &svc, "echo2",
        )
        .unwrap();

        assert_eq!(echo.echo_string(Some("hello")).await.unwrap().unwrap(), "hello");

        Ok(())
    }
}
