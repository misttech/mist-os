// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::router_ext::WeakComponentTokenExt,
    crate::model::component::WeakComponentInstance,
    ::routing::DictExt,
    cm_rust::{UseDecl, UseProtocolDecl},
    sandbox::{Capability, Dict, Request, Router, WeakComponentToken},
};

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
    ) -> (Router, Request) {
        match self {
            Self::UseProtocol(decl) => {
                let request = Request {
                    availability: decl.availability,
                    target: WeakComponentToken::new_component(target.clone()),
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
}
