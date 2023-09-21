// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Manages the Power Element Topology, keeping track of element dependencies.
use fidl_fuchsia_power_broker::PowerLevel;
use std::collections::HashMap;

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

#[derive(Clone, Debug, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct ElementLevel {
    pub id: ElementID,
    pub lvl: PowerLevel,
}

/// Power dependency from one element's PowerLevel to another.
/// The Element and PowerLevel specified by `level` depends on
/// the Element and PowerLevel specified by `requires`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Dependency {
    pub level: ElementLevel,
    pub requires: ElementLevel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Topology {
    source_to_targets_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
}

impl Topology {
    pub fn new() -> Self {
        Topology { source_to_targets_dependencies: HashMap::new() }
    }

    /// Get direct dependencies for the given Element and PowerLevel.
    pub fn get_direct_deps(&self, id: &ElementID, lvl: &PowerLevel) -> Vec<Dependency> {
        let source = ElementLevel { id: id.clone(), lvl: lvl.clone() };
        let targets = self
            .source_to_targets_dependencies
            .get(&source)
            .unwrap_or(&Vec::<ElementLevel>::new())
            .clone();
        targets
            .iter()
            .map(|target| Dependency { level: source.clone(), requires: target.clone() })
            .collect()
    }

    /// Walk the dependency graph using breadth-first search to get all
    /// transitive dependencies for the given Element and PowerLevel.
    pub fn get_all_deps(&self, id: &ElementID, lvl: &PowerLevel) -> Vec<Dependency> {
        let mut deps = Vec::<Dependency>::new();
        let mut element_levels = vec![ElementLevel { id: id.clone(), lvl: lvl.clone() }];
        while let Some(source) = element_levels.pop() {
            let direct_deps = self.get_direct_deps(&source.id, &source.lvl);
            for dep in direct_deps {
                element_levels.push(dep.requires.clone());
                deps.push(dep);
            }
        }
        deps
    }

    /// Add a direct dependency to the Topology.
    pub fn add_direct_dep(&mut self, dep: &Dependency) {
        // TODO(b/299463665): Add Dependency validation here, or in Dependency construction.
        let targets = self
            .source_to_targets_dependencies
            .entry(ElementLevel { id: dep.level.id.clone(), lvl: dep.level.lvl.clone() })
            .or_insert(Vec::new());
        targets.push(dep.requires.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::{BinaryPowerLevel, PowerLevel};

    #[fuchsia::test]
    fn test_add_get_direct_deps() {
        let mut t = Topology::new();
        // A <- B <- C -> D
        let ba = Dependency {
            level: ElementLevel { id: "B".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "A".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&ba);
        let cb = Dependency {
            level: ElementLevel { id: "C".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "B".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cb);
        let cd = Dependency {
            level: ElementLevel { id: "C".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "D".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cd);

        let mut a_deps = t.get_direct_deps(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        a_deps.sort();
        assert_eq!(a_deps, []);

        let mut b_deps = t.get_direct_deps(&"B".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        b_deps.sort();
        assert_eq!(b_deps, [ba]);

        let mut c_deps = t.get_direct_deps(&"C".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        c_deps.sort();
        assert_eq!(c_deps, [cb, cd]);
    }

    #[fuchsia::test]
    fn test_get_all_deps() {
        let mut t = Topology::new();
        // A <- B <- C -> D
        let ba = Dependency {
            level: ElementLevel { id: "B".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "A".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&ba);
        let cb = Dependency {
            level: ElementLevel { id: "C".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "B".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cb);
        let cd = Dependency {
            level: ElementLevel { id: "C".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "D".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        t.add_direct_dep(&cd);

        let mut a_deps = t.get_all_deps(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        a_deps.sort();
        assert_eq!(a_deps, []);

        let mut b_deps = t.get_all_deps(&"B".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        b_deps.sort();
        assert_eq!(b_deps, [ba.clone()]);

        let mut c_deps = t.get_all_deps(&"C".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        c_deps.sort();
        assert_eq!(c_deps, [ba, cb, cd]);
    }
}
