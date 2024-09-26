// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::availability::AvailabilityMetadata;
use crate::bedrock::request_metadata::METADATA_KEY_TYPE;
use crate::bedrock::structured_dict::{ComponentEnvironment, ComponentInput, StructuredDictMap};
use crate::bedrock::with_porcelain_type::WithPorcelainType as _;
use crate::capability_source::{CapabilitySource, InternalCapability, VoidSource};
use crate::component_instance::{ComponentInstanceInterface, WeakComponentInstanceInterface};
use crate::error::RoutingError;
use crate::{DictExt, LazyGet, WithAvailability, WithDefault};
use async_trait::async_trait;
use cm_rust::{
    CapabilityTypeName, ExposeDeclCommon, OfferDeclCommon, SourceName, SourcePath, UseDeclCommon,
};
use cm_types::{IterablePath, Name, SeparatedPath};
use fidl::endpoints::DiscoverableProtocolMarker;
use futures::FutureExt;
use itertools::Itertools;
use moniker::{ChildName, Moniker};
use router_error::RouterError;
use sandbox::{Capability, Data, Dict, Request, Router, Unit};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use tracing::warn;
use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_sys2 as fsys};

/// All capabilities that are available to a component's program.
#[derive(Default, Debug, Clone)]
pub struct ProgramInput {
    /// All of the capabilities that appear in a program's namespace.
    pub namespace: Dict,

    /// A router for the runner that a component has used (if any). This is in an `Arc<Mutex<_>>`
    /// because it needs to match the semantics of `Dict`, notably that clones grab new references
    /// to the same underlying data and the data can be mutated without a mutable reference to
    /// `ProgramInput`.
    pub runner: Arc<Mutex<Option<Router>>>,

    /// All of the config capabilities that a program will use.
    pub config: Dict,
}

/// A component's sandbox holds all the routing dictionaries that a component has once its been
/// resolved.
#[derive(Default, Debug, Clone)]
pub struct ComponentSandbox {
    /// The dictionary containing all capabilities that a component's parent provided to it.
    pub component_input: ComponentInput,

    /// The dictionary containing all capabilities that a component makes available to its parent.
    pub component_output_dict: Dict,

    /// The dictionary containing all capabilities that are available to a component's program.
    pub program_input: ProgramInput,

    /// The dictionary containing all capabilities that a component's program can provide.
    pub program_output_dict: Dict,

    /// The dictionary containing all framework capabilities that are available to a component.
    pub framework_dict: Dict,

    /// The dictionary containing all capabilities that a component declares based on another
    /// capability. Currently this is only the storage admin protocol.
    pub capability_sourced_capabilities_dict: Dict,

    /// The dictionary containing all dictionaries declared by this component.
    pub declared_dictionaries: Dict,

    /// This set holds a component input dictionary for each child of a component. Each dictionary
    /// contains all capabilities the component has made available to a specific collection.
    pub child_inputs: StructuredDictMap<ComponentInput>,

    /// This set holds a component input dictionary for each collection declared by a component.
    /// Each dictionary contains all capabilities the component has made available to a specific
    /// collection.
    pub collection_inputs: StructuredDictMap<ComponentInput>,
}

impl ComponentSandbox {
    /// Copies all of the entries from the given sandbox into this one. Panics if the given sandbox
    /// is holding any entries that cannot be copied. Panics if there are any duplicate entries.
    pub fn append(&self, sandbox: &ComponentSandbox) {
        // We destructure the sandbox here to ensure that this code is updated if the contents of
        // the sandbox change.
        let ComponentSandbox {
            component_input,
            component_output_dict,
            program_input,
            program_output_dict,
            framework_dict,
            capability_sourced_capabilities_dict,
            declared_dictionaries,
            child_inputs,
            collection_inputs,
        } = sandbox;
        for (copy_from, copy_to) in &[
            (&component_input.capabilities(), &self.component_input.capabilities()),
            (&component_input.environment().debug(), &self.component_input.environment().debug()),
            (
                &component_input.environment().runners(),
                &self.component_input.environment().runners(),
            ),
            (
                &component_input.environment().resolvers(),
                &self.component_input.environment().resolvers(),
            ),
            (&component_output_dict, &self.component_output_dict),
            (&program_input.namespace, &self.program_input.namespace),
            (&program_input.config, &self.program_input.config),
            (&program_output_dict, &self.program_output_dict),
            (&framework_dict, &self.framework_dict),
            (&capability_sourced_capabilities_dict, &self.capability_sourced_capabilities_dict),
            (&declared_dictionaries, &self.declared_dictionaries),
        ] {
            for (key, capability_res) in copy_from.enumerate() {
                copy_to
                    .insert(key, capability_res.expect("sandbox capability is not cloneable"))
                    .unwrap();
            }
        }
        if let Some(runner_router) = program_input.runner.lock().unwrap().take() {
            *self.program_input.runner.lock().unwrap() = Some(runner_router);
        }
        for (key, component_input) in child_inputs.enumerate() {
            self.child_inputs.insert(key, component_input).unwrap();
        }
        for (key, component_input) in collection_inputs.enumerate() {
            self.collection_inputs.insert(key, component_input).unwrap();
        }
    }
}

/// Once a component has been resolved and its manifest becomes known, this function produces the
/// various dicts the component needs based on the contents of its manifest.
pub fn build_component_sandbox<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: HashMap<ChildName, Router>,
    decl: &cm_rust::ComponentDecl,
    component_input: ComponentInput,
    program_input_dict_additions: &Dict,
    program_output_dict: Dict,
    program_output_router: Router,
    framework_dict: Dict,
    capability_sourced_capabilities_dict: Dict,
    declared_dictionaries: Dict,
) -> ComponentSandbox {
    let component_output_dict = Dict::new();
    let program_input = ProgramInput::default();
    let environments: StructuredDictMap<ComponentEnvironment> = Default::default();
    let child_inputs: StructuredDictMap<ComponentInput> = Default::default();
    let collection_inputs: StructuredDictMap<ComponentInput> = Default::default();

    for environment_decl in &decl.environments {
        environments
            .insert(
                environment_decl.name.clone(),
                build_environment(
                    &component.moniker(),
                    &child_component_output_dictionary_routers,
                    &component_input,
                    environment_decl,
                    program_input_dict_additions,
                    &program_output_router,
                ),
            )
            .ok();
    }

    for child in &decl.children {
        let environment;
        if let Some(environment_name) = child.environment.as_ref() {
            environment = environments.get(environment_name).expect(
                "child references nonexistent environment, \
                    this should be prevented in manifest validation",
            );
        } else {
            environment = component_input.environment();
        }
        let input = ComponentInput::new(environment);
        let name = Name::new(child.name.as_str()).expect("child is static so name is not long");
        child_inputs.insert(name, input).ok();
    }

    for collection in &decl.collections {
        let environment;
        if let Some(environment_name) = collection.environment.as_ref() {
            environment = environments.get(environment_name).expect(
                "collection references nonexistent environment, \
                    this should be prevented in manifest validation",
            )
        } else {
            environment = component_input.environment();
        }
        let input = ComponentInput::new(environment);
        collection_inputs.insert(collection.name.clone(), input).ok();
    }

    for use_ in &decl.uses {
        extend_dict_with_use(
            &component,
            &child_component_output_dictionary_routers,
            &component_input,
            &program_input,
            program_input_dict_additions,
            &program_output_router,
            &framework_dict,
            &capability_sourced_capabilities_dict,
            use_,
        );
    }

    // The runner may be specified by either use declaration or in the program section of the
    // manifest. If there's no use declaration for a runner and there is one set in the program
    // section, then let's synthesize a use decl for it and add it to the sandbox.
    if !decl.uses.iter().any(|u| matches!(u, cm_rust::UseDecl::Runner(_))) {
        if let Some(runner_name) = decl.program.as_ref().and_then(|p| p.runner.as_ref()) {
            extend_dict_with_use(
                &component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_input,
                program_input_dict_additions,
                &program_output_router,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                &cm_rust::UseDecl::Runner(cm_rust::UseRunnerDecl {
                    source: cm_rust::UseSource::Environment,
                    source_name: runner_name.clone(),
                    source_dictionary: Default::default(),
                }),
            );
        }
    }

    for offer in &decl.offers {
        if !is_supported_offer(offer) {
            continue;
        }
        let target_dict = match offer.target() {
            cm_rust::OfferTarget::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name = Name::new(child_ref.name.as_str())
                    .expect("child is static so name is not long");
                if child_inputs.get(&child_name).is_none() {
                    child_inputs.insert(child_name.clone(), Default::default()).ok();
                }
                child_inputs
                    .get(&child_name)
                    .expect("component input was just added")
                    .capabilities()
            }
            cm_rust::OfferTarget::Collection(name) => {
                if collection_inputs.get(name).is_none() {
                    collection_inputs.insert(name.clone(), Default::default()).ok();
                }
                collection_inputs.get(name).expect("collection input was just added").capabilities()
            }
            cm_rust::OfferTarget::Capability(name) => {
                let dict = match declared_dictionaries
                    .get(name)
                    .expect("dictionaries must be cloneable")
                {
                    Some(dict) => dict,
                    None => {
                        let dict = Dict::new();
                        declared_dictionaries
                            .insert(name.clone(), Capability::Dictionary(dict.clone()))
                            .ok();
                        Capability::Dictionary(dict)
                    }
                };
                let Capability::Dictionary(dict) = dict else {
                    panic!("wrong type in dict");
                };
                dict
            }
        };
        extend_dict_with_offer(
            component,
            &child_component_output_dictionary_routers,
            &component_input,
            &program_output_router,
            &framework_dict,
            &capability_sourced_capabilities_dict,
            offer,
            &target_dict,
        );
    }

    for expose in &decl.exposes {
        extend_dict_with_expose(
            component,
            &child_component_output_dictionary_routers,
            &program_output_router,
            &framework_dict,
            &capability_sourced_capabilities_dict,
            expose,
            &component_output_dict,
        );
    }

    ComponentSandbox {
        component_input,
        component_output_dict,
        program_input,
        program_output_dict,
        framework_dict,
        capability_sourced_capabilities_dict,
        declared_dictionaries,
        child_inputs,
        collection_inputs,
    }
}

fn build_environment(
    moniker: &Moniker,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    component_input: &ComponentInput,
    environment_decl: &cm_rust::EnvironmentDecl,
    program_input_dict_additions: &Dict,
    program_output_router: &Router,
) -> ComponentEnvironment {
    let mut environment = ComponentEnvironment::new();
    if environment_decl.extends == fdecl::EnvironmentExtends::Realm {
        if let Ok(e) = component_input.environment().shallow_copy() {
            environment = e;
        } else {
            warn!("failed to copy component_input.environment");
        }
    }
    let debug = environment_decl.debug_capabilities.iter().map(|debug_registration| {
        let cm_rust::DebugRegistration::Protocol(debug) = debug_registration;
        (&debug.source_name, debug.target_name.clone(), &debug.source, CapabilityTypeName::Protocol)
    });
    let runners = environment_decl.runners.iter().map(|runner| {
        (
            &runner.source_name,
            runner.target_name.clone(),
            &runner.source,
            CapabilityTypeName::Runner,
        )
    });
    let resolvers = environment_decl.resolvers.iter().map(|resolver| {
        (
            &resolver.resolver,
            Name::new(&resolver.scheme).unwrap(),
            &resolver.source,
            CapabilityTypeName::Resolver,
        )
    });
    for (source_name, target_name, source, cap_type) in debug.chain(runners).chain(resolvers) {
        let source_path =
            SeparatedPath { dirname: Default::default(), basename: source_name.clone() };
        let router = match &source {
            cm_rust::RegistrationSource::Parent => use_from_parent_router(
                component_input,
                source_path,
                moniker,
                program_input_dict_additions,
            )
            .with_porcelain_type(cap_type, moniker.clone()),
            cm_rust::RegistrationSource::Self_ => program_output_router
                .clone()
                .lazy_get(
                    source_path.clone(),
                    RoutingError::use_from_self_not_found(
                        moniker,
                        source_path.iter_segments().join("/"),
                    ),
                )
                .with_porcelain_type(cap_type, moniker.clone()),
            cm_rust::RegistrationSource::Child(child_name) => {
                let child_name = ChildName::parse(child_name).expect("invalid child name");
                let Some(child_component_output) =
                    child_component_output_dictionary_routers.get(&child_name)
                else {
                    continue;
                };
                child_component_output
                    .clone()
                    .lazy_get(
                        source_path,
                        RoutingError::use_from_child_expose_not_found(
                            &child_name,
                            &moniker,
                            source_name.clone(),
                        ),
                    )
                    .with_porcelain_type(cap_type, moniker.clone())
            }
        };
        let dict_to_insert_to = match cap_type {
            CapabilityTypeName::Protocol => environment.debug(),
            CapabilityTypeName::Runner => environment.runners(),
            CapabilityTypeName::Resolver => environment.resolvers(),
            c => panic!("unexpected capability type {}", c),
        };
        match dict_to_insert_to.insert_capability(&target_name, router.into()) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {} {} to environment: {e:?}", cap_type, target_name),
        }
    }
    environment
}

/// Extends the given dict based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_dict`.
pub fn extend_dict_with_offers<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    component_input: &ComponentInput,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    program_output_router: &Router,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    target_input: &ComponentInput,
) {
    for offer in dynamic_offers {
        extend_dict_with_offer(
            component,
            &child_component_output_dictionary_routers,
            component_input,
            program_output_router,
            framework_dict,
            capability_sourced_capabilities_dict,
            offer,
            &target_input.capabilities(),
        );
    }
}

fn is_supported_use(use_: &cm_rust::UseDecl) -> bool {
    matches!(
        use_,
        cm_rust::UseDecl::Config(_) | cm_rust::UseDecl::Protocol(_) | cm_rust::UseDecl::Runner(_)
    )
}

// Add the `config_use` to the `program_input_dict`, so the component is able to
// access this configuration.
fn extend_dict_with_config_use<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    component_input: &ComponentInput,
    program_input: &ProgramInput,
    program_input_dict_additions: &Dict,
    program_output_router: &Router,
    config_use: &cm_rust::UseConfigurationDecl,
) {
    let moniker = component.moniker();
    let source_path = config_use.source_path();
    let porcelain_type = CapabilityTypeName::Config;
    let router = match config_use.source() {
        cm_rust::UseSource::Parent => use_from_parent_router(
            component_input,
            source_path.to_owned(),
            moniker,
            program_input_dict_additions,
        )
        .with_porcelain_type(porcelain_type, moniker.clone()),
        cm_rust::UseSource::Self_ => program_output_router
            .clone()
            .lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_self_not_found(
                    moniker,
                    source_path.iter_segments().join("/"),
                ),
            )
            .with_porcelain_type(porcelain_type, moniker.clone()),
        cm_rust::UseSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid child name");
            let Some(child_component_output) =
                child_component_output_dictionary_routers.get(&child_name)
            else {
                panic!("use declaration in manifest for component {} has a source of a nonexistent child {}, this should be prevented by manifest validation", moniker, child_name);
            };
            child_component_output
                .clone()
                .lazy_get(
                    source_path.to_owned(),
                    RoutingError::use_from_child_expose_not_found(
                        &child_name,
                        &moniker,
                        config_use.source_name().clone(),
                    ),
                )
                .with_porcelain_type(porcelain_type, moniker.clone())
        }
        // The following are not used with config capabilities.
        cm_rust::UseSource::Environment => return,
        cm_rust::UseSource::Debug => return,
        cm_rust::UseSource::Capability(_) => return,
        cm_rust::UseSource::Framework => return,
    };

    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default use metadata?");
    metadata.set_availability(*config_use.availability());
    let default_request = Request { metadata, target: component.as_weak().into() };
    match program_input.config.insert_capability(
        &config_use.target_name,
        router
            .with_availability(moniker.clone(), *config_use.availability())
            .with_default(default_request)
            .into(),
    ) {
        Ok(()) => (),
        Err(e) => {
            warn!("failed to insert {} in program input dict: {e:?}", config_use.target_name)
        }
    }
}

fn extend_dict_with_use<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    component_input: &ComponentInput,
    program_input: &ProgramInput,
    program_input_dict_additions: &Dict,
    program_output_router: &Router,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    use_: &cm_rust::UseDecl,
) {
    let moniker = component.moniker();
    if !is_supported_use(use_) {
        return;
    }
    if let cm_rust::UseDecl::Config(config) = use_ {
        return extend_dict_with_config_use(
            component,
            child_component_output_dictionary_routers,
            component_input,
            program_input,
            program_input_dict_additions,
            program_output_router,
            config,
        );
    };

    let source_path = use_.source_path();
    let porcelain_type = CapabilityTypeName::from(use_);
    let router = match use_.source() {
        cm_rust::UseSource::Parent => use_from_parent_router(
            component_input,
            source_path.to_owned(),
            moniker,
            program_input_dict_additions,
        )
        .with_porcelain_type(porcelain_type, moniker.clone()),
        cm_rust::UseSource::Self_ => program_output_router
            .clone()
            .lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_self_not_found(
                    moniker,
                    source_path.iter_segments().join("/"),
                ),
            )
            .with_porcelain_type(porcelain_type, moniker.clone()),
        cm_rust::UseSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid child name");
            let Some(child_component_output) =
                child_component_output_dictionary_routers.get(&child_name)
            else {
                panic!("use declaration in manifest for component {} has a source of a nonexistent child {}, this should be prevented by manifest validation", moniker, child_name);
            };
            child_component_output
                .clone()
                .lazy_get(
                    source_path.to_owned(),
                    RoutingError::use_from_child_expose_not_found(
                        &child_name,
                        &moniker,
                        use_.source_name().clone(),
                    ),
                )
                .with_porcelain_type(porcelain_type, moniker.clone())
        }
        cm_rust::UseSource::Framework if use_.is_from_dictionary() => {
            Router::new_error(RoutingError::capability_from_framework_not_found(
                moniker,
                source_path.iter_segments().join("/"),
            ))
        }
        cm_rust::UseSource::Framework => framework_dict
            .clone()
            .lazy_get(
                source_path.to_owned(),
                RoutingError::capability_from_framework_not_found(
                    moniker,
                    source_path.iter_segments().join("/"),
                ),
            )
            .with_porcelain_type(porcelain_type, moniker.clone()),
        cm_rust::UseSource::Capability(capability_name) => {
            let err = RoutingError::capability_from_capability_not_found(
                moniker,
                capability_name.as_str().to_string(),
            );
            if source_path.iter_segments().join("/") == fsys::StorageAdminMarker::PROTOCOL_NAME {
                capability_sourced_capabilities_dict.clone().lazy_get(capability_name.clone(), err)
            } else {
                Router::new_error(err)
            }
        }
        cm_rust::UseSource::Debug => {
            let cm_rust::UseDecl::Protocol(use_protocol) = use_ else {
                panic!("non-protocol capability used with a debug source, this should be prevented by manifest validation");
            };
            component_input
                .environment()
                .debug()
                .lazy_get(
                    use_protocol.source_name.clone(),
                    RoutingError::use_from_environment_not_found(
                        moniker,
                        "protocol",
                        &use_protocol.source_name,
                    ),
                )
                .with_porcelain_type(use_.into(), moniker.clone())
        }
        cm_rust::UseSource::Environment => {
            let cm_rust::UseDecl::Runner(use_runner) = use_ else {
                panic!("non-runner capability used with an environment source, this should be prevented by manifest validation");
            };
            component_input
                .environment()
                .runners()
                .lazy_get(
                    use_runner.source_name.clone(),
                    RoutingError::use_from_environment_not_found(
                        moniker,
                        "runner",
                        &use_runner.source_name,
                    ),
                )
                .with_porcelain_type(porcelain_type, moniker.clone())
        }
    };
    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default use metadata?");
    metadata.set_availability(*use_.availability());
    let default_request = Request { metadata, target: component.as_weak().into() };
    let router = router
        .with_availability(moniker.clone(), *use_.availability())
        .with_default(default_request);
    if let Some(target_path) = use_.path() {
        if let Err(e) = program_input.namespace.insert_capability(target_path, router.into()) {
            warn!("failed to insert {} in program input dict: {e:?}", target_path)
        }
    } else {
        match use_ {
            cm_rust::UseDecl::Runner(_) => {
                let mut runner_guard = program_input.runner.lock().unwrap();
                assert!(runner_guard.is_none(), "component can't use multiple runners");
                *runner_guard = Some(router);
            }
            _ => panic!("unexpected capability type: {:?}", use_),
        }
    }
}

/// Builds a router that obtains a capability that the program uses from `parent`.
///
/// The capability is usually an entry in the `component_input.capabilities` dict unless it is
/// overridden by an eponymous capability in the `program_input_dict_additions` when started.
fn use_from_parent_router(
    component_input: &ComponentInput,
    source_path: impl IterablePath + 'static + Debug,
    moniker: &Moniker,
    program_input_dict_additions: &Dict,
) -> Router {
    let err = if moniker == &Moniker::root() {
        RoutingError::register_from_component_manager_not_found(
            source_path.iter_segments().join("/"),
        )
    } else {
        RoutingError::use_from_parent_not_found(moniker, source_path.iter_segments().join("/"))
    };
    let component_input_capability =
        component_input.capabilities().lazy_get(source_path.clone(), err);

    let program_input_dict_additions = program_input_dict_additions.clone();

    Router::new(move |request: Option<Request>, debug: bool| {
        let source_path = source_path.clone();
        let component_input_capability = component_input_capability.clone();
        let router = match program_input_dict_additions.get_capability(&source_path) {
            // There's an addition to the program input dictionary for this
            // capability, let's use it.
            Some(Capability::Connector(s)) => Router::new_ok(s),
            // There's no addition to the program input dictionary for this
            // capability, let's use the component input dictionary.
            _ => component_input_capability,
        };
        async move { router.route(request, debug).await }.boxed()
    })
}

fn is_supported_offer(offer: &cm_rust::OfferDecl) -> bool {
    matches!(
        offer,
        cm_rust::OfferDecl::Config(_)
            | cm_rust::OfferDecl::Protocol(_)
            | cm_rust::OfferDecl::Dictionary(_)
            | cm_rust::OfferDecl::Runner(_)
            | cm_rust::OfferDecl::Resolver(_)
    )
}

fn extend_dict_with_offer<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    component_input: &ComponentInput,
    program_output_router: &Router,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    offer: &cm_rust::OfferDecl,
    target_dict: &Dict,
) {
    if !is_supported_offer(offer) {
        return;
    }
    let source_path = offer.source_path();
    let target_name = offer.target_name();
    if target_dict.get_capability(&source_path).is_some() {
        warn!(
            "duplicate sources for protocol {} in a dict, unable to populate dict entry",
            target_name
        );
        target_dict.remove_capability(target_name);
        return;
    }
    let porcelain_type = CapabilityTypeName::from(offer);
    let router = match offer.source() {
        cm_rust::OfferSource::Parent => {
            let err = if component.moniker() == &Moniker::root() {
                RoutingError::register_from_component_manager_not_found(
                    offer.source_name().to_string(),
                )
            } else {
                RoutingError::offer_from_parent_not_found(
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                )
            };
            let router = component_input.capabilities().lazy_get(source_path.to_owned(), err);
            router.with_porcelain_type(porcelain_type, component.moniker().clone())
        }
        cm_rust::OfferSource::Self_ => {
            let router = program_output_router.clone().lazy_get(
                source_path.to_owned(),
                RoutingError::offer_from_self_not_found(
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                ),
            );
            router.with_porcelain_type(porcelain_type, component.moniker().clone())
        }
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            match child_component_output_dictionary_routers.get(&child_name) {
                None => Router::new_error(RoutingError::offer_from_child_instance_not_found(
                    &child_name,
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                )),
                Some(child_component_output) => {
                    let router = child_component_output.clone().lazy_get(
                        source_path.to_owned(),
                        RoutingError::offer_from_child_expose_not_found(
                            &child_name,
                            &component.moniker(),
                            offer.source_name().clone(),
                        ),
                    );
                    match offer {
                        cm_rust::OfferDecl::Protocol(_) => {
                            router.with_porcelain_type(porcelain_type, component.moniker().clone())
                        }
                        _ => router,
                    }
                }
            }
        }
        cm_rust::OfferSource::Framework => {
            if offer.is_from_dictionary() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let router = framework_dict.clone().lazy_get(
                source_path.to_owned(),
                RoutingError::capability_from_framework_not_found(
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                ),
            );
            router.with_porcelain_type(offer.into(), component.moniker().clone())
        }
        cm_rust::OfferSource::Capability(capability_name) => {
            let err = RoutingError::capability_from_capability_not_found(
                &component.moniker(),
                capability_name.as_str().to_string(),
            );
            if source_path.iter_segments().join("/") == fsys::StorageAdminMarker::PROTOCOL_NAME {
                capability_sourced_capabilities_dict.clone().lazy_get(capability_name.clone(), err)
            } else {
                Router::new_error(err)
            }
        }
        cm_rust::OfferSource::Void => {
            UnitRouter::new(InternalCapability::Protocol(offer.source_name().clone()), component)
        }
        // This is only relevant for services, so this arm is never reached.
        cm_rust::OfferSource::Collection(_name) => return,
    };
    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default offer metadata?");
    metadata.set_availability(*offer.availability());
    // Offered capabilities need to support default requests in the case of offer-to-dictionary.
    // This is a corollary of the fact that program_input_dictionary and
    // component_output_dictionary support default requests, and we need this to cover the case
    // where the use or expose is from a dictionary.
    //
    // Technically, we could restrict this to the case of offer-to-dictionary, not offer in
    // general. However, supporting the general case simplifies the logic and establishes a nice
    // symmetry between program_input_dict, component_output_dict, and {child,collection}_inputs.
    let default_request = Request { metadata, target: component.as_weak().into() };
    match target_dict.insert_capability(
        target_name,
        router
            .with_availability(component.moniker().clone(), *offer.availability())
            .with_default(default_request)
            .into(),
    ) {
        Ok(()) => (),
        Err(e) => warn!("failed to insert {target_name} into target dict: {e:?}"),
    }
}

pub fn is_supported_expose(expose: &cm_rust::ExposeDecl) -> bool {
    matches!(
        expose,
        cm_rust::ExposeDecl::Config(_)
            | cm_rust::ExposeDecl::Protocol(_)
            | cm_rust::ExposeDecl::Dictionary(_)
            | cm_rust::ExposeDecl::Runner(_)
            | cm_rust::ExposeDecl::Resolver(_)
    )
}

fn extend_dict_with_expose<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router>,
    program_output_router: &Router,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
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

    let porcelain_type = CapabilityTypeName::from(expose);
    let router = match expose.source() {
        cm_rust::ExposeSource::Self_ => program_output_router
            .clone()
            .lazy_get(
                source_path.to_owned(),
                RoutingError::expose_from_self_not_found(
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                ),
            )
            .with_porcelain_type(porcelain_type, component.moniker().clone()),
        cm_rust::ExposeSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid static child name");
            if let Some(child_component_output) =
                child_component_output_dictionary_routers.get(&child_name)
            {
                let router = child_component_output.clone().lazy_get(
                    source_path.to_owned(),
                    RoutingError::expose_from_child_expose_not_found(
                        &child_name,
                        &component.moniker(),
                        expose.source_name().clone(),
                    ),
                );
                match expose {
                    cm_rust::ExposeDecl::Protocol(_) => {
                        router.with_porcelain_type(porcelain_type, component.moniker().clone())
                    }
                    _ => router,
                }
            } else {
                Router::new_error(RoutingError::expose_from_child_instance_not_found(
                    &child_name,
                    &component.moniker(),
                    expose.source_name().clone(),
                ))
            }
        }
        cm_rust::ExposeSource::Framework => {
            if expose.is_from_dictionary() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            framework_dict
                .clone()
                .lazy_get(
                    source_path.to_owned(),
                    RoutingError::capability_from_framework_not_found(
                        &component.moniker(),
                        source_path.iter_segments().join("/"),
                    ),
                )
                .with_porcelain_type(porcelain_type, component.moniker().clone())
        }
        cm_rust::ExposeSource::Capability(capability_name) => {
            let err = RoutingError::capability_from_capability_not_found(
                &component.moniker(),
                capability_name.as_str().to_string(),
            );
            if source_path.iter_segments().join("/") == fsys::StorageAdminMarker::PROTOCOL_NAME {
                capability_sourced_capabilities_dict.clone().lazy_get(capability_name.clone(), err)
            } else {
                Router::new_error(err)
            }
        }
        cm_rust::ExposeSource::Void => {
            UnitRouter::new(InternalCapability::Protocol(expose.source_name().clone()), component)
        }
        // This is only relevant for services, so this arm is never reached.
        cm_rust::ExposeSource::Collection(_name) => return,
    };
    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default expose metadata?");
    metadata.set_availability(*expose.availability());
    let default_request = Request { metadata, target: component.as_weak().into() };
    match target_dict.insert_capability(
        target_name,
        router
            .with_availability(component.moniker().clone(), *expose.availability())
            .with_default(default_request)
            .into(),
    ) {
        Ok(()) => (),
        Err(e) => warn!("failed to insert {target_name} into target_dict: {e:?}"),
    }
}

struct UnitRouter<C: ComponentInstanceInterface> {
    capability: InternalCapability,
    component: WeakComponentInstanceInterface<C>,
}

impl<C: ComponentInstanceInterface + 'static> UnitRouter<C> {
    fn new(capability: InternalCapability, component: &Arc<C>) -> Router {
        Router::new(UnitRouter { capability, component: component.as_weak() })
    }
}

#[async_trait]
impl<C: ComponentInstanceInterface + 'static> sandbox::Routable for UnitRouter<C> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Capability, RouterError> {
        if debug {
            return Ok(CapabilitySource::Void(VoidSource {
                capability: self.capability.clone(),
                moniker: self.component.moniker.clone(),
            })
            .try_into()
            .expect("failed to convert capability source to dictionary"));
        }
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let availability = request.metadata.get_availability().ok_or(RouterError::InvalidArgs)?;
        match availability {
            cm_rust::Availability::Required | cm_rust::Availability::SameAsTarget => {
                Err(RoutingError::SourceCapabilityIsVoid {
                    moniker: self.component.moniker.clone(),
                }
                .into())
            }
            cm_rust::Availability::Optional | cm_rust::Availability::Transitional => {
                Ok(Unit {}.into())
            }
        }
    }
}
