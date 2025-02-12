// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::WeakComponentInstance;
use ::routing::bedrock::sandbox_construction::{ComponentSandbox, ProgramInput};
use ::routing::DictExt;
use cm_rust::{
    ExposeDecl, ExposeDictionaryDecl, ExposeProtocolDecl, ExposeRunnerDecl, UseDecl,
    UseProtocolDecl, UseRunnerDecl,
};
use sandbox::Capability;

/// A request to route a capability through the bedrock layer from use.
#[derive(Clone, Debug)]
pub enum UseRouteRequest {
    UseProtocol(UseProtocolDecl),
    // decl is never used outside Debug, but include it for consistency
    #[allow(dead_code)]
    UseRunner(UseRunnerDecl),
}

impl TryFrom<UseDecl> for UseRouteRequest {
    type Error = UseDecl;

    fn try_from(decl: UseDecl) -> Result<Self, Self::Error> {
        match decl {
            UseDecl::Protocol(decl) => Ok(Self::UseProtocol(decl)),
            UseDecl::Runner(decl) => Ok(Self::UseRunner(decl)),
            decl => Err(decl),
        }
    }
}

impl UseRouteRequest {
    pub fn into_router(
        self,
        target: WeakComponentInstance,
        program_input: &ProgramInput,
    ) -> Capability {
        match self {
            Self::UseProtocol(decl) => {
                let Some(capability) = program_input.namespace().get_capability(&decl.target_path)
                else {
                    panic!(
                        "router for capability {:?} is missing from program input dictionary for \
                         component {}",
                        decl.target_path, target.moniker
                    );
                };
                let Capability::ConnectorRouter(_) = &capability else {
                    panic!(
                        "program input dictionary for component {} had an entry with an unexpected \
                         type: {:?}",
                        target.moniker, capability
                    );
                };
                capability
            }
            Self::UseRunner(_) => {
                // A component can only use one runner, it must be this one.
                let router = program_input
                    .runner()
                    .expect("component has `use runner` but no runner in program input?");
                router.into()
            }
        }
    }

    pub fn availability(&self) -> cm_rust::Availability {
        match self {
            Self::UseProtocol(p) => p.availability,
            Self::UseRunner(_) => cm_rust::Availability::Required,
        }
    }
}

/// A request to route a capability through the bedrock layer from expose.
#[derive(Clone, Debug)]
pub enum ExposeRouteRequest {
    ExposeProtocol(ExposeProtocolDecl),
    ExposeDictionary(ExposeDictionaryDecl),
    ExposeRunner(ExposeRunnerDecl),
}

impl TryFrom<&Vec<&ExposeDecl>> for ExposeRouteRequest {
    type Error = ();

    fn try_from(exposes: &Vec<&ExposeDecl>) -> Result<Self, Self::Error> {
        if exposes.len() == 1 {
            match exposes[0] {
                ExposeDecl::Protocol(decl) => Ok(Self::ExposeProtocol(decl.clone())),
                ExposeDecl::Dictionary(decl) => Ok(Self::ExposeDictionary(decl.clone())),
                ExposeDecl::Runner(decl) => Ok(Self::ExposeRunner(decl.clone())),
                _ => Err(()),
            }
        } else {
            Err(())
        }
    }
}

impl ExposeRouteRequest {
    pub fn into_router(
        self,
        target: WeakComponentInstance,
        sandbox: &ComponentSandbox,
    ) -> Capability {
        match self {
            Self::ExposeProtocol(decl) => {
                let Some(capability) =
                    sandbox.component_output.capabilities().get_capability(&decl.target_name)
                else {
                    panic!(
                        "router for capability {:?} is missing from component output dictionary for \
                         component {}",
                        decl.target_name, target.moniker
                    );
                };
                let Capability::ConnectorRouter(_) = &capability else {
                    panic!(
                        "program input dictionary for component {} had an entry with an unexpected \
                                 type: {:?}",
                        target.moniker, capability
                    );
                };
                capability
            }
            Self::ExposeDictionary(decl) => {
                let Some(capability) =
                    sandbox.component_output.capabilities().get_capability(&decl.target_name)
                else {
                    panic!(
                        "router for capability {:?} is missing from component output dictionary for \
                         component {}",
                        decl.target_name, target.moniker
                    );
                };
                let Capability::DictionaryRouter(_) = &capability else {
                    panic!(
                        "program input dictionary for component {} had an entry with an unexpected \
                                 type: {:?}",
                        target.moniker, capability
                    );
                };
                capability
            }
            Self::ExposeRunner(decl) => {
                let Some(capability) =
                    sandbox.component_output.capabilities().get_capability(&decl.target_name)
                else {
                    panic!(
                        "router for capability {:?} is missing from component output dictionary for \
                         component {}",
                        decl.target_name, target.moniker
                    );
                };
                let Capability::ConnectorRouter(_) = &capability else {
                    panic!(
                        "program input dictionary for component {} had an entry with an unexpected \
                                 type: {:?}",
                        target.moniker, capability
                    );
                };
                capability
            }
        }
    }

    pub fn availability(&self) -> cm_rust::Availability {
        match self {
            Self::ExposeProtocol(p) => p.availability,
            Self::ExposeDictionary(d) => d.availability,
            Self::ExposeRunner(_) => cm_rust::Availability::Required,
        }
    }
}

/// A request to route a capability through the bedrock layer from use or expose.
#[derive(Clone, Debug)]
pub enum RouteRequest {
    Use(UseRouteRequest),
    Expose(ExposeRouteRequest),
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
        Ok(Self::Expose(exposes.try_into()?))
    }
}

impl RouteRequest {
    pub fn into_router(
        self,
        target: WeakComponentInstance,
        sandbox: &ComponentSandbox,
    ) -> Capability {
        match self {
            Self::Use(r) => r.into_router(target, &sandbox.program_input),
            Self::Expose(r) => r.into_router(target, sandbox),
        }
    }

    pub fn availability(&self) -> cm_rust::Availability {
        match self {
            Self::Use(u) => u.availability(),
            Self::Expose(e) => e.availability(),
        }
    }
}
