// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::router_ext::WeakInstanceTokenExt;
use crate::model::component::WeakComponentInstance;
use ::routing::bedrock::request_metadata::protocol_metadata;
use ::routing::bedrock::sandbox_construction::ComponentSandbox;
use ::routing::DictExt;
use cm_rust::{ExposeDecl, ExposeProtocolDecl, UseDecl, UseProtocolDecl};
use sandbox::{Capability, Dict, Request, Router, WeakInstanceToken};

/// A request to route a capability through the bedrock layer from use.
#[derive(Clone, Debug)]
pub enum UseRouteRequest {
    UseProtocol(UseProtocolDecl),
}

impl TryFrom<UseDecl> for UseRouteRequest {
    type Error = UseDecl;

    fn try_from(decl: UseDecl) -> Result<Self, Self::Error> {
        match decl {
            UseDecl::Protocol(decl) => Ok(Self::UseProtocol(decl)),
            decl => Err(decl),
        }
    }
}

impl UseRouteRequest {
    pub fn into_router(
        self,
        target: WeakComponentInstance,
        program_input_dict: &Dict,
        debug: bool,
    ) -> (Router, Request) {
        match self {
            Self::UseProtocol(decl) => {
                let request = Request {
                    availability: decl.availability,
                    target: WeakInstanceToken::new_component(target.clone()),
                    debug,
                    metadata: protocol_metadata(),
                };
                let Some(capability) = program_input_dict.get_capability(&decl.target_path) else {
                    panic!(
                        "router for capability {:?} is missing from program input dictionary for \
                         component {}",
                        decl.target_path, target.moniker
                    );
                };
                let Capability::Router(router) = capability else {
                    panic!(
                        "program input dictionary for component {} had an entry with an unexpected \
                                 type: {:?}",
                        target.moniker, capability
                    );
                };
                (router, request)
            }
        }
    }

    pub fn availability(&self) -> cm_rust::Availability {
        match self {
            Self::UseProtocol(p) => p.availability,
        }
    }
}

/// A request to route a capability through the bedrock layer from use or expose.
#[derive(Clone, Debug)]
pub enum RouteRequest {
    Use(UseRouteRequest),
    ExposeProtocol(ExposeProtocolDecl),
}

impl TryFrom<UseDecl> for RouteRequest {
    type Error = UseDecl;

    fn try_from(decl: UseDecl) -> Result<Self, Self::Error> {
        Ok(Self::Use(decl.try_into()?))
    }
}

impl TryFrom<&Vec<&ExposeDecl>> for RouteRequest {
    type Error = ();

    fn try_from(exposes: &Vec<&ExposeDecl>) -> Result<Self, Self::Error> {
        if exposes.len() == 1 {
            match exposes[0] {
                ExposeDecl::Protocol(decl) => Ok(Self::ExposeProtocol(decl.clone())),
                _ => Err(()),
            }
        } else {
            Err(())
        }
    }
}

impl RouteRequest {
    pub fn into_router(
        self,
        target: WeakComponentInstance,
        sandbox: &ComponentSandbox,
        debug: bool,
    ) -> (Router, Request) {
        match self {
            Self::Use(r) => r.into_router(target, &sandbox.program_input.namespace, debug),
            Self::ExposeProtocol(decl) => {
                let request = Request {
                    availability: decl.availability,
                    target: WeakInstanceToken::new_component(target.clone()),
                    debug,
                    metadata: protocol_metadata(),
                };
                let Some(capability) =
                    sandbox.component_output_dict.get_capability(&decl.target_name)
                else {
                    panic!(
                        "router for capability {:?} is missing from component output dictionary for \
                         component {}",
                        decl.target_name, target.moniker
                    );
                };
                let Capability::Router(router) = capability else {
                    panic!(
                        "program input dictionary for component {} had an entry with an unexpected \
                                 type: {:?}",
                        target.moniker, capability
                    );
                };
                (router, request)
            }
        }
    }

    pub fn availability(&self) -> cm_rust::Availability {
        match self {
            Self::Use(u) => u.availability(),
            Self::ExposeProtocol(p) => p.availability,
        }
    }
}
