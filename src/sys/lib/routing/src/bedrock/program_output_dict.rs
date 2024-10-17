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
use sandbox::{Capability, Dict, Request, Routable, Router};
use std::sync::Arc;
use tracing::warn;

pub type ProgramRouterFn<C> =
    dyn Fn(WeakComponentInstanceInterface<C>, Path, ComponentCapability) -> Router;
pub type OutgoingDirRouterFn<C> =
    dyn Fn(&Arc<C>, &cm_rust::ComponentDecl, &cm_rust::CapabilityDecl) -> Router;

pub fn build_program_output_dictionary<C: ComponentInstanceInterface + 'static>(
    component: &Arc<C>,
    decl: &cm_rust::ComponentDecl,
    // This router should forward routing requests to a component's program
    new_program_router: &ProgramRouterFn<C>,
    new_outgoing_dir_router: &OutgoingDirRouterFn<C>,
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
            new_program_router,
            new_outgoing_dir_router,
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
    new_program_router: &ProgramRouterFn<C>,
    new_outgoing_dir_router: &OutgoingDirRouterFn<C>,
) {
    match capability {
        cm_rust::CapabilityDecl::Service(_)
        | cm_rust::CapabilityDecl::Protocol(_)
        | cm_rust::CapabilityDecl::Directory(_)
        | cm_rust::CapabilityDecl::Runner(_)
        | cm_rust::CapabilityDecl::Resolver(_) => {
            let router = new_outgoing_dir_router(component, decl, capability);
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
                new_program_router,
            );
        }
        cm_rust::CapabilityDecl::Config(c) => {
            let data: sandbox::Capability =
                sandbox::Data::Bytes(fidl::persist(&c.value.clone().native_into_fidl()).unwrap())
                    .into();
            match program_output_dict.insert_capability(capability.name(), Router::new(data).into())
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
    new_program_router: &ProgramRouterFn<C>,
) {
    let router;
    let declared_dict;
    if let Some(source_path) = decl.source_path.as_ref() {
        // Dictionary backed by program's outgoing directory.
        router = new_program_router(
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

/// Makes a [Router] that always returns the given dictionary.
fn make_simple_dict_router<C: ComponentInstanceInterface + 'static>(
    dict: Dict,
    component: &Arc<C>,
    decl: &cm_rust::DictionaryDecl,
) -> Router {
    struct DictRouter {
        dict: Dict,
        source: CapabilitySource,
    }
    #[async_trait]
    impl Routable for DictRouter {
        async fn route(
            &self,
            _request: Option<Request>,
            debug: bool,
        ) -> Result<Capability, RouterError> {
            if debug {
                Ok(self
                    .source
                    .clone()
                    .try_into()
                    .expect("failed to convert capability source to dictionary"))
            } else {
                Ok(self.dict.clone().into())
            }
        }
    }
    let source = CapabilitySource::Component(ComponentSource {
        capability: ComponentCapability::Dictionary(decl.clone()),
        moniker: component.moniker().clone(),
    });
    Router::new(DictRouter { dict, source })
}
