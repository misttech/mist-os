// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Manages the Power Element Topology, keeping track of element dependencies.
use fidl_fuchsia_power_broker::{self as fpb, PowerLevel};
use std::collections::HashMap;
use uuid::Uuid;

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

/// Power dependency from one element's PowerLevel to another.
/// The Element and PowerLevel specified by `level` depends on
/// the Element and PowerLevel specified by `requires`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Dependency {
    pub dependent: ElementLevel,
    pub requires: ElementLevel,
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
    NotAuthorized,
}

impl Into<fpb::RemoveElementError> for RemoveElementError {
    fn into(self) -> fpb::RemoveElementError {
        match self {
            RemoveElementError::NotFound(_) => fpb::RemoveElementError::NotFound,
            RemoveElementError::NotAuthorized => fpb::RemoveElementError::NotAuthorized,
        }
    }
}

#[derive(Debug)]
pub enum AddDependencyError {
    AlreadyExists,
    ElementNotFound(ElementID),
    NotAuthorized,
    RequiredElementNotFound(ElementID),
}

impl Into<fpb::AddDependencyError> for AddDependencyError {
    fn into(self) -> fpb::AddDependencyError {
        match self {
            AddDependencyError::AlreadyExists => fpb::AddDependencyError::AlreadyExists,
            AddDependencyError::ElementNotFound(_) => fpb::AddDependencyError::ElementNotFound,
            AddDependencyError::NotAuthorized => fpb::AddDependencyError::NotAuthorized,
            AddDependencyError::RequiredElementNotFound(_) => {
                fpb::AddDependencyError::RequiredElementNotFound
            }
        }
    }
}

#[derive(Debug)]
pub enum RemoveDependencyError {
    NotAuthorized,
    NotFound(Dependency),
}

impl Into<fpb::RemoveDependencyError> for RemoveDependencyError {
    fn into(self) -> fpb::RemoveDependencyError {
        match self {
            RemoveDependencyError::NotAuthorized => fpb::RemoveDependencyError::NotAuthorized,
            RemoveDependencyError::NotFound(_) => fpb::RemoveDependencyError::NotFound,
        }
    }
}

#[derive(Debug)]
pub struct Topology {
    elements: HashMap<ElementID, Element>,
    source_to_targets_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
}

impl Topology {
    pub fn new() -> Self {
        Topology { elements: HashMap::new(), source_to_targets_dependencies: HashMap::new() }
    }

    pub fn add_element(&mut self, name: &str) -> Result<ElementID, AddElementError> {
        let id = ElementID::from(Uuid::new_v4().as_simple().to_string());
        self.elements.insert(id.clone(), Element { id: id.clone(), name: name.into() });
        Ok(id)
    }

    pub fn remove_element(&mut self, element: &ElementID) -> Result<(), RemoveElementError> {
        if self.elements.remove(element).is_none() {
            return Err(RemoveElementError::NotFound(element.clone()));
        }
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
            .map(|target| Dependency { dependent: element_level.clone(), requires: target.clone() })
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
        if !self.elements.contains_key(&dep.dependent.element) {
            return Err(AddDependencyError::ElementNotFound(dep.dependent.element.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element) {
            return Err(AddDependencyError::RequiredElementNotFound(dep.requires.element.clone()));
        }
        // TODO(b/299463665): Add Dependency validation here, or in Dependency construction.
        let targets =
            self.source_to_targets_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if targets.contains(&dep.requires) {
            return Err(AddDependencyError::AlreadyExists);
        }
        targets.push(dep.requires.clone());
        Ok(())
    }

    /// Removes a direct dependency from the Topology.
    pub fn remove_direct_dep(&mut self, dep: &Dependency) -> Result<(), RemoveDependencyError> {
        if !self.elements.contains_key(&dep.dependent.element) {
            return Err(RemoveDependencyError::NotFound(dep.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element) {
            return Err(RemoveDependencyError::NotFound(dep.clone()));
        }
        let targets =
            self.source_to_targets_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if !targets.contains(&dep.requires) {
            return Err(RemoveDependencyError::NotFound(dep.clone()));
        }
        targets.retain(|el| el != &dep.requires);
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
        let water = t.add_element("Water").expect("add_element failed");
        let earth = t.add_element("Earth").expect("add_element failed");
        let fire = t.add_element("Fire").expect("add_element failed");
        let air = t.add_element("Air").expect("add_element failed");

        t.add_direct_dep(&Dependency {
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
        let a = t.add_element("A").expect("add_element failed");
        let b = t.add_element("B").expect("add_element failed");
        let c = t.add_element("C").expect("add_element failed");
        let d = t.add_element("D").expect("add_element failed");
        // A <- B <- C -> D
        let ba = Dependency {
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
        let a = t.add_element("A").expect("add_element failed");
        let b = t.add_element("B").expect("add_element failed");
        let c = t.add_element("C").expect("add_element failed");
        let d = t.add_element("D").expect("add_element failed");
        // A <- B <- C -> D
        let ba = Dependency {
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
            dependent: ElementLevel {
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
