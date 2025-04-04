// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::aggregate_router::{AggregateRouterFn, AggregateSource};
use crate::bedrock::request_metadata::{service_metadata, Metadata, METADATA_KEY_TYPE};
use crate::bedrock::structured_dict::{
    ComponentEnvironment, ComponentInput, ComponentOutput, StructuredDictMap,
};
use crate::bedrock::with_porcelain_type::WithPorcelainType as _;
use crate::bedrock::with_service_renames_and_filter::WithServiceRenamesAndFilter;
use crate::capability_source::{
    AggregateCapability, AggregateInstance, AggregateMember, AnonymizedAggregateSource,
    CapabilitySource, FilteredAggregateProviderSource, InternalCapability, VoidSource,
};
use crate::component_instance::{ComponentInstanceInterface, WeakComponentInstanceInterface};
use crate::error::{ErrorReporter, RouteRequestErrorInfo, RoutingError};
use crate::{DictExt, LazyGet, Sources, WithAvailability, WithDefault, WithErrorReporter};
use async_trait::async_trait;
use cm_rust::{
    CapabilityTypeName, ExposeDeclCommon, OfferDeclCommon, SourceName, SourcePath, UseDeclCommon,
};
use cm_types::{IterablePath, Name, SeparatedPath};
use fidl::endpoints::DiscoverableProtocolMarker;
use itertools::Itertools;
use lazy_static::lazy_static;
use log::warn;
use moniker::{ChildName, Moniker};
use router_error::RouterError;
use sandbox::{
    Capability, CapabilityBound, Connector, Data, Dict, DirEntry, Request, Routable, Router,
    RouterResponse,
};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_sys2 as fsys};

lazy_static! {
    static ref NAMESPACE: Name = "namespace".parse().unwrap();
    static ref RUNNER: Name = "runner".parse().unwrap();
    static ref CONFIG: Name = "config".parse().unwrap();
}

/// All capabilities that are available to a component's program.
#[derive(Debug, Clone)]
pub struct ProgramInput {
    // This will always have the following fields:
    // - namespace: Dict
    // - runner: Option<Router<Connector>>
    // - config: Dict
    inner: Dict,
}

impl Default for ProgramInput {
    fn default() -> Self {
        Self::new(Dict::new(), None, Dict::new())
    }
}

impl ProgramInput {
    pub fn new(namespace: Dict, runner: Option<Router<Connector>>, config: Dict) -> Self {
        let inner = Dict::new();
        inner.insert(NAMESPACE.clone(), namespace.into()).unwrap();
        if let Some(runner) = runner {
            inner.insert(RUNNER.clone(), runner.into()).unwrap();
        }
        inner.insert(CONFIG.clone(), config.into()).unwrap();
        ProgramInput { inner }
    }

    /// All of the capabilities that appear in a program's namespace.
    pub fn namespace(&self) -> Dict {
        let cap = self.inner.get(&*NAMESPACE).expect("capabilities must be cloneable").unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("namespace entry must be a dict: {cap:?}");
        };
        dict
    }

    /// A router for the runner that a component has used (if any).
    pub fn runner(&self) -> Option<Router<Connector>> {
        let cap = self.inner.get(&*RUNNER).expect("capabilities must be cloneable");
        match cap {
            None => None,
            Some(Capability::ConnectorRouter(r)) => Some(r),
            cap => unreachable!("runner entry must be a router: {cap:?}"),
        }
    }

    fn set_runner(&self, capability: Capability) {
        self.inner.insert(RUNNER.clone(), capability).unwrap()
    }

    /// All of the config capabilities that a program will use.
    pub fn config(&self) -> Dict {
        let cap = self.inner.get(&*CONFIG).expect("capabilities must be cloneable").unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("config entry must be a dict: {cap:?}");
        };
        dict
    }
}

/// A component's sandbox holds all the routing dictionaries that a component has once its been
/// resolved.
#[derive(Default, Debug, Clone)]
pub struct ComponentSandbox {
    /// The dictionary containing all capabilities that a component's parent provided to it.
    pub component_input: ComponentInput,

    /// The dictionary containing all capabilities that a component makes available to its parent.
    pub component_output: ComponentOutput,

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
            component_output,
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
            (&component_output.capabilities(), &self.component_output.capabilities()),
            (&component_output.framework(), &self.component_output.framework()),
            (&program_input.namespace(), &self.program_input.namespace()),
            (&program_input.config(), &self.program_input.config()),
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
        if let Some(runner_router) = program_input.runner() {
            self.program_input.set_runner(runner_router.into());
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
    child_component_output_dictionary_routers: HashMap<ChildName, Router<Dict>>,
    decl: &cm_rust::ComponentDecl,
    component_input: ComponentInput,
    program_output_dict: Dict,
    framework_dict: Dict,
    capability_sourced_capabilities_dict: Dict,
    declared_dictionaries: Dict,
    error_reporter: impl ErrorReporter,
    aggregate_router_fn: &AggregateRouterFn<C>,
) -> ComponentSandbox {
    let component_output = ComponentOutput::new();
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
                    &program_output_dict,
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

    for use_ in decl.uses.iter() {
        match use_ {
            cm_rust::UseDecl::Service(_)
                if matches!(use_.source(), cm_rust::UseSource::Collection(_)) =>
            {
                let cm_rust::UseSource::Collection(collection_name) = use_.source() else {
                    unreachable!();
                };
                let aggregate = (aggregate_router_fn)(
                    component.clone(),
                    vec![AggregateSource::Collection { collection_name: collection_name.clone() }],
                    CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
                        capability: AggregateCapability::Service(use_.source_name().clone()),
                        moniker: component.moniker().clone(),
                        members: vec![AggregateMember::try_from(use_).unwrap()],
                        sources: Sources::new(cm_rust::CapabilityTypeName::Service),
                        instances: vec![],
                    }),
                )
                .with_default(Request {
                    metadata: service_metadata(*use_.availability()),
                    target: component.as_weak().into(),
                })
                .with_error_reporter(RouteRequestErrorInfo::from(use_), error_reporter.clone());
                if let Err(e) = program_input
                    .namespace()
                    .insert_capability(use_.path().unwrap(), aggregate.into())
                {
                    warn!("failed to insert {} in program input dict: {e:?}", use_.path().unwrap())
                }
            }
            cm_rust::UseDecl::Service(_) => extend_dict_with_use::<DirEntry, _>(
                component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_input,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                use_,
                error_reporter.clone(),
            ),
            cm_rust::UseDecl::Protocol(_) | cm_rust::UseDecl::Runner(_) => {
                extend_dict_with_use::<Connector, _>(
                    component,
                    &child_component_output_dictionary_routers,
                    &component_input,
                    &program_input,
                    &program_output_dict,
                    &framework_dict,
                    &capability_sourced_capabilities_dict,
                    use_,
                    error_reporter.clone(),
                )
            }
            cm_rust::UseDecl::Config(config) => extend_dict_with_config_use(
                component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_input,
                &program_output_dict,
                config,
                error_reporter.clone(),
            ),
            _ => (),
        }
    }

    // The runner may be specified by either use declaration or in the program section of the
    // manifest. If there's no use declaration for a runner and there is one set in the program
    // section, then let's synthesize a use decl for it and add it to the sandbox.
    if !decl.uses.iter().any(|u| matches!(u, cm_rust::UseDecl::Runner(_))) {
        if let Some(runner_name) = decl.program.as_ref().and_then(|p| p.runner.as_ref()) {
            extend_dict_with_use::<Connector, _>(
                component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_input,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                &cm_rust::UseDecl::Runner(cm_rust::UseRunnerDecl {
                    source: cm_rust::UseSource::Environment,
                    source_name: runner_name.clone(),
                    source_dictionary: Default::default(),
                }),
                error_reporter.clone(),
            );
        }
    }

    for offer_bundle in group_offer_aggregates(&decl.offers).into_iter() {
        let first_offer = offer_bundle.first().unwrap();
        let get_target_dict = || match first_offer.target() {
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
                if collection_inputs.get(&name).is_none() {
                    collection_inputs.insert(name.clone(), Default::default()).ok();
                }
                collection_inputs
                    .get(&name)
                    .expect("collection input was just added")
                    .capabilities()
            }
            cm_rust::OfferTarget::Capability(name) => {
                let dict =
                    match declared_dictionaries.get(&name).expect("dictionaries must be cloneable")
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
        match first_offer {
            cm_rust::OfferDecl::Service(_)
                if offer_bundle.len() == 1
                    && !matches!(first_offer.source(), cm_rust::OfferSource::Collection(_)) =>
            {
                extend_dict_with_offer::<DirEntry, _>(
                    component,
                    &child_component_output_dictionary_routers,
                    &component_input,
                    &program_output_dict,
                    &framework_dict,
                    &capability_sourced_capabilities_dict,
                    first_offer,
                    &(get_target_dict)(),
                    error_reporter.clone(),
                );
            }
            cm_rust::OfferDecl::Service(_) => {
                let aggregate_router = new_aggregate_router_from_service_offers(
                    &offer_bundle,
                    component,
                    &child_component_output_dictionary_routers,
                    &component_input,
                    &program_output_dict,
                    &framework_dict,
                    &capability_sourced_capabilities_dict,
                    error_reporter.clone(),
                    aggregate_router_fn,
                );
                (get_target_dict)()
                    .insert(first_offer.target_name().clone(), aggregate_router.into())
                    .expect("failed to insert capability into target dict")
            }
            cm_rust::OfferDecl::Config(_) => extend_dict_with_offer::<Data, _>(
                component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                first_offer,
                &(get_target_dict)(),
                error_reporter.clone(),
            ),
            cm_rust::OfferDecl::Dictionary(_) => extend_dict_with_offer::<Dict, _>(
                component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                first_offer,
                &(get_target_dict)(),
                error_reporter.clone(),
            ),
            cm_rust::OfferDecl::Protocol(_)
            | cm_rust::OfferDecl::Runner(_)
            | cm_rust::OfferDecl::Resolver(_) => extend_dict_with_offer::<Connector, _>(
                component,
                &child_component_output_dictionary_routers,
                &component_input,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                first_offer,
                &(get_target_dict)(),
                error_reporter.clone(),
            ),
            _ => {}
        }
    }

    for expose_bundle in group_expose_aggregates(&decl.exposes).into_iter() {
        let first_expose = expose_bundle.first().unwrap();
        match first_expose {
            cm_rust::ExposeDecl::Service(_)
                if expose_bundle.len() == 1
                    && !matches!(first_expose.source(), cm_rust::ExposeSource::Collection(_)) =>
            {
                extend_dict_with_expose::<DirEntry, _>(
                    component,
                    &child_component_output_dictionary_routers,
                    &program_output_dict,
                    &framework_dict,
                    &capability_sourced_capabilities_dict,
                    first_expose,
                    &component_output,
                    error_reporter.clone(),
                );
            }
            cm_rust::ExposeDecl::Service(_) => {
                let mut aggregate_sources = vec![];
                let temp_component_output = ComponentOutput::new();
                for expose in expose_bundle.iter() {
                    extend_dict_with_expose::<DirEntry, _>(
                        component,
                        &child_component_output_dictionary_routers,
                        &program_output_dict,
                        &framework_dict,
                        &capability_sourced_capabilities_dict,
                        expose,
                        &temp_component_output,
                        error_reporter.clone(),
                    );
                    match temp_component_output.capabilities().remove(first_expose.target_name()) {
                        Some(Capability::DirEntryRouter(router)) => {
                            let source_instance = match expose.source() {
                                cm_rust::ExposeSource::Self_ => AggregateInstance::Self_,
                                cm_rust::ExposeSource::Child(name) => AggregateInstance::Child(
                                    moniker::ChildName::new(name.clone().to_long(), None),
                                ),
                                other_source => {
                                    warn!(
                                        "unsupported source found in expose aggregate: {:?}",
                                        other_source
                                    );
                                    continue;
                                }
                            };
                            aggregate_sources
                                .push(AggregateSource::DirectoryRouter { source_instance, router })
                        }
                        None => match expose.source() {
                            cm_rust::ExposeSource::Collection(collection_name) => {
                                aggregate_sources.push(AggregateSource::Collection {
                                    collection_name: collection_name.clone(),
                                });
                            }
                            _ => continue,
                        },
                        other_value => panic!("unexpected dictionary entry: {:?}", other_value),
                    }
                }
                let aggregate = (aggregate_router_fn)(
                    component.clone(),
                    aggregate_sources,
                    CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
                        capability: AggregateCapability::Service(
                            first_expose.target_name().clone(),
                        ),
                        moniker: component.moniker().clone(),
                        members: expose_bundle
                            .iter()
                            .filter_map(|e| AggregateMember::try_from(*e).ok())
                            .collect(),
                        sources: Sources::new(cm_rust::CapabilityTypeName::Service),
                        instances: vec![],
                    }),
                )
                .with_default(Request {
                    metadata: service_metadata(*first_expose.availability()),
                    target: component.as_weak().into(),
                });
                component_output
                    .capabilities()
                    .insert(first_expose.target_name().clone(), aggregate.into())
                    .expect("failed to insert capability into target dict")
            }
            cm_rust::ExposeDecl::Config(_) => extend_dict_with_expose::<Data, _>(
                component,
                &child_component_output_dictionary_routers,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                first_expose,
                &component_output,
                error_reporter.clone(),
            ),
            cm_rust::ExposeDecl::Dictionary(_) => extend_dict_with_expose::<Dict, _>(
                component,
                &child_component_output_dictionary_routers,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                first_expose,
                &component_output,
                error_reporter.clone(),
            ),
            cm_rust::ExposeDecl::Protocol(_)
            | cm_rust::ExposeDecl::Runner(_)
            | cm_rust::ExposeDecl::Resolver(_) => extend_dict_with_expose::<Connector, _>(
                component,
                &child_component_output_dictionary_routers,
                &program_output_dict,
                &framework_dict,
                &capability_sourced_capabilities_dict,
                first_expose,
                &component_output,
                error_reporter.clone(),
            ),
            _ => {}
        }
    }

    ComponentSandbox {
        component_input,
        component_output,
        program_input,
        program_output_dict,
        framework_dict,
        capability_sourced_capabilities_dict,
        declared_dictionaries,
        child_inputs,
        collection_inputs,
    }
}

fn new_aggregate_router_from_service_offers<C: ComponentInstanceInterface + 'static>(
    offer_bundle: &Vec<&cm_rust::OfferDecl>,
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    error_reporter: impl ErrorReporter,
    aggregate_router_fn: &AggregateRouterFn<C>,
) -> Router<DirEntry> {
    let mut aggregate_sources = vec![];
    let dict_for_source_router = Dict::new();
    let source = new_aggregate_capability_source(component.moniker().clone(), offer_bundle.clone());
    for offer in offer_bundle.iter() {
        if matches!(&source, &CapabilitySource::FilteredAggregateProvider(_)) {
            if let cm_rust::OfferDecl::Service(offer_service_decl) = offer {
                if offer_service_decl
                    .source_instance_filter
                    .as_ref()
                    .and_then(|v| v.first())
                    .is_none()
                    && offer_service_decl
                        .renamed_instances
                        .as_ref()
                        .and_then(|v| v.first())
                        .is_none()
                {
                    // If we're a filtering aggregate and no filter or renames have been
                    // set, then all instances here are ignored, and there's no point in
                    // including the router in the aggregate.
                    continue;
                }
            }
        }
        extend_dict_with_offer::<DirEntry, _>(
            component,
            &child_component_output_dictionary_routers,
            &component_input,
            &program_output_dict,
            &framework_dict,
            &capability_sourced_capabilities_dict,
            offer,
            &dict_for_source_router,
            error_reporter.clone(),
        );
        match dict_for_source_router.remove(offer.target_name()) {
            Some(Capability::DirEntryRouter(router)) => {
                let source_instance = match offer.source() {
                    cm_rust::OfferSource::Self_ => AggregateInstance::Self_,
                    cm_rust::OfferSource::Parent => AggregateInstance::Parent,
                    cm_rust::OfferSource::Child(child_ref) => {
                        AggregateInstance::Child(moniker::ChildName::new(
                            child_ref.name.clone(),
                            child_ref.collection.clone(),
                        ))
                    }
                    other_source => {
                        warn!("unsupported source found in offer aggregate: {:?}", other_source);
                        continue;
                    }
                };
                aggregate_sources.push(AggregateSource::DirectoryRouter { source_instance, router })
            }
            None => match offer.source() {
                // `extend_dict_with_offer` doesn't insert a capability for offers with a source of
                // `OfferSource::Collection`. This is because at this stage there's nothing in the
                // collection, and thus no routers to things in the collection.
                cm_rust::OfferSource::Collection(collection_name) => {
                    aggregate_sources.push(AggregateSource::Collection {
                        collection_name: collection_name.clone(),
                    });
                }
                _ => continue,
            },
            other => warn!("found unexpected entry in dictionary: {:?}", other),
        }
    }
    (aggregate_router_fn)(component.clone(), aggregate_sources, source).with_default(Request {
        metadata: service_metadata(*offer_bundle.first().unwrap().availability()),
        target: component.as_weak().into(),
    })
}

fn new_aggregate_capability_source(
    moniker: Moniker,
    offers: Vec<&cm_rust::OfferDecl>,
) -> CapabilitySource {
    let offer_service_decls = offers
        .iter()
        .map(|o| match o {
            cm_rust::OfferDecl::Service(o) => o,
            _ => panic!("cannot aggregate non-service capabilities, manifest validation should prevent this"),
        }).collect::<Vec<_>>();
    // This is a filtered offer if any of the offers set a filter or rename mapping.
    let is_filtered_offer = offer_service_decls.iter().any(|o| {
        o.source_instance_filter.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
            || o.renamed_instances.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
    });
    let capability =
        AggregateCapability::Service(offer_service_decls.first().unwrap().target_name.clone());
    if is_filtered_offer {
        CapabilitySource::FilteredAggregateProvider(FilteredAggregateProviderSource {
            capability,
            moniker,
            offer_service_decls: offer_service_decls.into_iter().cloned().collect(),
            sources: Sources::new(cm_rust::CapabilityTypeName::Service).component().collection(),
        })
    } else {
        let members = offers.iter().filter_map(|o| AggregateMember::try_from(*o).ok()).collect();
        CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
            capability,
            moniker,
            members,
            sources: Sources::new(cm_rust::CapabilityTypeName::Service).component().collection(),
            instances: vec![],
        })
    }
}

/// Groups together a set of offers into sub-sets of those that have the same target and target
/// name. This is useful for identifying which offers are part of an aggregation of capabilities,
/// and which are for standalone routes.
fn group_offer_aggregates(offers: &Vec<cm_rust::OfferDecl>) -> Vec<Vec<&cm_rust::OfferDecl>> {
    let mut groupings = HashMap::new();
    for offer in offers.iter() {
        groupings.entry((offer.target(), offer.target_name())).or_insert(vec![]).push(offer);
    }
    groupings.into_iter().map(|(_key, grouping)| grouping).collect()
}

/// Identical to `group_offer_aggregates`, but for exposes.
fn group_expose_aggregates(exposes: &Vec<cm_rust::ExposeDecl>) -> Vec<Vec<&cm_rust::ExposeDecl>> {
    let mut groupings = HashMap::new();
    for expose in exposes.iter() {
        groupings.entry((expose.target(), expose.target_name())).or_insert(vec![]).push(expose);
    }
    groupings.into_iter().map(|(_key, grouping)| grouping).collect()
}

fn build_environment(
    moniker: &Moniker,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    component_input: &ComponentInput,
    environment_decl: &cm_rust::EnvironmentDecl,
    program_output_dict: &Dict,
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
        let router: Router<Connector> = match &source {
            cm_rust::RegistrationSource::Parent => {
                use_from_parent_router::<Connector>(component_input, source_path, moniker)
                    .with_porcelain_type(cap_type, moniker.clone())
            }
            cm_rust::RegistrationSource::Self_ => program_output_dict
                .get_router_or_not_found::<Connector>(
                    &source_path,
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
                let r: Router<Connector> = child_component_output.clone().lazy_get(
                    source_path,
                    RoutingError::use_from_child_expose_not_found(
                        &child_name,
                        &moniker,
                        source_name.clone(),
                    ),
                );
                r.with_porcelain_type(cap_type, moniker.clone())
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
            Err(_e) => {
                // The only reason this will happen is if we're shadowing something else in the
                // environment. `insert_capability` will still insert the new capability when it
                // returns an error, so we can safely ignore this.
            }
        }
    }
    environment
}

/// Extends the given `target_input` to contain the capabilities described in `dynamic_offers`.
pub fn extend_dict_with_offers<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    static_offers: &Vec<cm_rust::OfferDecl>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    component_input: &ComponentInput,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    program_output_dict: &Dict,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    target_input: &ComponentInput,
    error_reporter: impl ErrorReporter,
    aggregate_router_fn: &AggregateRouterFn<C>,
) {
    for offer_bundle in group_offer_aggregates(dynamic_offers).into_iter() {
        let first_offer = offer_bundle.first().unwrap();
        match first_offer {
            cm_rust::OfferDecl::Service(_) => {
                let static_offer_bundles = group_offer_aggregates(static_offers);
                let maybe_static_offer_bundle = static_offer_bundles.into_iter().find(|bundle| {
                    bundle.first().unwrap().target_name() == first_offer.target_name()
                });
                let mut combined_offer_bundle = offer_bundle.clone();
                if let Some(mut static_offer_bundle) = maybe_static_offer_bundle {
                    // We are aggregating together dynamic and static offers, as there are static
                    // offers with the same target name as our current dynamic offers. We already
                    // populated a router for the static bundle in the target input, let's toss
                    // that and generate a new one with the expanded set of offers.
                    let _ = target_input.capabilities().remove(first_offer.target_name());
                    combined_offer_bundle.append(&mut static_offer_bundle);
                }
                if combined_offer_bundle.len() == 1
                    && !matches!(first_offer.source(), cm_rust::OfferSource::Collection(_))
                {
                    extend_dict_with_offer::<DirEntry, _>(
                        component,
                        &child_component_output_dictionary_routers,
                        &component_input,
                        &program_output_dict,
                        &framework_dict,
                        &capability_sourced_capabilities_dict,
                        first_offer,
                        &target_input.capabilities(),
                        error_reporter.clone(),
                    );
                } else {
                    let aggregate_router = new_aggregate_router_from_service_offers(
                        &combined_offer_bundle,
                        component,
                        &child_component_output_dictionary_routers,
                        &component_input,
                        &program_output_dict,
                        &framework_dict,
                        &capability_sourced_capabilities_dict,
                        error_reporter.clone(),
                        aggregate_router_fn,
                    );
                    target_input
                        .capabilities()
                        .insert(first_offer.target_name().clone(), aggregate_router.into())
                        .expect("failed to insert capability into target dict");
                }
            }
            cm_rust::OfferDecl::Config(_) => extend_dict_with_offer::<Data, _>(
                component,
                &child_component_output_dictionary_routers,
                component_input,
                program_output_dict,
                framework_dict,
                capability_sourced_capabilities_dict,
                first_offer,
                &target_input.capabilities(),
                error_reporter.clone(),
            ),
            cm_rust::OfferDecl::Dictionary(_) => extend_dict_with_offer::<Dict, _>(
                component,
                &child_component_output_dictionary_routers,
                component_input,
                program_output_dict,
                framework_dict,
                capability_sourced_capabilities_dict,
                first_offer,
                &target_input.capabilities(),
                error_reporter.clone(),
            ),
            cm_rust::OfferDecl::Protocol(_)
            | cm_rust::OfferDecl::Runner(_)
            | cm_rust::OfferDecl::Resolver(_) => extend_dict_with_offer::<Connector, _>(
                component,
                &child_component_output_dictionary_routers,
                component_input,
                program_output_dict,
                framework_dict,
                capability_sourced_capabilities_dict,
                first_offer,
                &target_input.capabilities(),
                error_reporter.clone(),
            ),
            _ => {}
        }
    }
}

pub fn is_supported_use(use_: &cm_rust::UseDecl) -> bool {
    matches!(
        use_,
        cm_rust::UseDecl::Config(_)
            | cm_rust::UseDecl::Protocol(_)
            | cm_rust::UseDecl::Runner(_)
            | cm_rust::UseDecl::Service(_)
    )
}

// Add the `config_use` to the `program_input_dict`, so the component is able to
// access this configuration.
fn extend_dict_with_config_use<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    component_input: &ComponentInput,
    program_input: &ProgramInput,
    program_output_dict: &Dict,
    config_use: &cm_rust::UseConfigurationDecl,
    error_reporter: impl ErrorReporter,
) {
    let moniker = component.moniker();
    let source_path = config_use.source_path();
    let porcelain_type = CapabilityTypeName::Config;
    let router: Router<Data> = match config_use.source() {
        cm_rust::UseSource::Parent => {
            use_from_parent_router::<Data>(component_input, source_path.to_owned(), moniker)
                .with_porcelain_type(porcelain_type, moniker.clone())
        }
        cm_rust::UseSource::Self_ => program_output_dict
            .get_router_or_not_found::<Data>(
                &source_path,
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
            let r: Router<Data> = child_component_output.clone().lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_child_expose_not_found(
                    &child_name,
                    &moniker,
                    config_use.source_name().clone(),
                ),
            );
            r.with_porcelain_type(porcelain_type, moniker.clone())
        }
        // The following are not used with config capabilities.
        cm_rust::UseSource::Environment => return,
        cm_rust::UseSource::Debug => return,
        cm_rust::UseSource::Capability(_) => return,
        cm_rust::UseSource::Framework => return,
        cm_rust::UseSource::Collection(_) => return,
    };

    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default use metadata?");
    metadata.set_metadata(*config_use.availability());
    let default_request = Request { metadata, target: component.as_weak().into() };
    match program_input.config().insert_capability(
        &config_use.target_name,
        router
            .with_availability(moniker.clone(), *config_use.availability())
            .with_default(default_request)
            .with_error_reporter(RouteRequestErrorInfo::from(config_use), error_reporter)
            .into(),
    ) {
        Ok(()) => (),
        Err(e) => {
            warn!("failed to insert {} in program input dict: {e:?}", config_use.target_name)
        }
    }
}

fn extend_dict_with_use<T, C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    component_input: &ComponentInput,
    program_input: &ProgramInput,
    program_output_dict: &Dict,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    use_: &cm_rust::UseDecl,
    error_reporter: impl ErrorReporter,
) where
    T: CapabilityBound + Clone,
    Router<T>: TryFrom<Capability> + Into<Capability>,
{
    if !is_supported_use(use_) {
        return;
    }
    let moniker = component.moniker();
    if let cm_rust::UseDecl::Config(config) = use_ {
        return extend_dict_with_config_use(
            component,
            child_component_output_dictionary_routers,
            component_input,
            program_input,
            program_output_dict,
            config,
            error_reporter,
        );
    };

    let source_path = use_.source_path();
    let porcelain_type = CapabilityTypeName::from(use_);
    let router: Router<T> = match use_.source() {
        cm_rust::UseSource::Parent => {
            use_from_parent_router::<T>(component_input, source_path.to_owned(), moniker)
                .with_porcelain_type(porcelain_type, moniker.clone())
        }
        cm_rust::UseSource::Self_ => program_output_dict
            .get_router_or_not_found::<T>(
                &source_path,
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
            let r: Router<T> = child_component_output.clone().lazy_get(
                source_path.to_owned(),
                RoutingError::use_from_child_expose_not_found(
                    &child_name,
                    &moniker,
                    use_.source_name().clone(),
                ),
            );
            r.with_porcelain_type(porcelain_type, moniker.clone())
        }
        cm_rust::UseSource::Framework if use_.is_from_dictionary() => {
            Router::<T>::new_error(RoutingError::capability_from_framework_not_found(
                moniker,
                source_path.iter_segments().join("/"),
            ))
        }
        cm_rust::UseSource::Framework => framework_dict
            .get_router_or_not_found::<T>(
                &source_path,
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
                capability_sourced_capabilities_dict.get_router_or_not_found(&capability_name, err)
            } else {
                Router::<T>::new_error(err)
            }
        }
        cm_rust::UseSource::Debug => {
            let cm_rust::UseDecl::Protocol(use_protocol) = use_ else {
                panic!("non-protocol capability used with a debug source, this should be prevented by manifest validation");
            };
            component_input
                .environment()
                .debug()
                .get_router_or_not_found::<T>(
                    &use_protocol.source_name,
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
                .get_router_or_not_found::<T>(
                    &use_runner.source_name,
                    RoutingError::use_from_environment_not_found(
                        moniker,
                        "runner",
                        &use_runner.source_name,
                    ),
                )
                .with_porcelain_type(porcelain_type, moniker.clone())
        }
        cm_rust::UseSource::Collection(_) => {
            // Collection sources are handled separately, in `build_component_sandbox`
            return;
        }
    };
    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default use metadata?");
    metadata.set_metadata(*use_.availability());
    let default_request = Request { metadata, target: component.as_weak().into() };
    let router = router
        .with_availability(moniker.clone(), *use_.availability())
        .with_default(default_request)
        .with_error_reporter(RouteRequestErrorInfo::from(use_), error_reporter);

    if let Some(target_path) = use_.path() {
        if let Err(e) = program_input.namespace().insert_capability(target_path, router.into()) {
            warn!("failed to insert {} in program input dict: {e:?}", target_path)
        }
    } else {
        match use_ {
            cm_rust::UseDecl::Runner(_) => {
                assert!(program_input.runner().is_none(), "component can't use multiple runners");
                program_input.set_runner(router.into());
            }
            _ => panic!("unexpected capability type: {:?}", use_),
        }
    }
}

/// Builds a router that obtains a capability that the program uses from `parent`.
fn use_from_parent_router<T>(
    component_input: &ComponentInput,
    source_path: impl IterablePath + 'static + Debug,
    moniker: &Moniker,
) -> Router<T>
where
    T: CapabilityBound + Clone,
    Router<T>: TryFrom<Capability>,
{
    let err = if moniker == &Moniker::root() {
        RoutingError::register_from_component_manager_not_found(
            source_path.iter_segments().join("/"),
        )
    } else {
        RoutingError::use_from_parent_not_found(moniker, source_path.iter_segments().join("/"))
    };
    component_input.capabilities().get_router_or_not_found::<T>(&source_path, err)
}

fn is_supported_offer(offer: &cm_rust::OfferDecl) -> bool {
    matches!(
        offer,
        cm_rust::OfferDecl::Config(_)
            | cm_rust::OfferDecl::Protocol(_)
            | cm_rust::OfferDecl::Dictionary(_)
            | cm_rust::OfferDecl::Runner(_)
            | cm_rust::OfferDecl::Resolver(_)
            | cm_rust::OfferDecl::Service(_)
    )
}

fn extend_dict_with_offer<T, C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    offer: &cm_rust::OfferDecl,
    target_dict: &Dict,
    error_reporter: impl ErrorReporter,
) where
    T: CapabilityBound + Clone,
    Router<T>: TryFrom<Capability> + Into<Capability> + WithServiceRenamesAndFilter,
{
    assert!(is_supported_offer(offer), "{offer:?}");

    let source_path = offer.source_path();
    let target_name = offer.target_name();
    if target_dict.get_capability(&target_name).is_some() {
        panic!(
            "duplicate sources for {} {} in a dict, unable to populate dict entry, manifest \
                validation should prevent this",
            cm_rust::CapabilityTypeName::from(offer),
            target_name
        );
    }
    let porcelain_type = CapabilityTypeName::from(offer);
    let router: Router<T> = match offer.source() {
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
            let router =
                component_input.capabilities().get_router_or_not_found::<T>(&source_path, err);
            router.with_porcelain_type(porcelain_type, component.moniker().clone())
        }
        cm_rust::OfferSource::Self_ => program_output_dict
            .get_router_or_not_found::<T>(
                &source_path,
                RoutingError::offer_from_self_not_found(
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                ),
            )
            .with_porcelain_type(porcelain_type, component.moniker().clone()),
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            match child_component_output_dictionary_routers.get(&child_name) {
                None => Router::<T>::new_error(RoutingError::offer_from_child_instance_not_found(
                    &child_name,
                    &component.moniker(),
                    source_path.iter_segments().join("/"),
                )),
                Some(child_component_output) => {
                    let router: Router<T> = child_component_output.clone().lazy_get(
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
            let router = framework_dict.get_router_or_not_found::<T>(
                &source_path,
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
                capability_sourced_capabilities_dict.get_router_or_not_found(&capability_name, err)
            } else {
                Router::<T>::new_error(err)
            }
        }
        cm_rust::OfferSource::Void => UnavailableRouter::new::<T>(
            InternalCapability::Protocol(offer.source_name().clone()),
            component,
        ),
        cm_rust::OfferSource::Collection(_collection_name) => {
            // There's nothing in a collection at this stage, and thus we can't get any routers to
            // things in the collection. What's more: the contents of the collection can change
            // over time, so it must be monitored. We don't handle collections here, they're
            // handled in a different way by whoever called `extend_dict_with_offer`.
            return;
        }
    };
    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default offer metadata?");
    metadata.set_metadata(*offer.availability());
    // Offered capabilities need to support default requests in the case of offer-to-dictionary.
    // This is a corollary of the fact that program_input_dictionary and
    // component_output_dictionary support default requests, and we need this to cover the case
    // where the use or expose is from a dictionary.
    //
    // Technically, we could restrict this to the case of offer-to-dictionary, not offer in
    // general. However, supporting the general case simplifies the logic and establishes a nice
    // symmetry between program_input_dict, component_output_dict, and {child,collection}_inputs.
    let default_request = Request { metadata, target: component.as_weak().into() };
    let router = router
        .with_availability(component.moniker().clone(), *offer.availability())
        .with_default(default_request)
        .with_error_reporter(RouteRequestErrorInfo::from(offer), error_reporter)
        .with_service_renames_and_filter(offer.clone());
    match target_dict.insert_capability(target_name, router.into()) {
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
            | cm_rust::ExposeDecl::Service(_)
    )
}

fn extend_dict_with_expose<T, C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    child_component_output_dictionary_routers: &HashMap<ChildName, Router<Dict>>,
    program_output_dict: &Dict,
    framework_dict: &Dict,
    capability_sourced_capabilities_dict: &Dict,
    expose: &cm_rust::ExposeDecl,
    target_component_output: &ComponentOutput,
    error_reporter: impl ErrorReporter,
) where
    T: CapabilityBound + Clone,
    Router<T>: TryFrom<Capability> + Into<Capability>,
{
    assert!(is_supported_expose(expose), "{expose:?}");

    // Exposing to the framework is vestigial
    let target_dict = match expose.target() {
        cm_rust::ExposeTarget::Parent => target_component_output.capabilities(),
        cm_rust::ExposeTarget::Framework => target_component_output.framework(),
    };
    let source_path = expose.source_path();
    let target_name = expose.target_name();

    let porcelain_type = CapabilityTypeName::from(expose);
    let router: Router<T> = match expose.source() {
        cm_rust::ExposeSource::Self_ => program_output_dict
            .get_router_or_not_found::<T>(
                &source_path,
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
                let router: Router<T> = child_component_output.clone().lazy_get(
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
                Router::<T>::new_error(RoutingError::expose_from_child_instance_not_found(
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
                .get_router_or_not_found::<T>(
                    &source_path,
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
                capability_sourced_capabilities_dict
                    .clone()
                    .get_router_or_not_found::<T>(&capability_name, err)
            } else {
                Router::<T>::new_error(err)
            }
        }
        cm_rust::ExposeSource::Void => UnavailableRouter::new(
            InternalCapability::Protocol(expose.source_name().clone()),
            component,
        ),
        // There's nothing in a collection at this stage, and thus we can't get any routers to
        // things in the collection. What's more: the contents of the collection can change over
        // time, so it must be monitored. We don't handle collections here, they're handled in a
        // different way by whoever called `extend_dict_with_expose`.
        cm_rust::ExposeSource::Collection(_name) => return,
    };
    let metadata = Dict::new();
    metadata
        .insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(porcelain_type.to_string())),
        )
        .expect("failed to build default expose metadata?");
    metadata.set_metadata(*expose.availability());
    let default_request = Request { metadata, target: component.as_weak().into() };
    match target_dict.insert_capability(
        target_name,
        router
            .with_availability(component.moniker().clone(), *expose.availability())
            .with_default(default_request)
            .with_error_reporter(RouteRequestErrorInfo::from(expose), error_reporter)
            .into(),
    ) {
        Ok(()) => (),
        Err(e) => warn!("failed to insert {target_name} into target_dict: {e:?}"),
    }
}

struct UnavailableRouter<C: ComponentInstanceInterface> {
    capability: InternalCapability,
    component: WeakComponentInstanceInterface<C>,
}

impl<C: ComponentInstanceInterface + 'static> UnavailableRouter<C> {
    fn new<T: CapabilityBound>(capability: InternalCapability, component: &Arc<C>) -> Router<T> {
        Router::<T>::new(UnavailableRouter { capability, component: component.as_weak() })
    }
}

#[async_trait]
impl<T: CapabilityBound, C: ComponentInstanceInterface + 'static> Routable<T>
    for UnavailableRouter<C>
{
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        if debug {
            let data = CapabilitySource::Void(VoidSource {
                capability: self.capability.clone(),
                moniker: self.component.moniker.clone(),
            })
            .try_into()
            .expect("failed to convert capability source to Data");
            return Ok(RouterResponse::<T>::Debug(data));
        }
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let availability = request.metadata.get_metadata().ok_or(RouterError::InvalidArgs)?;
        match availability {
            cm_rust::Availability::Required | cm_rust::Availability::SameAsTarget => {
                Err(RoutingError::SourceCapabilityIsVoid {
                    moniker: self.component.moniker.clone(),
                }
                .into())
            }
            cm_rust::Availability::Optional | cm_rust::Availability::Transitional => {
                Ok(RouterResponse::Unavailable)
            }
        }
    }
}
