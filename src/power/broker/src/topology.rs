// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Manages the Power Element Topology, keeping track of element dependencies.
use fidl_fuchsia_power_broker::{self as fpb, PowerLevel};
use std::collections::HashMap;
use uuid::Uuid;

use crate::credentials::*;

// This may be a token later, but using a String for now for simplicity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct ElementID {
    id: String,
}

impl From<&str> for ElementID {
    fn from(s: &str) -> Self {
        ElementID { id: s.into() }
    }
}

impl From<String> for ElementID {
    fn from(s: String) -> Self {
        ElementID { id: s }
    }
}

impl Into<String> for ElementID {
    fn into(self) -> String {
        self.id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct ElementLevel {
    pub element: ElementID,
    pub level: PowerLevel,
}

impl From<fpb::ElementLevel> for ElementLevel {
    fn from(el: fpb::ElementLevel) -> Self {
        ElementLevel { element: el.element.into(), level: el.level.into() }
    }
}

/// Power dependency from one element's PowerLevel to another.
/// The Element and PowerLevel specified by `level` depends on
/// the Element and PowerLevel specified by `requires`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Dependency {
    pub level: ElementLevel,
    pub requires: ElementLevel,
}

impl From<fpb::Dependency> for Dependency {
    fn from(d: fpb::Dependency) -> Self {
        Dependency { level: d.level.into(), requires: d.requires.into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    id: ElementID,
    name: String,
}

#[derive(Debug)]
pub enum AddElementError {
    Internal,
}

impl Into<fpb::AddElementError> for AddElementError {
    fn into(self) -> fpb::AddElementError {
        match self {
            AddElementError::Internal => fpb::AddElementError::Internal,
        }
    }
}

#[derive(Debug)]
pub enum RemoveElementError {
    NotFound(ElementID),
}

impl Into<fpb::RemoveElementError> for RemoveElementError {
    fn into(self) -> fpb::RemoveElementError {
        match self {
            RemoveElementError::NotFound(_) => fpb::RemoveElementError::NotFound,
        }
    }
}

#[derive(Debug)]
pub enum AddDependencyError {
    AlreadyExists(),
    ElementNotFound(ElementID),
    RequiredElementNotFound(ElementID),
}

impl Into<fpb::AddDependencyError> for AddDependencyError {
    fn into(self) -> fpb::AddDependencyError {
        match self {
            AddDependencyError::AlreadyExists() => fpb::AddDependencyError::AlreadyExists,
            AddDependencyError::ElementNotFound(_) => fpb::AddDependencyError::ElementNotFound,
            AddDependencyError::RequiredElementNotFound(_) => {
                fpb::AddDependencyError::RequiredElementNotFound
            }
        }
    }
}

#[derive(Debug)]
pub enum RemoveDependencyError {
    NotFound(Dependency),
}

impl Into<fpb::RemoveDependencyError> for RemoveDependencyError {
    fn into(self) -> fpb::RemoveDependencyError {
        match self {
            RemoveDependencyError::NotFound(_) => fpb::RemoveDependencyError::NotFound,
        }
    }
}

#[derive(Debug)]
pub struct Topology {
    elements: HashMap<ElementID, Element>,
    credentials: Registry,
    source_to_targets_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
}

impl Topology {
    pub fn new() -> Self {
        Topology {
            elements: HashMap::new(),
            credentials: Registry::new(),
            source_to_targets_dependencies: HashMap::new(),
        }
    }

    pub fn add_element(
        &mut self,
        name: &str,
        _dependencies: Vec<Dependency>,
        credentials_to_register: Vec<CredentialToRegister>,
    ) -> Result<ElementID, AddElementError> {
        let id = ElementID::from(Uuid::new_v4().as_simple().to_string());
        self.elements.insert(id.clone(), Element { id: id.clone(), name: name.into() });
        for credential_to_register in credentials_to_register {
            if let Err(err) = self.credentials.register(credential_to_register) {
                match err {
                    RegisterCredentialsError::Internal => {
                        tracing::error!(
                            "credentials.register returned an Internal error: {:?}",
                            id
                        );
                        return Err(AddElementError::Internal);
                    }
                    // Owner should be authorized for all credentials on its
                    // own element, so this is an Internal error:
                    RegisterCredentialsError::NotAuthorized => {
                        tracing::error!("credentials.register unexpectedly returned NotAuthorized in add_element: {:?}", id);
                        return Err(AddElementError::Internal);
                    }
                }
            }
        }
        Ok(id)
    }

    pub fn remove_element(&mut self, id: &ElementID) -> Result<(), RemoveElementError> {
        if self.elements.remove(id).is_none() {
            return Err(RemoveElementError::NotFound(id.clone()));
        }
        self.credentials.unregister_all_for_element(id);
        Ok(())
    }

    /// Gets direct dependencies for the given Element and PowerLevel.
    pub fn get_direct_deps(&self, element_level: &ElementLevel) -> Vec<Dependency> {
        let targets = self
            .source_to_targets_dependencies
            .get(&element_level)
            .unwrap_or(&Vec::<ElementLevel>::new())
            .clone();
        targets
            .iter()
            .map(|target| Dependency { level: element_level.clone(), requires: target.clone() })
            .collect()
    }

    /// Gets all direct and transitive dependencies for the given Element and
    /// PowerLevel.
    pub fn get_all_deps(&self, element_level: &ElementLevel) -> Vec<Dependency> {
        let mut deps = Vec::<Dependency>::new();
        let mut element_levels = vec![element_level.clone()];
        while let Some(source) = element_levels.pop() {
            let direct_deps = self.get_direct_deps(&source);
            for dep in direct_deps {
                element_levels.push(dep.requires.clone());
                deps.push(dep);
            }
        }
        deps
    }

    /// Adds a direct dependency to the Topology.
    pub fn add_direct_dep(&mut self, dep: &Dependency) -> Result<(), AddDependencyError> {
        if !self.elements.contains_key(&dep.level.element) {
            return Err(AddDependencyError::ElementNotFound(dep.level.element.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element) {
            return Err(AddDependencyError::RequiredElementNotFound(dep.requires.element.clone()));
        }
        // TODO(b/299463665): Add Dependency validation here, or in Dependency construction.
        let targets =
            self.source_to_targets_dependencies.entry(dep.level.clone()).or_insert(Vec::new());
        if targets.contains(&dep.requires) {
            return Err(AddDependencyError::AlreadyExists());
        }
        targets.push(dep.requires.clone());
        Ok(())
    }

    /// Removes a direct dependency from the Topology.
    pub fn remove_direct_dep(&mut self, dep: &Dependency) -> Result<(), RemoveDependencyError> {
        if !self.elements.contains_key(&dep.level.element) {
            return Err(RemoveDependencyError::NotFound(dep.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element) {
            return Err(RemoveDependencyError::NotFound(dep.clone()));
        }
        let targets =
            self.source_to_targets_dependencies.entry(dep.level.clone()).or_insert(Vec::new());
        if !targets.contains(&dep.requires) {
            return Err(RemoveDependencyError::NotFound(dep.clone()));
        }
        targets.retain(|el| el != &dep.requires);
        Ok(())
    }

    pub fn register_credentials(
        &mut self,
        _token: Token,
        credentials_to_register: Vec<CredentialToRegister>,
    ) -> Result<(), RegisterCredentialsError> {
        // TODO(b/308199278): Validate token against credentials_to_register
        for credential_to_register in credentials_to_register {
            if let Err(err) = self.credentials.register(credential_to_register) {
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn unregister_credentials(
        &mut self,
        _token: Token,
        tokens_to_unregister: Vec<Token>,
    ) -> Result<(), UnregisterCredentialsError> {
        // TODO(b/308199278): Validate token against tokens_to_unregister
        for token in tokens_to_unregister {
            let Some(credential) = self.credentials.lookup(token) else {
                continue;
            };
            self.credentials.unregister(&credential);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::{BinaryPowerLevel, PowerLevel};

    #[fuchsia::test]
    fn test_add_remove_elements() {
        let mut t = Topology::new();
        let water = t.add_element("Water", Vec::new(), Vec::new()).expect("add_element failed");
        let earth = t.add_element("Earth", Vec::new(), Vec::new()).expect("add_element failed");
        let fire = t.add_element("Fire", Vec::new(), Vec::new()).expect("add_element failed");
        let air = t.add_element("Air", Vec::new(), Vec::new()).expect("add_element failed");

        t.add_direct_dep(&Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .expect("add_direct_dep failed");

        let extra_add_dep_res = t.add_direct_dep(&Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        });
        assert!(matches!(extra_add_dep_res, Err(AddDependencyError::AlreadyExists { .. })));

        t.remove_direct_dep(&Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .expect("add_direct_dep failed");

        let extra_remove_dep_res = t.remove_direct_dep(&Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        });
        assert!(matches!(extra_remove_dep_res, Err(RemoveDependencyError::NotFound { .. })));

        t.remove_element(&fire).expect("remove_element failed");
        t.remove_element(&air).expect("remove_element failed");

        let element_not_found_res = t.add_direct_dep(&Dependency {
            level: ElementLevel {
                element: air.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        });
        assert!(matches!(element_not_found_res, Err(AddDependencyError::ElementNotFound { .. })));

        let req_element_not_found_res = t.add_direct_dep(&Dependency {
            level: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: fire.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        });
        assert!(matches!(
            req_element_not_found_res,
            Err(AddDependencyError::RequiredElementNotFound { .. })
        ));
    }

    #[fuchsia::test]
    fn test_add_remove_direct_deps() {
        let mut t = Topology::new();
        let a = t.add_element("A", Vec::new(), Vec::new()).expect("add_element failed");
        let b = t.add_element("B", Vec::new(), Vec::new()).expect("add_element failed");
        let c = t.add_element("C", Vec::new(), Vec::new()).expect("add_element failed");
        let d = t.add_element("D", Vec::new(), Vec::new()).expect("add_element failed");
        // A <- B <- C -> D
        let ba = Dependency {
            level: ElementLevel {
                element: b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: a.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&ba).expect("add_direct_dep failed");
        let cb = Dependency {
            level: ElementLevel {
                element: c.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cb).expect("add_direct_dep failed");
        let cd = Dependency {
            level: ElementLevel {
                element: c.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: d.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cd).expect("add_direct_dep failed");

        let mut a_deps = t.get_direct_deps(&ElementLevel {
            element: a.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        a_deps.sort();
        assert_eq!(a_deps, []);

        let mut b_deps = t.get_direct_deps(&ElementLevel {
            element: b.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        b_deps.sort();
        assert_eq!(b_deps, [ba]);

        let mut c_deps = t.get_direct_deps(&ElementLevel {
            element: c.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        let mut want_c_deps = [cb, cd];
        c_deps.sort();
        want_c_deps.sort();
        assert_eq!(c_deps, want_c_deps);
    }

    #[fuchsia::test]
    fn test_get_all_deps() {
        let mut t = Topology::new();
        let a = t.add_element("A", Vec::new(), Vec::new()).expect("add_element failed");
        let b = t.add_element("B", Vec::new(), Vec::new()).expect("add_element failed");
        let c = t.add_element("C", Vec::new(), Vec::new()).expect("add_element failed");
        let d = t.add_element("D", Vec::new(), Vec::new()).expect("add_element failed");
        // A <- B <- C -> D
        let ba = Dependency {
            level: ElementLevel {
                element: b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: a.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&ba).expect("add_direct_dep failed");
        let cb = Dependency {
            level: ElementLevel {
                element: c.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cb).expect("add_direct_dep failed");
        let cd = Dependency {
            level: ElementLevel {
                element: c.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: d.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cd).expect("add_direct_dep failed");

        let mut a_deps = t.get_all_deps(&ElementLevel {
            element: a.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        a_deps.sort();
        assert_eq!(a_deps, []);

        let mut b_deps = t.get_all_deps(&ElementLevel {
            element: b.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        b_deps.sort();
        assert_eq!(b_deps, [ba.clone()]);

        let mut c_deps = t.get_all_deps(&ElementLevel {
            element: c.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        let mut want_c_deps = [ba.clone(), cb.clone(), cd.clone()];
        c_deps.sort();
        want_c_deps.sort();
        assert_eq!(c_deps, want_c_deps);

        t.remove_direct_dep(&cd).expect("remove_direct_dep failed");
        let mut c_deps = t.get_all_deps(&ElementLevel {
            element: c.clone(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        });
        let mut want_c_deps = [ba.clone(), cb.clone()];
        c_deps.sort();
        want_c_deps.sort();
        assert_eq!(c_deps, want_c_deps);
    }
}
