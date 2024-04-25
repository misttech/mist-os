// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Manages the Power Element Topology, keeping track of element dependencies.
use fidl_fuchsia_power_broker::{self as fpb, PowerLevel};
use std::collections::HashMap;
use std::fmt;
use uuid::Uuid;

/// If true, use non-random IDs for ease of debugging.
const ID_DEBUG_MODE: bool = false;

/// The minimum possible PowerLevel.
const ABSOLUTE_MINIMUM_POWER_LEVEL: PowerLevel = 0;

// This may be a token later, but using a String for now for simplicity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct ElementID {
    id: String,
}

impl fmt::Display for ElementID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.id.fmt(f)
    }
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
    pub element_id: ElementID,
    pub level: PowerLevel,
}

impl fmt::Display for ElementLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.element_id, self.level)
    }
}

/// Power dependency from one element's PowerLevel to another.
/// The Element and PowerLevel specified by `dependent` depends on
/// the Element and PowerLevel specified by `requires`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Dependency {
    pub dependent: ElementLevel,
    pub requires: ElementLevel,
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Dep{{{}->{}}}", self.dependent, self.requires)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    id: ElementID,
    name: String,
    // Sorted ascending.
    valid_levels: Vec<PowerLevel>,
}

impl Element {
    fn new(id: ElementID, name: String, mut valid_levels: Vec<PowerLevel>) -> Self {
        valid_levels.sort();
        Self { id, name, valid_levels }
    }
}

#[derive(Debug)]
pub enum AddElementError {
    Internal,
    Invalid,
    NotAuthorized,
}

impl Into<fpb::AddElementError> for AddElementError {
    fn into(self) -> fpb::AddElementError {
        match self {
            AddElementError::Internal => fpb::AddElementError::Internal,
            AddElementError::Invalid => fpb::AddElementError::Invalid,
            AddElementError::NotAuthorized => fpb::AddElementError::NotAuthorized,
        }
    }
}

#[derive(Debug)]
pub enum ModifyDependencyError {
    AlreadyExists,
    Invalid,
    NotAuthorized,
    // TODO(https://fxbug.dev/332392008): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    NotFound(ElementID),
}

impl Into<fpb::ModifyDependencyError> for ModifyDependencyError {
    fn into(self) -> fpb::ModifyDependencyError {
        match self {
            ModifyDependencyError::AlreadyExists => fpb::ModifyDependencyError::AlreadyExists,
            ModifyDependencyError::Invalid => fpb::ModifyDependencyError::Invalid,
            ModifyDependencyError::NotAuthorized => fpb::ModifyDependencyError::NotAuthorized,
            ModifyDependencyError::NotFound(_) => fpb::ModifyDependencyError::NotFound,
        }
    }
}

#[derive(Debug)]
pub struct Topology {
    elements: HashMap<ElementID, Element>,
    active_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
    passive_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
}

impl Topology {
    pub fn new() -> Self {
        Topology {
            elements: HashMap::new(),
            active_dependencies: HashMap::new(),
            passive_dependencies: HashMap::new(),
        }
    }

    pub fn add_element(
        &mut self,
        name: &str,
        valid_levels: Vec<PowerLevel>,
    ) -> Result<ElementID, AddElementError> {
        let id: ElementID = if ID_DEBUG_MODE {
            ElementID::from(name)
        } else {
            ElementID::from(Uuid::new_v4().as_simple().to_string())
        };
        self.elements.insert(id.clone(), Element::new(id.clone(), name.into(), valid_levels));
        Ok(id)
    }

    #[cfg(test)]
    pub fn element_exists(&self, element_id: &ElementID) -> bool {
        self.elements.contains_key(element_id)
    }

    pub fn remove_element(&mut self, element_id: &ElementID) {
        self.elements.remove(element_id);
    }

    pub fn minimum_level(&self, element_id: &ElementID) -> PowerLevel {
        let Some(elem) = self.elements.get(element_id) else {
            return ABSOLUTE_MINIMUM_POWER_LEVEL;
        };
        match elem.valid_levels.first().copied() {
            Some(level) => level,
            None => ABSOLUTE_MINIMUM_POWER_LEVEL,
        }
    }

    pub fn is_valid_level(&self, element_id: &ElementID, level: PowerLevel) -> bool {
        let Some(elem) = self.elements.get(element_id) else {
            return false;
        };
        elem.valid_levels.contains(&level)
    }

    /// Gets direct, active dependencies for the given Element and PowerLevel.
    pub fn direct_active_dependencies(&self, element_level: &ElementLevel) -> Vec<Dependency> {
        self.active_dependencies
            .get(&element_level)
            .unwrap_or(&Vec::<ElementLevel>::new())
            .iter()
            .map(|required| Dependency {
                dependent: element_level.clone(),
                requires: required.clone(),
            })
            .collect()
    }

    /// Gets direct, passive dependencies for the given Element and PowerLevel.
    pub fn direct_passive_dependencies(&self, element_level: &ElementLevel) -> Vec<Dependency> {
        self.passive_dependencies
            .get(&element_level)
            .unwrap_or(&Vec::<ElementLevel>::new())
            .iter()
            .map(|required| Dependency {
                dependent: element_level.clone(),
                requires: required.clone(),
            })
            .collect()
    }

    /// Gets direct and transitive dependencies for the given Element and
    /// PowerLevel. All transitive active dependencies will be returned, but
    /// whenever a passive dependency is encountered, transitive dependencies
    /// downstream of that dependency will be ignored.
    pub fn all_active_and_passive_dependencies(
        &self,
        element_level: &ElementLevel,
    ) -> (Vec<Dependency>, Vec<Dependency>) {
        // For active dependencies, we need to inspect the required level of
        // every active dependency encountered for any transitive active
        // dependencies.
        let mut active_dependencies = Vec::<Dependency>::new();
        // For passive dependencies, we need to inspect the required level of
        // every active dependency encountered for any passive dependencies.
        // However, we do not examine the transitive dependencies of passive
        // dependencies, as they have no effect and can be ignored.
        let mut passive_dependencies = Vec::<Dependency>::new();
        let mut element_levels_to_inspect = vec![element_level.clone()];
        while let Some(element_level) = element_levels_to_inspect.pop() {
            for dep in self.direct_active_dependencies(&element_level) {
                element_levels_to_inspect.push(dep.requires.clone());
                active_dependencies.push(dep);
            }
            for dep in self.direct_passive_dependencies(&element_level) {
                passive_dependencies.push(dep);
            }
        }
        (active_dependencies, passive_dependencies)
    }

    /// Checks that a dependency is valid. Returns ModifyDependencyError if not.
    fn check_valid_dependency(&self, dep: &Dependency) -> Result<(), ModifyDependencyError> {
        if !self.elements.contains_key(&dep.dependent.element_id) {
            return Err(ModifyDependencyError::NotFound(dep.dependent.element_id.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element_id) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        if !self.is_valid_level(&dep.dependent.element_id, dep.dependent.level) {
            return Err(ModifyDependencyError::Invalid);
        }
        if !self.is_valid_level(&dep.requires.element_id, dep.requires.level) {
            return Err(ModifyDependencyError::Invalid);
        }
        Ok(())
    }

    /// Adds an active dependency to the Topology.
    pub fn add_active_dependency(&mut self, dep: &Dependency) -> Result<(), ModifyDependencyError> {
        self.check_valid_dependency(dep)?;
        let required_levels =
            self.active_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::AlreadyExists);
        }
        required_levels.push(dep.requires.clone());
        Ok(())
    }

    /// Removes an active dependency from the Topology.
    pub fn remove_active_dependency(
        &mut self,
        dep: &Dependency,
    ) -> Result<(), ModifyDependencyError> {
        if !self.elements.contains_key(&dep.dependent.element_id) {
            return Err(ModifyDependencyError::NotFound(dep.dependent.element_id.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element_id) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        let required_levels =
            self.active_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if !required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        required_levels.retain(|el| el != &dep.requires);
        Ok(())
    }

    /// Adds a passive dependency to the Topology.
    pub fn add_passive_dependency(
        &mut self,
        dep: &Dependency,
    ) -> Result<(), ModifyDependencyError> {
        self.check_valid_dependency(dep)?;
        let active_required_levels =
            self.active_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if active_required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::AlreadyExists);
        }
        let required_levels =
            self.passive_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::AlreadyExists);
        }
        required_levels.push(dep.requires.clone());
        Ok(())
    }

    /// Removes an passive dependency from the Topology.
    pub fn remove_passive_dependency(
        &mut self,
        dep: &Dependency,
    ) -> Result<(), ModifyDependencyError> {
        if !self.elements.contains_key(&dep.dependent.element_id) {
            return Err(ModifyDependencyError::NotFound(dep.dependent.element_id.clone()));
        }
        if !self.elements.contains_key(&dep.requires.element_id) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        let required_levels =
            self.passive_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if !required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        required_levels.retain(|el| el != &dep.requires);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::BinaryPowerLevel;
    use power_broker_client::BINARY_POWER_LEVELS;

    #[fuchsia::test]
    fn test_add_remove_elements() {
        let mut t = Topology::new();
        let water =
            t.add_element("Water", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        let earth =
            t.add_element("Earth", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        let fire = t.add_element("Fire", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        let air = t.add_element("Air", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");

        t.add_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: water.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: earth.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        })
        .expect("add_active_dependency failed");

        let extra_add_dep_res = t.add_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: water.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: earth.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        });
        assert!(matches!(extra_add_dep_res, Err(ModifyDependencyError::AlreadyExists { .. })));

        t.remove_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: water.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: earth.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        })
        .expect("remove_active_dependency failed");

        let extra_remove_dep_res = t.remove_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: water.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: earth.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        });
        assert!(matches!(extra_remove_dep_res, Err(ModifyDependencyError::NotFound { .. })));

        assert_eq!(t.element_exists(&fire), true);
        t.remove_element(&fire);
        assert_eq!(t.element_exists(&fire), false);
        assert_eq!(t.element_exists(&air), true);
        t.remove_element(&air);
        assert_eq!(t.element_exists(&air), false);

        let element_not_found_res = t.add_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: air.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: water.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        });
        assert!(matches!(element_not_found_res, Err(ModifyDependencyError::NotFound { .. })));

        let req_element_not_found_res = t.add_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: earth.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: fire.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        });
        assert!(matches!(req_element_not_found_res, Err(ModifyDependencyError::NotFound { .. })));
    }

    #[fuchsia::test]
    fn test_add_remove_direct_deps() {
        let mut t = Topology::new();
        let a = t.add_element("A", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        let b = t.add_element("B", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        let c = t.add_element("C", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        let d = t.add_element("D", BINARY_POWER_LEVELS.to_vec()).expect("add_element failed");
        // A <- B <- C -> D
        let ba = Dependency {
            dependent: ElementLevel {
                element_id: b.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: a.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        };
        t.add_active_dependency(&ba).expect("add_active_dependency failed");
        let cb = Dependency {
            dependent: ElementLevel {
                element_id: c.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: b.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        };
        t.add_active_dependency(&cb).expect("add_active_dependency failed");
        let cd = Dependency {
            dependent: ElementLevel {
                element_id: c.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
            requires: ElementLevel {
                element_id: d.clone(),
                level: BinaryPowerLevel::On.into_primitive(),
            },
        };
        t.add_active_dependency(&cd).expect("add_active_dependency failed");

        let mut a_deps = t.direct_active_dependencies(&ElementLevel {
            element_id: a.clone(),
            level: BinaryPowerLevel::On.into_primitive(),
        });
        a_deps.sort();
        assert_eq!(a_deps, []);

        let mut b_deps = t.direct_active_dependencies(&ElementLevel {
            element_id: b.clone(),
            level: BinaryPowerLevel::On.into_primitive(),
        });
        b_deps.sort();
        assert_eq!(b_deps, [ba]);

        let mut c_deps = t.direct_active_dependencies(&ElementLevel {
            element_id: c.clone(),
            level: BinaryPowerLevel::On.into_primitive(),
        });
        let mut want_c_deps = [cb, cd];
        c_deps.sort();
        want_c_deps.sort();
        assert_eq!(c_deps, want_c_deps);
    }

    #[fuchsia::test]
    fn test_all_active_and_passive_dependencies() {
        let mut t = Topology::new();
        let a = t.add_element("A", vec![0, 2, 3]).expect("add_element failed");
        let b = t.add_element("B", vec![0, 1, 5]).expect("add_element failed");
        let c = t.add_element("C", vec![0, 1]).expect("add_element failed");
        let d = t.add_element("D", vec![0, 3]).expect("add_element failed");
        // C has direct active dependencies on B and D.
        // B only has passive dependencies on A.
        // Therefore, C has a transitive passive dependency on A.
        // A <- B <= C => D
        // 2 <- 1
        // 3 <- 5 <= 1 => 3
        let b1_a2 = Dependency {
            dependent: ElementLevel { element_id: b.clone(), level: 1 },
            requires: ElementLevel { element_id: a.clone(), level: 2 },
        };
        t.add_passive_dependency(&b1_a2).expect("add_passive_dependency failed");
        let b5_a3 = Dependency {
            dependent: ElementLevel { element_id: b.clone(), level: 5 },
            requires: ElementLevel { element_id: a.clone(), level: 3 },
        };
        t.add_passive_dependency(&b5_a3).expect("add_passive_dependency failed");
        let c1_b5 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: 1 },
            requires: ElementLevel { element_id: b.clone(), level: 5 },
        };
        t.add_active_dependency(&c1_b5).expect("add_active_dependency failed");
        let c1_d3 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: 1 },
            requires: ElementLevel { element_id: d.clone(), level: 3 },
        };
        t.add_active_dependency(&c1_d3).expect("add_active_dependency failed");

        let (a_active_deps, a_passive_deps) = t
            .all_active_and_passive_dependencies(&ElementLevel { element_id: a.clone(), level: 1 });
        assert_eq!(a_active_deps, []);
        assert_eq!(a_passive_deps, []);

        let (b1_active_deps, b1_passive_deps) = t
            .all_active_and_passive_dependencies(&ElementLevel { element_id: b.clone(), level: 1 });
        assert_eq!(b1_active_deps, []);
        assert_eq!(b1_passive_deps, [b1_a2.clone()]);

        let (b5_active_deps, b5_passive_deps) = t
            .all_active_and_passive_dependencies(&ElementLevel { element_id: b.clone(), level: 5 });
        assert_eq!(b5_active_deps, []);
        assert_eq!(b5_passive_deps, [b5_a3.clone()]);

        let (mut c_active_deps, c_passive_deps) = t
            .all_active_and_passive_dependencies(&ElementLevel { element_id: c.clone(), level: 1 });
        let mut want_c_active_deps = [c1_b5.clone(), c1_d3.clone()];
        c_active_deps.sort();
        want_c_active_deps.sort();
        assert_eq!(c_active_deps, want_c_active_deps);
        assert_eq!(c_passive_deps, [b5_a3.clone()]);

        t.remove_active_dependency(&c1_d3).expect("remove_direct_dep failed");
        let (c_active_deps, c_passive_deps) = t
            .all_active_and_passive_dependencies(&ElementLevel { element_id: c.clone(), level: 1 });
        assert_eq!(c_active_deps, [c1_b5.clone()]);
        assert_eq!(c_passive_deps, [b5_a3.clone()]);
    }
}
