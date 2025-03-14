// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::{
    ComponentInstanceInterface, ExtendedInstanceInterface, WeakExtendedInstanceInterface,
};
use crate::error::ComponentInstanceError;
use cm_rust::{RegistrationDeclCommon, RegistrationSource, RunnerRegistration, SourceName};
use cm_types::{Name, Url};
use fidl_fuchsia_component_decl as fdecl;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Environment<Component: ComponentInstanceInterface> {
    /// Name of this environment as defined by its creator.
    /// Would be `None` for root environment.
    name: Option<Name>,
    /// The parent that created or inherited the environment.
    parent: WeakExtendedInstanceInterface<Component>,
    /// The extension mode of this environment.
    extends: EnvironmentExtends,
    /// The runners available in this environment.
    runner_registry: RunnerRegistry,
    /// Protocols available in this environment as debug capabilities.
    debug_registry: DebugRegistry,
}

impl<C: ComponentInstanceInterface> Environment<C> {
    pub fn new(
        name: Option<Name>,
        parent: WeakExtendedInstanceInterface<C>,
        extends: EnvironmentExtends,
        runner_registry: RunnerRegistry,
        debug_registry: DebugRegistry,
    ) -> Self {
        Self { name, parent, extends, runner_registry, debug_registry }
    }

    /// The name of this environment as defined by its creator.
    /// Should be `None` for the root environment.
    pub fn name(&self) -> Option<&Name> {
        self.name.as_ref()
    }

    /// The parent component instance or top instance that created or inherited the environment.
    pub fn parent(&self) -> &WeakExtendedInstanceInterface<C> {
        &self.parent
    }

    /// The relationship of this environment to that of the component instance's parent.
    pub fn extends(&self) -> &EnvironmentExtends {
        &self.extends
    }

    /// The runners available in this environment.
    pub fn runner_registry(&self) -> &RunnerRegistry {
        &self.runner_registry
    }

    /// Protocols avaliable in this environment as debug capabilities.
    pub fn debug_registry(&self) -> &DebugRegistry {
        &self.debug_registry
    }

    /// Returns the runner registered to `name` and the component that created the environment the
    /// runner was registered to. Returns `None` if there was no match.
    pub fn get_registered_runner(
        &self,
        name: &Name,
    ) -> Result<Option<(ExtendedInstanceInterface<C>, RunnerRegistration)>, ComponentInstanceError>
    {
        let parent = self.parent().upgrade()?;
        match self.runner_registry().get_runner(name) {
            Some(reg) => Ok(Some((parent, reg.clone()))),
            None => match self.extends() {
                EnvironmentExtends::Realm => match parent {
                    ExtendedInstanceInterface::<C>::Component(parent) => {
                        parent.environment().get_registered_runner(name)
                    }
                    ExtendedInstanceInterface::<C>::AboveRoot(_) => {
                        unreachable!("root env can't extend")
                    }
                },
                EnvironmentExtends::None => {
                    return Ok(None);
                }
            },
        }
    }

    /// Returns the debug capability registered to `name`, the realm that created the environment
    /// and the capability was registered to (`None` for component manager's realm) and name of the
    /// environment that registered the capability. Returns `None` if there was no match.
    pub fn get_debug_capability(
        &self,
        name: &Name,
    ) -> Result<
        Option<(ExtendedInstanceInterface<C>, Option<Name>, DebugRegistration)>,
        ComponentInstanceError,
    > {
        let parent = self.parent().upgrade()?;
        match self.debug_registry().get_capability(name) {
            Some(reg) => Ok(Some((parent, self.name().cloned(), reg.clone()))),
            None => match self.extends() {
                EnvironmentExtends::Realm => match parent {
                    ExtendedInstanceInterface::<C>::Component(parent) => {
                        parent.environment().get_debug_capability(name)
                    }
                    ExtendedInstanceInterface::<C>::AboveRoot(_) => {
                        unreachable!("root env can't extend")
                    }
                },
                EnvironmentExtends::None => {
                    return Ok(None);
                }
            },
        }
    }
}

/// How this environment extends its parent's.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentExtends {
    /// This environment extends the environment of its parent's.
    Realm,
    /// This environment was created from scratch.
    None,
}

impl From<fdecl::EnvironmentExtends> for EnvironmentExtends {
    fn from(e: fdecl::EnvironmentExtends) -> Self {
        match e {
            fdecl::EnvironmentExtends::Realm => Self::Realm,
            fdecl::EnvironmentExtends::None => Self::None,
        }
    }
}

/// The set of runners available in a realm's environment.
///
/// [`RunnerRegistration`]: fidl_fuchsia_sys2::RunnerRegistration
#[derive(Clone, Debug)]
pub struct RunnerRegistry {
    runners: HashMap<Name, RunnerRegistration>,
}

impl RunnerRegistry {
    pub fn default() -> Self {
        Self { runners: HashMap::new() }
    }

    pub fn new(runners: HashMap<Name, RunnerRegistration>) -> Self {
        Self { runners }
    }

    pub fn from_decl(regs: &Vec<RunnerRegistration>) -> Self {
        let mut runners = HashMap::new();
        for reg in regs {
            runners.insert(reg.target_name.clone(), reg.clone());
        }
        Self { runners }
    }
    pub fn get_runner(&self, name: &Name) -> Option<&RunnerRegistration> {
        self.runners.get(name)
    }
}

/// The set of debug capabilities available in this environment.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct DebugRegistry {
    pub debug_capabilities: HashMap<Name, DebugRegistration>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugRegistration {
    pub source: RegistrationSource,
    pub source_name: Name,
}

impl SourceName for DebugRegistration {
    fn source_name(&self) -> &Name {
        &self.source_name
    }
}

impl RegistrationDeclCommon for DebugRegistration {
    const TYPE: &'static str = "protocol";

    fn source(&self) -> &RegistrationSource {
        &self.source
    }
}

impl From<Vec<cm_rust::DebugRegistration>> for DebugRegistry {
    fn from(regs: Vec<cm_rust::DebugRegistration>) -> Self {
        let mut debug_capabilities = HashMap::new();
        for reg in regs {
            match reg {
                cm_rust::DebugRegistration::Protocol(r) => {
                    debug_capabilities.insert(
                        r.target_name,
                        DebugRegistration { source_name: r.source_name, source: r.source },
                    );
                }
            };
        }
        Self { debug_capabilities }
    }
}

impl DebugRegistry {
    pub fn get_capability(&self, name: &Name) -> Option<&DebugRegistration> {
        self.debug_capabilities.get(name)
    }
}

pub fn find_first_absolute_ancestor_url<C: ComponentInstanceInterface>(
    component: &Arc<C>,
) -> Result<Url, ComponentInstanceError> {
    let mut parent = component.try_get_parent()?;
    loop {
        match parent {
            ExtendedInstanceInterface::Component(parent_component) => {
                if !parent_component.url().is_relative() {
                    return Ok(parent_component.url().clone());
                }
                parent = parent_component.try_get_parent()?;
            }
            ExtendedInstanceInterface::AboveRoot(_) => {
                return Err(ComponentInstanceError::NoAbsoluteUrl {
                    url: component.url().to_string(),
                    moniker: component.moniker().clone(),
                });
            }
        }
    }
}
