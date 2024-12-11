// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::with_policy_check::WithPolicyCheck;
use crate::capability_source::{CapabilitySource, ComponentCapability, ComponentSource};
use crate::component_instance::{ComponentInstanceInterface, WeakComponentInstanceInterface};
use crate::DictExt;
use async_trait::async_trait;
use cm_rust::NativeIntoFidl;
use cm_types::Path;
use router_error::RouterError;
use sandbox::{Connector, Data, Dict, DirEntry, Request, Routable, Router, RouterResponse};
use std::sync::Arc;
use tracing::warn;

pub trait ProgramOutputGenerator<C: ComponentInstanceInterface + 'static> {
    /// Get a router for [Dict] that forwards the request to a [Router] served at `path`
    /// in the program's outgoing directory.
    fn new_program_dictionary_router(
        &self,
        component: WeakComponentInstanceInterface<C>,
        path: Path,
        capability: ComponentCapability,
    ) -> Router<Dict>;

    /// Get an outgoing directory router for `capability` that returns [Connector]. `capability`
    /// should be a type that maps to [Connector].
    fn new_outgoing_dir_connector_router(
        &self,
        component: &Arc<C>,
        decl: &cm_rust::ComponentDecl,
        capability: &cm_rust::CapabilityDecl,
    ) -> Router<Connector>;

    /// Get an outgoing directory router for `capability` that returns [DirEntry]. `capability`
    /// should be a type that maps to [DirEntry].
    fn new_outgoing_dir_dir_entry_router(
        &self,
        component: &Arc<C>,
        decl: &cm_rust::ComponentDecl,
        capability: &cm_rust::CapabilityDecl,
    ) -> Router<DirEntry>;

    /// Get an outgoing directory router for `capability` that returns [Dict]. `capability`
    /// should be a type that maps to [Dict].
    fn new_outgoing_dir_dictionary_router(
        &self,
        component: &Arc<C>,
        decl: &cm_rust::ComponentDecl,
        capability: &cm_rust::CapabilityDecl,
    ) -> Router<Dict>;
}

pub fn build_program_output_dictionary<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    decl: &cm_rust::ComponentDecl,
    router_gen: &impl ProgramOutputGenerator<C>,
) -> (Dict, Dict) {
    let program_output_dict = Dict::new();
    let declared_dictionaries = Dict::new();
    for capability in &decl.capabilities {
        extend_dict_with_capability(
            component,
            decl,
            capability,
            &program_output_dict,
            &declared_dictionaries,
            router_gen,
        );
    }
    (program_output_dict, declared_dictionaries)
}

/// Adds `capability` to the program output dict given the resolved `decl`. The program output dict
/// is a dict of routers, keyed by capability name.
fn extend_dict_with_capability<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    decl: &cm_rust::ComponentDecl,
    capability: &cm_rust::CapabilityDecl,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
    router_gen: &impl ProgramOutputGenerator<C>,
) {
    match capability {
        cm_rust::CapabilityDecl::Service(_) => {
            let router = router_gen.new_outgoing_dir_dictionary_router(component, decl, capability);
            let router = router.with_policy_check::<C>(
                CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::from(capability.clone()),
                    moniker: component.moniker().clone(),
                }),
                component.policy_checker().clone(),
            );
            match program_output_dict.insert_capability(capability.name(), router.into()) {
                Ok(()) => (),
                Err(e) => {
                    warn!("failed to add {} to program output dict: {e:?}", capability.name())
                }
            }
        }
        cm_rust::CapabilityDecl::Directory(_) => {
            let router = router_gen.new_outgoing_dir_dir_entry_router(component, decl, capability);
            let router = router.with_policy_check::<C>(
                CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::from(capability.clone()),
                    moniker: component.moniker().clone(),
                }),
                component.policy_checker().clone(),
            );
            match program_output_dict.insert_capability(capability.name(), router.into()) {
                Ok(()) => (),
                Err(e) => {
                    warn!("failed to add {} to program output dict: {e:?}", capability.name())
                }
            }
        }
        cm_rust::CapabilityDecl::Protocol(_)
        | cm_rust::CapabilityDecl::Runner(_)
        | cm_rust::CapabilityDecl::Resolver(_) => {
            let router = router_gen.new_outgoing_dir_connector_router(component, decl, capability);
            let router = router.with_policy_check::<C>(
                CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::from(capability.clone()),
                    moniker: component.moniker().clone(),
                }),
                component.policy_checker().clone(),
            );
            match program_output_dict.insert_capability(capability.name(), router.into()) {
                Ok(()) => (),
                Err(e) => {
                    warn!("failed to add {} to program output dict: {e:?}", capability.name())
                }
            }
        }
        cm_rust::CapabilityDecl::Dictionary(d) => {
            extend_dict_with_dictionary(
                component,
                d,
                program_output_dict,
                declared_dictionaries,
                router_gen,
            );
        }
        cm_rust::CapabilityDecl::Config(c) => {
            let data =
                sandbox::Data::Bytes(fidl::persist(&c.value.clone().native_into_fidl()).unwrap());
            match program_output_dict
                .insert_capability(capability.name(), Router::<Data>::new_ok(data).into())
            {
                Ok(()) => (),
                Err(e) => {
                    warn!("failed to add {} to program output dict: {e:?}", capability.name())
                }
            }
        }
        cm_rust::CapabilityDecl::EventStream(_) | cm_rust::CapabilityDecl::Storage(_) => {
            // Capabilities not supported in bedrock program output dict yet.
            return;
        }
    }
}

fn extend_dict_with_dictionary<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    decl: &cm_rust::DictionaryDecl,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
    router_gen: &impl ProgramOutputGenerator<C>,
) {
    let router;
    let declared_dict;
    if let Some(source_path) = decl.source_path.as_ref() {
        // Dictionary backed by program's outgoing directory.
        router = router_gen.new_program_dictionary_router(
            component.as_weak(),
            source_path.clone(),
            ComponentCapability::Dictionary(decl.clone()),
        );
        declared_dict = None;
    } else {
        let dict = Dict::new();
        router = make_simple_dict_router(dict.clone(), component, decl);
        declared_dict = Some(dict);
    }
    if let Some(dict) = declared_dict {
        match declared_dictionaries.insert_capability(&decl.name, dict.into()) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {} to declared dicts: {e:?}", decl.name),
        };
    }
    match program_output_dict.insert_capability(&decl.name, router.into()) {
        Ok(()) => (),
        Err(e) => warn!("failed to add {} to program output dict: {e:?}", decl.name),
    }
}

/// Makes a router that always returns the given dictionary.
fn make_simple_dict_router<C: ComponentInstanceInterface + 'static>(
    dict: Dict,
    component: &Arc<C>,
    decl: &cm_rust::DictionaryDecl,
) -> Router<Dict> {
    struct DictRouter {
        dict: Dict,
        source: CapabilitySource,
    }
    #[async_trait]
    impl Routable<Dict> for DictRouter {
        async fn route(
            &self,
            _request: Option<Request>,
            debug: bool,
        ) -> Result<RouterResponse<Dict>, RouterError> {
            if debug {
                Ok(RouterResponse::Debug(
                    self.source
                        .clone()
                        .try_into()
                        .expect("failed to convert capability source to dictionary"),
                ))
            } else {
                Ok(RouterResponse::Capability(self.dict.clone().into()))
            }
        }
    }
    let source = CapabilitySource::Component(ComponentSource {
        capability: ComponentCapability::Dictionary(decl.clone()),
        moniker: component.moniker().clone(),
    });
    Router::<Dict>::new(DictRouter { dict, source })
}
