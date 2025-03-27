// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Manages the Power Element Topology, keeping track of element dependencies.
use crate::inspect::{DependencyData, ElementData, TopologyInspect};
use fidl_fuchsia_power_broker::{self as fpb};
use fuchsia_inspect as inspect;
use fuchsia_inspect_contrib::graph as igraph;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::{fmt, ops};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IndexedPowerLevel {
    pub level: fpb::PowerLevel,
    pub index: usize,
}

impl IndexedPowerLevel {
    pub const MIN: IndexedPowerLevel = IndexedPowerLevel { level: fpb::PowerLevel::MIN, index: 0 };

    #[cfg(test)]
    pub const MAX: IndexedPowerLevel = Self { level: fpb::PowerLevel::MAX, index: usize::MAX };

    #[cfg(test)]
    pub const fn from_same_level_and_index(level_and_index: u8) -> Self {
        Self { level: level_and_index, index: level_and_index as usize }
    }
}

impl fmt::Display for IndexedPowerLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.level)
    }
}

impl std::cmp::PartialOrd for IndexedPowerLevel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.index.partial_cmp(&other.index)
    }
}

impl std::cmp::Ord for IndexedPowerLevel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.index.cmp(&other.index)
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct ElementID(u64);

impl ElementID {
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    fn gen() -> Self {
        Self(rand::random::<u64>())
    }
}

impl fmt::Display for ElementID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl ops::Deref for ElementID {
    type Target = u64;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct ElementLevel {
    pub element_id: ElementID,
    pub level: IndexedPowerLevel,
}

impl fmt::Display for ElementLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.element_id, self.level)
    }
}

/// Power dependency from one element's IndexedPowerLevel to another.
/// The Element and IndexedPowerLevel specified by `dependent` depends on
/// the Element and IndexedPowerLevel specified by `requires`.
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

#[derive(Clone, Debug)]
pub struct Element {
    pub(crate) id: ElementID,
    pub(crate) name: String,
    valid_levels: Vec<IndexedPowerLevel>,
    pub(crate) synthetic: bool,
    pub(crate) inspect_vertex: Option<Rc<RefCell<igraph::Vertex<ElementData>>>>,
    pub(crate) inspect_edges: Rc<RefCell<HashMap<ElementID, igraph::Edge<DependencyData>>>>,
}

impl Element {
    fn new(
        id: ElementID,
        name: String,
        mut valid_levels: Vec<IndexedPowerLevel>,
        synthetic: bool,
    ) -> Self {
        valid_levels.sort();
        Self {
            id,
            name,
            valid_levels,
            synthetic,
            inspect_vertex: None,
            inspect_edges: Rc::new(RefCell::new(HashMap::new())),
        }
    }
}

#[derive(Debug)]
pub enum AddElementError {
    Invalid,
    NotAuthorized,
}

impl Into<fpb::AddElementError> for AddElementError {
    fn into(self) -> fpb::AddElementError {
        match self {
            AddElementError::Invalid => fpb::AddElementError::Invalid,
            AddElementError::NotAuthorized => fpb::AddElementError::NotAuthorized,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ModifyDependencyError {
    AlreadyExists,
    Invalid,
    NotAuthorized,
    // Suppress `dead_code`, callers may want to know what ElementID was not found, but currently
    // no one makes use of this or prints it out.
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
    assertive_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
    opportunistic_dependencies: HashMap<ElementLevel, Vec<ElementLevel>>,
    unsatisfiable_element_id: ElementID,
    inspect: TopologyInspect,
}

impl Topology {
    const TOPOLOGY_UNSATISFIABLE_ELEMENT: &'static str = "TOPOLOGY_UNSATISFIABLE_ELEMENT";
    const TOPOLOGY_UNSATISFIABLE_ELEMENT_POWER_LEVELS: [fpb::PowerLevel; 2] =
        [fpb::PowerLevel::MIN, fpb::PowerLevel::MAX];

    pub fn new(parent_inspect_node: &inspect::Node, inspect_max_event: usize) -> Self {
        let mut topology = Topology {
            elements: HashMap::new(),
            assertive_dependencies: HashMap::new(),
            opportunistic_dependencies: HashMap::new(),
            unsatisfiable_element_id: ElementID::new(0),
            inspect: TopologyInspect::new(
                parent_inspect_node.create_child("topology"),
                inspect_max_event,
            ),
        };
        topology.unsatisfiable_element_id = topology
            .add_element(
                Self::TOPOLOGY_UNSATISFIABLE_ELEMENT,
                &Self::TOPOLOGY_UNSATISFIABLE_ELEMENT_POWER_LEVELS,
            )
            .ok()
            .expect("Failed to add unsatisfiable element");
        topology.elements.get_mut(&topology.unsatisfiable_element_id).unwrap().inspect_vertex =
            Some(Rc::new(RefCell::new(topology.inspect.record_unsatisfiable_element(
                topology.unsatisfiable_element_id.clone(),
                Self::TOPOLOGY_UNSATISFIABLE_ELEMENT,
                &Self::TOPOLOGY_UNSATISFIABLE_ELEMENT_POWER_LEVELS,
            ))));
        topology
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element(&self) -> Element {
        self.elements.get(&self.unsatisfiable_element_id).unwrap().clone()
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element_name(&self) -> String {
        Self::TOPOLOGY_UNSATISFIABLE_ELEMENT.to_string().clone()
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element_id(&self) -> ElementID {
        self.unsatisfiable_element_id.clone()
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element_levels(&self) -> Vec<u64> {
        Self::TOPOLOGY_UNSATISFIABLE_ELEMENT_POWER_LEVELS
            .iter()
            .map(|&v| v as u64)
            .collect::<Vec<_>>()
            .clone()
    }

    pub fn inspect(&self) -> &TopologyInspect {
        &self.inspect
    }

    pub fn get_element(&self, id: &ElementID) -> Option<&Element> {
        self.elements.get(id)
    }

    pub fn add_element(
        &mut self,
        name: &str,
        valid_levels: &[fpb::PowerLevel],
    ) -> Result<ElementID, AddElementError> {
        self.add_element_internal(name, valid_levels, false)
    }

    pub fn add_synthetic_element(
        &mut self,
        name: &str,
        valid_levels: &[fpb::PowerLevel],
    ) -> Result<ElementID, AddElementError> {
        self.add_element_internal(name, valid_levels, true)
    }

    fn add_element_internal(
        &mut self,
        name: &str,
        valid_levels: &[fpb::PowerLevel],
        synthetic: bool,
    ) -> Result<ElementID, AddElementError> {
        let id = {
            loop {
                let element_id = ElementID::gen();
                if !self.elements.contains_key(&element_id) {
                    break element_id;
                }
            }
        };
        let valid_levels = valid_levels
            .iter()
            .enumerate()
            .map(|(index, level)| IndexedPowerLevel { level: *level, index })
            .collect();
        self.elements
            .insert(id.clone(), Element::new(id.clone(), name.into(), valid_levels, synthetic));
        Ok(id)
    }

    #[cfg(test)]
    pub fn element_exists(&self, element_id: &ElementID) -> bool {
        self.elements.contains_key(element_id)
    }

    #[allow(dead_code)]
    pub fn element_is_synthetic(&self, element_id: &ElementID) -> bool {
        self.elements.get(element_id).and_then(|x| Some(x.synthetic)).unwrap_or(false)
    }

    pub fn element_name(&self, element_id: &ElementID) -> Cow<'_, str> {
        Cow::from(
            self.elements.get(element_id).and_then(|e| Some(e.name.as_str())).unwrap_or_default(),
        )
    }

    pub fn remove_element(&mut self, element_id: &ElementID) {
        if self.unsatisfiable_element_id != *element_id {
            self.invalidate_dependent_elements(element_id);
            if let Some(element) = self.elements.remove(element_id) {
                self.inspect().on_remove_element(&element);
            }
        }
    }

    pub fn minimum_level(&self, element_id: &ElementID) -> IndexedPowerLevel {
        let Some(elem) = self.elements.get(element_id) else {
            return IndexedPowerLevel::MIN;
        };
        match elem.valid_levels.first().copied() {
            Some(level) => level,
            None => IndexedPowerLevel::MIN,
        }
    }

    pub fn is_valid_level(&self, element_id: &ElementID, level: IndexedPowerLevel) -> bool {
        let Some(elem) = self.elements.get(element_id) else {
            return false;
        };
        elem.valid_levels.contains(&level)
    }

    pub fn get_level_index(
        &self,
        element_id: &ElementID,
        level: &fpb::PowerLevel,
    ) -> Option<&IndexedPowerLevel> {
        let Some(elem) = self.elements.get(element_id) else {
            return Some(&IndexedPowerLevel::MIN);
        };
        elem.valid_levels.iter().find(|l| &l.level == level)
    }

    fn decrement_element_level_index(
        &self,
        element_id: &ElementID,
        level: &IndexedPowerLevel,
    ) -> IndexedPowerLevel {
        if level.index < 1 {
            return IndexedPowerLevel::MIN;
        }
        let Some(elem) = self.elements.get(element_id) else {
            return IndexedPowerLevel::MIN;
        };
        return elem.valid_levels[level.index - 1];
    }

    /// Gets direct, assertive dependencies for the given Element and PowerLevel.
    pub fn direct_assertive_dependencies<'a>(
        &'a self,
        element_level: &'a ElementLevel,
    ) -> impl Iterator<Item = Dependency> + 'a {
        self.assertive_dependencies
            .get(&element_level)
            .into_iter()
            .flat_map(|required_levels| required_levels.iter())
            .map(|required| Dependency {
                dependent: element_level.clone(),
                requires: required.clone(),
            })
    }

    /// Gets direct, opportunistic dependencies for the given Element and IndexedPowerLevel.
    pub fn direct_opportunistic_dependencies<'a>(
        &'a self,
        element_level: &'a ElementLevel,
    ) -> impl Iterator<Item = Dependency> + 'a {
        self.opportunistic_dependencies
            .get(&element_level)
            .into_iter()
            .flat_map(|required_levels| required_levels.iter())
            .map(|required| Dependency {
                dependent: element_level.clone(),
                requires: required.clone(),
            })
    }

    /// Gets direct and transitive dependencies for the given Element and
    /// IndexedPowerLevel. This is distinct from 'all_assertive_and_opportunistic_dependencies'
    /// as it returns transitive dependencies of opportunistic dependencies
    /// as well.
    pub fn all_direct_and_indirect_dependencies(
        &self,
        element_level: &ElementLevel,
    ) -> (Vec<Dependency>, Vec<Dependency>) {
        // For assertive dependencies, we need to inspect the required level of
        // every assertive dependency encountered for any transitive assertive
        // dependencies.
        let mut assertive_dependencies = Vec::<Dependency>::new();
        // For opportunistic dependencies, we need to inspect the required level of
        // every assertive dependency encountered for any opportunistic dependencies.
        // However, we do not examine the transitive dependencies of opportunistic
        // dependencies, as they have no effect and can be ignored.
        let mut opportunistic_dependencies = Vec::<Dependency>::new();
        let mut element_levels_to_inspect = vec![element_level.clone()];
        while let Some(element_level) = element_levels_to_inspect.pop() {
            if element_level.level != self.minimum_level(&element_level.element_id) {
                let mut lower_element_level = element_level.clone();
                lower_element_level.level = self
                    .decrement_element_level_index(&element_level.element_id, &element_level.level);
                element_levels_to_inspect.push(lower_element_level);
            }
            for dep in self.direct_assertive_dependencies(&element_level) {
                element_levels_to_inspect.push(dep.requires.clone());
                assertive_dependencies.push(dep);
            }
            for dep in self.direct_opportunistic_dependencies(&element_level) {
                element_levels_to_inspect.push(dep.requires.clone());
                opportunistic_dependencies.push(dep);
            }
        }
        (assertive_dependencies, opportunistic_dependencies)
    }

    /// Gets direct and transitive dependencies for the given Element and
    /// IndexedPowerLevel. All transitive assertive dependencies will be returned, but
    /// whenever a opportunistic dependency is encountered, transitive dependencies
    /// downstream of that dependency will be ignored.
    pub fn all_assertive_and_opportunistic_dependencies(
        &self,
        element_level: &ElementLevel,
    ) -> (Vec<Dependency>, Vec<Dependency>) {
        // For assertive dependencies, we need to inspect the required level of
        // every assertive dependency encountered for any transitive assertive
        // dependencies.
        let mut assertive_dependencies = Vec::<Dependency>::new();
        // For opportunistic dependencies, we need to inspect the required level of
        // every assertive dependency encountered for any opportunistic dependencies.
        // However, we do not examine the transitive dependencies of opportunistic
        // dependencies, as they have no effect and can be ignored.
        let mut opportunistic_dependencies = Vec::<Dependency>::new();
        let mut element_levels_to_inspect = vec![element_level.clone()];
        while let Some(element_level) = element_levels_to_inspect.pop() {
            if element_level.level != self.minimum_level(&element_level.element_id) {
                let mut lower_element_level = element_level.clone();
                lower_element_level.level = self
                    .decrement_element_level_index(&element_level.element_id, &element_level.level);
                element_levels_to_inspect.push(lower_element_level);
            }
            for dep in self.direct_assertive_dependencies(&element_level) {
                element_levels_to_inspect.push(dep.requires.clone());
                assertive_dependencies.push(dep);
            }
            for dep in self.direct_opportunistic_dependencies(&element_level) {
                opportunistic_dependencies.push(dep);
            }
        }
        (assertive_dependencies, opportunistic_dependencies)
    }

    /// Elements that have any type of dependency on the provided ElementID are 'invalidated'
    /// by replacing their dependency on the provided ElementID with the 'unsatisfiable' element that
    /// will never be turned on.
    fn invalidate_dependent_elements(&mut self, invalid_element_id: &ElementID) {
        // Prior to removing any dependencies that are no longer valid, ensure that we add a
        // opportunistic dependency to the unsatisfiable element, which forces *future* leases into the
        // contingent state and prevents the broker from attempting to turn on other dependent
        // elements. Existing leases will remain unaffected.
        let assertive_dependents_of_invalid_elements: Vec<ElementLevel> = self
            .assertive_dependencies
            .iter()
            .filter_map(|(dependent, requires)| {
                if requires
                    .iter()
                    .any(|required_level| required_level.element_id == *invalid_element_id)
                {
                    Some(dependent.clone())
                } else {
                    None
                }
            })
            .collect();
        for dependent in assertive_dependents_of_invalid_elements {
            self.add_opportunistic_dependency(&Dependency {
                dependent: dependent.clone(),
                requires: ElementLevel {
                    element_id: self.unsatisfiable_element_id.clone(),
                    level: IndexedPowerLevel { level: fpb::PowerLevel::MAX, index: 1 },
                },
            })
            .expect("failed to replace assertive dependency with unsatisfiable dependency");
            for requires in self.assertive_dependencies.get(&dependent).unwrap().clone() {
                if requires.element_id == *invalid_element_id {
                    self.remove_assertive_dependency(&Dependency {
                        dependent: dependent.clone(),
                        requires: requires.clone(),
                    })
                    .expect("failed to remove invalid assertive dependency");
                }
            }
        }
        let opportunistic_dependents_of_invalid_elements: Vec<ElementLevel> = self
            .opportunistic_dependencies
            .iter()
            .filter_map(|(dependent, requires)| {
                if requires
                    .iter()
                    .any(|required_level| required_level.element_id == *invalid_element_id)
                {
                    Some(dependent.clone())
                } else {
                    None
                }
            })
            .collect();
        for dependent in opportunistic_dependents_of_invalid_elements {
            self.add_opportunistic_dependency(&Dependency {
                dependent: dependent.clone(),
                requires: ElementLevel {
                    element_id: self.unsatisfiable_element_id.clone(),
                    level: IndexedPowerLevel { level: fpb::PowerLevel::MAX, index: 1 },
                },
            })
            .expect("failed to replace opportunistic dependency with unsatisfiable dependency");
            for requires in self.opportunistic_dependencies.get(&dependent).unwrap().clone() {
                if requires.element_id == *invalid_element_id {
                    self.remove_opportunistic_dependency(&Dependency {
                        dependent: dependent.clone(),
                        requires: requires.clone(),
                    })
                    .expect("failed to remove invalid opportunistic dependency");
                }
            }
        }
        self.assertive_dependencies.retain(|key, _| key.element_id != *invalid_element_id);
        self.opportunistic_dependencies.retain(|key, _| key.element_id != *invalid_element_id);
    }

    /// Checks that a dependency is valid. Returns ModifyDependencyError if not.
    fn check_valid_dependency(&self, dep: &Dependency) -> Result<(), ModifyDependencyError> {
        if &dep.dependent.element_id == &dep.requires.element_id {
            return Err(ModifyDependencyError::Invalid);
        }
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
        if self.unsatisfiable_element_id == dep.dependent.element_id {
            return Err(ModifyDependencyError::Invalid);
        }
        Ok(())
    }

    /// Adds an assertive dependency to the Topology.
    pub fn add_assertive_dependency(
        &mut self,
        dep: &Dependency,
    ) -> Result<(), ModifyDependencyError> {
        self.check_valid_dependency(dep)?;
        let required_levels =
            self.assertive_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::AlreadyExists);
        }
        required_levels.push(dep.requires.clone());
        self.inspect.on_add_dependency(&self.elements, dep, true);
        Ok(())
    }

    /// Removes an assertive dependency from the Topology.
    pub fn remove_assertive_dependency(
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
            self.assertive_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if !required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        required_levels.retain(|el| el != &dep.requires);
        self.inspect.on_remove_dependency(&self.elements, dep);
        Ok(())
    }

    /// Adds a opportunistic dependency to the Topology.
    pub fn add_opportunistic_dependency(
        &mut self,
        dep: &Dependency,
    ) -> Result<(), ModifyDependencyError> {
        self.check_valid_dependency(dep)?;
        let assertive_required_levels =
            self.assertive_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if assertive_required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::AlreadyExists);
        }
        let required_levels =
            self.opportunistic_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::AlreadyExists);
        }
        required_levels.push(dep.requires.clone());
        self.inspect.on_add_dependency(&self.elements, dep, false);
        Ok(())
    }

    /// Removes an opportunistic dependency from the Topology.
    pub fn remove_opportunistic_dependency(
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
            self.opportunistic_dependencies.entry(dep.dependent.clone()).or_insert(Vec::new());
        if !required_levels.contains(&dep.requires) {
            return Err(ModifyDependencyError::NotFound(dep.requires.element_id.clone()));
        }
        required_levels.retain(|el| el != &dep.requires);
        self.inspect.on_remove_dependency(&self.elements, dep);
        Ok(())
    }

    pub fn initialize_element_inspect<'a>(
        &mut self,
        element_id: &'a ElementID,
        valid_levels: Vec<fpb::PowerLevel>,
        current_level: fpb::PowerLevel,
        required_level: fpb::PowerLevel,
    ) {
        let Some(element) = self.elements.get_mut(element_id) else {
            return;
        };
        let inspect_vertex = self.inspect.on_add_element(
            element_id.clone(),
            &element.name,
            element.synthetic,
            valid_levels,
            current_level,
            required_level,
        );
        element.inspect_vertex = Some(Rc::new(RefCell::new(inspect_vertex)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use power_broker_client::BINARY_POWER_LEVELS;

    const BINARY_POWER_LEVEL_ON: IndexedPowerLevel = IndexedPowerLevel { level: 1, index: 1 };

    const ONE: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(1);
    const TWO: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(2);
    const THREE: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(3);

    impl Topology {
        fn add_element_with_inspect(
            &mut self,
            name: &str,
            valid_levels: Vec<fpb::PowerLevel>,
            current_level: fpb::PowerLevel,
            required_level: fpb::PowerLevel,
        ) -> Result<ElementID, AddElementError> {
            let element_id = self.add_element(name, &valid_levels)?;
            self.initialize_element_inspect(
                &element_id,
                valid_levels,
                current_level,
                required_level,
            );
            Ok(element_id)
        }
    }

    #[fuchsia::test]
    fn test_add_remove_elements() {
        let inspect = fuchsia_inspect::Inspector::default();
        let mut t = Topology::new(inspect.root(), 0);
        let water = t
            .add_element_with_inspect(
                "Water",
                BINARY_POWER_LEVELS.to_vec(),
                fpb::BinaryPowerLevel::On as u8,
                fpb::BinaryPowerLevel::On as u8,
            )
            .expect("add_element failed");
        let earth = t
            .add_element_with_inspect(
                "Earth",
                BINARY_POWER_LEVELS.to_vec(),
                fpb::BinaryPowerLevel::Off as u8,
                fpb::BinaryPowerLevel::On as u8,
            )
            .expect("add_element failed");
        let fire = t
            .add_element_with_inspect(
                "Fire",
                BINARY_POWER_LEVELS.to_vec(),
                fpb::BinaryPowerLevel::On as u8,
                fpb::BinaryPowerLevel::Off as u8,
            )
            .expect("add_element failed");
        let air = t
            .add_element_with_inspect(
                "Air",
                BINARY_POWER_LEVELS.to_vec(),
                fpb::BinaryPowerLevel::Off as u8,
                fpb::BinaryPowerLevel::Off as u8,
            )
            .expect("add_element failed");
        let v01: Vec<u64> = BINARY_POWER_LEVELS.iter().map(|&v| v as u64).collect();
        assert_data_tree!(inspect, root: {
            topology: {
                "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element_id().to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                current_level: "unset",
                                required_level: "unset",
                                leases: {}
                            },
                            relationships: {},
                        },
                        water.to_string() => {
                            meta: {
                                name: "Water",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        earth.to_string() => {
                            meta: {
                                name: "Earth",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        fire.to_string() => {
                            meta: {
                                name: "Fire",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        air.to_string() => {
                            meta: {
                                name: "Air",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
        }}}});

        t.add_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: water.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        })
        .expect("add_assertive_dependency failed");
        assert_data_tree!(inspect, root: {
           topology: {
            "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {}
                            },
                            relationships: {}
                        },
                        water.to_string() => {
                            meta: {
                                name: "Water",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {
                                earth.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 1u64,
                                        }
                                    },
                                },
                            },
                        },
                        earth.to_string() => {
                            meta: {
                                name: "Earth",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        fire.to_string() => {
                            meta: {
                                name: "Fire",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        air.to_string() => {
                            meta: {
                                name: "Air",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
        }}}});

        let extra_add_dep_res = t.add_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: water.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        });
        assert!(matches!(extra_add_dep_res, Err(ModifyDependencyError::AlreadyExists { .. })));

        t.remove_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: water.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        })
        .expect("remove_assertive_dependency failed");
        assert_data_tree!(inspect, root: {
           topology: {
            "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {}
                            },
                            relationships: {}},
                        water.to_string() => {
                            meta: {
                                name: "Water",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {
                                earth.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {},
                                },
                            },
                        },
                        earth.to_string() => {
                            meta: {
                                name: "Earth",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        fire.to_string() => {
                            meta: {
                                name: "Fire",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        air.to_string() => {
                            meta: {
                                name: "Air",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
        }}}});

        let extra_remove_dep_res = t.remove_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: water.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        });
        assert!(matches!(extra_remove_dep_res, Err(ModifyDependencyError::NotFound { .. })));

        assert_eq!(t.element_exists(&fire), true);
        t.add_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: fire.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        })
        .expect("add_assertive_dependency failed");
        t.remove_element(&fire);
        assert_eq!(t.element_exists(&fire), false);
        let removed_element_dep_res = t.remove_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: fire.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        });
        assert!(matches!(removed_element_dep_res, Err(ModifyDependencyError::NotFound { .. })));

        assert_eq!(t.element_exists(&air), true);
        t.remove_element(&air);
        assert_eq!(t.element_exists(&air), false);

        assert_data_tree!(inspect, root: {
           topology: {
            "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {},
                            },
                            relationships: {}},
                        water.to_string() => {
                            meta: {
                                name: "Water",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {},
                            },
                            relationships: {
                                earth.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {},
                                },
                            },
                        },
                        earth.to_string() => {
                            meta: {
                                name: "Earth",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 1u64,
                                leases: {},
                            },
                            relationships: {},
                        },
        }}}});

        let element_not_found_res = t.add_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: air.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: water.clone(), level: BINARY_POWER_LEVEL_ON },
        });
        assert!(matches!(element_not_found_res, Err(ModifyDependencyError::NotFound { .. })));

        let req_element_not_found_res = t.add_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: fire.clone(), level: BINARY_POWER_LEVEL_ON },
        });
        assert!(matches!(req_element_not_found_res, Err(ModifyDependencyError::NotFound { .. })));

        assert_data_tree!(inspect, root: {
           topology: {
            "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {},
                            },
                            relationships: {}},
                        water.to_string() => {
                            meta: {
                                name: "Water",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {},
                            },
                            relationships: {
                                earth.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {},
                                },
                            },
                        },
                        earth.to_string() => {
                            meta: {
                                name: "Earth",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 1u64,
                                leases: {},
                            },
                            relationships: {},
                        },
        }}}});

        t.add_assertive_dependency(&Dependency {
            dependent: ElementLevel { element_id: water.clone(), level: BINARY_POWER_LEVEL_ON },
            requires: ElementLevel { element_id: earth.clone(), level: BINARY_POWER_LEVEL_ON },
        })
        .expect("add_assertive_dependency failed");
        assert_data_tree!(inspect, root: { topology: {
            "fuchsia.inspect.synthetic.Graph": contains {},
            "fuchsia.inspect.Graph": { "topology": {
            t.get_unsatisfiable_element().id.to_string() => {
                meta: {
                    name: t.get_unsatisfiable_element().name,
                    valid_levels: t.get_unsatisfiable_element_levels(),
                    required_level: "unset",
                    current_level: "unset",
                    leases: {},
                },
                relationships: {}},
            water.to_string() => {
                meta: {
                    name: "Water",
                    valid_levels: v01.clone(),
                    current_level: 1u64,
                    required_level: 1u64,
                    leases: {},
                },
                relationships: {
                    earth.to_string() => {
                        edge_id: AnyProperty,
                        meta: {
                            "1": {
                                required_level: 1u64,
                            }
                        }
                    },
                },
            },
            earth.to_string() => {
                meta: {
                    name: "Earth",
                    valid_levels: v01.clone(),
                    current_level: 0u64,
                    required_level: 1u64,
                    leases: {},
                },
                relationships: {},
            },
        }}}});

        t.remove_element(&earth);
        assert_eq!(t.element_exists(&earth), false);
        assert_data_tree!(inspect, root: { topology: { "fuchsia.inspect.synthetic.Graph": contains {}, "fuchsia.inspect.Graph": { "topology": {
            t.get_unsatisfiable_element().id.to_string() => {
                meta: {
                    name: t.get_unsatisfiable_element().name,
                    valid_levels: t.get_unsatisfiable_element_levels(),
                    required_level: "unset",
                    current_level: "unset",
                    leases: {},
                },
                relationships: {}
            },
            water.to_string() => {
                meta: {
                    name: "Water",
                    valid_levels: v01.clone(),
                    current_level: 1u64,
                    required_level: 1u64,
                    leases: {},
                },
                relationships: {
                    t.get_unsatisfiable_element().id.to_string() => {
                        edge_id: AnyProperty,
                        meta: {
                            "1": {
                                required_level: fpb::PowerLevel::MAX as u64,
                                opportunistic: true,
                            }
                        }
                    },
                },
            },
        }}}});

        let synthetic_element = t
            .add_synthetic_element("Synthetic", &BINARY_POWER_LEVELS)
            .expect("add_synthetic_element failed");
        assert_eq!(t.element_exists(&synthetic_element), true);
        assert_eq!(t.element_is_synthetic(&synthetic_element), true);
    }

    #[fuchsia::test]
    fn test_add_remove_direct_deps() {
        let inspect = fuchsia_inspect::Inspector::default();
        let mut t = Topology::new(inspect.root(), 0);

        let v012_u8: Vec<u8> = vec![0, 1, 2];
        let v012: Vec<u64> = v012_u8.iter().map(|&v| v as u64).collect();

        let a = t.add_element_with_inspect("A", v012_u8.clone(), 0, 0).expect("add_element failed");
        let b = t.add_element_with_inspect("B", v012_u8.clone(), 0, 0).expect("add_element failed");
        let c = t.add_element_with_inspect("C", v012_u8.clone(), 0, 0).expect("add_element failed");
        let d = t.add_element_with_inspect("D", v012_u8.clone(), 0, 0).expect("add_element failed");
        // A <- B <- C -> D
        let ba = Dependency {
            dependent: ElementLevel { element_id: b.clone(), level: ONE },
            requires: ElementLevel { element_id: a.clone(), level: ONE },
        };
        t.add_assertive_dependency(&ba).expect("add_assertive_dependency failed");
        let cb = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: ONE },
            requires: ElementLevel { element_id: b.clone(), level: ONE },
        };
        t.add_assertive_dependency(&cb).expect("add_assertive_dependency failed");
        let cd = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: ONE },
            requires: ElementLevel { element_id: d.clone(), level: ONE },
        };
        t.add_assertive_dependency(&cd).expect("add_assertive_dependency failed");
        let cd2 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: TWO },
            requires: ElementLevel { element_id: d.clone(), level: TWO },
        };
        t.add_assertive_dependency(&cd2).expect("add_assertive_dependency failed");
        assert_data_tree!(inspect, root: {
            topology: {
                "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {}
                            },
                            relationships: {}},
                        a.to_string() => {
                            meta: {
                                name: "A",
                                valid_levels: v012.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        b.to_string() => {
                            meta: {
                                name: "B",
                                valid_levels: v012.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {
                                a.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 1u64,
                                        }
                                    }
                                },
                            },
                        },
                        c.to_string() => {
                            meta: {
                                name: "C",
                                valid_levels: v012.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {
                                b.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 1u64,
                                        }
                                    }
                                },
                                d.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 1u64,
                                        },
                                        "2": {
                                            required_level: 2u64,
                                        }
                                    }
                                },
                            },
                        },
                        d.to_string() => {
                            meta: {
                                name: "D",
                                valid_levels: v012.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
        }}}});

        let mut a_deps = t
            .direct_assertive_dependencies(&ElementLevel { element_id: a.clone(), level: ONE })
            .collect::<Vec<_>>();
        a_deps.sort();
        assert_eq!(a_deps, []);

        let mut b_deps = t
            .direct_assertive_dependencies(&ElementLevel { element_id: b.clone(), level: ONE })
            .collect::<Vec<_>>();
        b_deps.sort();
        assert_eq!(b_deps, [ba]);

        let mut c_deps = t
            .direct_assertive_dependencies(&ElementLevel { element_id: c.clone(), level: ONE })
            .collect::<Vec<_>>();
        let mut want_c_deps = [cb, cd];
        c_deps.sort();
        want_c_deps.sort();
        assert_eq!(c_deps, want_c_deps);
    }

    #[fuchsia::test]
    fn test_all_assertive_and_opportunistic_dependencies() {
        let inspect = fuchsia_inspect::Inspector::default();
        let mut t = Topology::new(inspect.root(), 0);

        let (v0123_u8, v015_u8, v01_u8, v013_u8): (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) =
            (vec![0, 1, 2, 3], vec![0, 1, 5], vec![0, 1], vec![0, 1, 3]);
        let (v0123, v015, v01, v013): (Vec<u64>, Vec<u64>, Vec<u64>, Vec<u64>) = (
            v0123_u8.iter().map(|&v| v as u64).collect(),
            v015_u8.iter().map(|&v| v as u64).collect(),
            v01_u8.iter().map(|&v| v as u64).collect(),
            v013_u8.iter().map(|&v| v as u64).collect(),
        );

        let a =
            t.add_element_with_inspect("A", vec![0, 1, 2, 3], 2, 1).expect("add_element failed");
        let b = t.add_element_with_inspect("B", vec![0, 1, 5], 0, 5).expect("add_element failed");
        let c = t.add_element_with_inspect("C", vec![0, 1], 1, 1).expect("add_element failed");
        let d = t.add_element_with_inspect("D", vec![0, 1, 3], 1, 3).expect("add_element failed");
        let e = t.add_element_with_inspect("E", vec![0, 1], 0, 0).expect("add_element failed");
        assert_data_tree!(inspect, root: {
            topology: {
                "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {}
                            },
                            relationships: {}},
                        a.to_string() => {
                            meta: {
                                name: "A",
                                valid_levels: v0123.clone(),
                                current_level: 2u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        b.to_string() => {
                            meta: {
                                name: "B",
                                valid_levels: v015.clone(),
                                current_level: 0u64,
                                required_level: 5u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        c.to_string() => {
                            meta: {
                                name: "C",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        d.to_string() => {
                            meta: {
                                name: "D",
                                valid_levels: v013.clone(),
                                current_level: 1u64,
                                required_level: 3u64,
                                leases: {}
                            },
                            relationships: {},
                        },
                        e.to_string() => {
                            meta: {
                                name: "E",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {}
                            },
                            relationships: {},
                        },
        },
                }}});

        // C has direct assertive dependencies on B and D.
        // B only has opportunistic dependencies on A.
        // D only has an assertive dependency on A.
        //
        // C has a transitive opportunistic dependency on A[3] (through B[5]).
        // C has an *implicit* transitive opportunistic dependency on A[2] (through B[1]).
        // C has an *implicit* transitive assertive dependency on A (through D[1]).
        //
        // A    B    C    D    E
        // 1 <=========== 1 => 1
        // 2 <- 1
        // 3 <- 5 <= 1 => 3
        let b1_a2 = Dependency {
            dependent: ElementLevel { element_id: b.clone(), level: ONE },
            requires: ElementLevel { element_id: a.clone(), level: TWO },
        };
        t.add_opportunistic_dependency(&b1_a2).expect("add_opportunistic_dependency failed");
        let b5_a3 = Dependency {
            dependent: ElementLevel {
                element_id: b.clone(),
                level: IndexedPowerLevel { level: 5, index: 2 },
            },
            requires: ElementLevel { element_id: a.clone(), level: THREE },
        };
        t.add_opportunistic_dependency(&b5_a3).expect("add_opportunistic_dependency failed");
        let c1_b5 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: ONE },
            requires: ElementLevel {
                element_id: b.clone(),
                level: IndexedPowerLevel { level: 5, index: 2 },
            },
        };
        t.add_assertive_dependency(&c1_b5).expect("add_assertive_dependency failed");
        let c1_d3 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: ONE },
            requires: ElementLevel {
                element_id: d.clone(),
                level: IndexedPowerLevel { level: 3, index: 2 },
            },
        };
        t.add_assertive_dependency(&c1_d3).expect("add_assertive_dependency failed");
        let d1_a1 = Dependency {
            dependent: ElementLevel { element_id: d.clone(), level: ONE },
            requires: ElementLevel { element_id: a.clone(), level: ONE },
        };
        t.add_assertive_dependency(&d1_a1).expect("add_assertive_dependency failed");
        let d1_e1 = Dependency {
            dependent: ElementLevel { element_id: d.clone(), level: ONE },
            requires: ElementLevel { element_id: e.clone(), level: ONE },
        };
        t.add_assertive_dependency(&d1_e1).expect("add_assertive_dependency failed");
        assert_data_tree!(inspect, root: {
            topology: {
                "fuchsia.inspect.synthetic.Graph": contains {},
                "fuchsia.inspect.Graph": {
                    topology: {
                        t.get_unsatisfiable_element().id.to_string() => {
                            meta: {
                                name: t.get_unsatisfiable_element().name,
                                valid_levels: t.get_unsatisfiable_element_levels(),
                                required_level: "unset",
                                current_level: "unset",
                                leases: {},
                            },
                            relationships: {}},
                        a.to_string() => {
                            meta: {
                                name: "A",
                                valid_levels: v0123.clone(),
                                current_level: 2u64,
                                required_level: 1u64,
                                leases: {},
                            },
                            relationships: {},
                        },
                        b.to_string() => {
                            meta: {
                                name: "B",
                                valid_levels: v015.clone(),
                                current_level: 0u64,
                                required_level: 5u64,
                                leases: {},
                            },
                            relationships: {
                                a.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 2u64,
                                            opportunistic: true,
                                        },
                                        "5": {
                                            required_level: 3u64,
                                            opportunistic: true,
                                        }
                                    },
                                },
                            },
                        },
                        c.to_string() => {
                            meta: {
                                name: "C",
                                valid_levels: v01.clone(),
                                current_level: 1u64,
                                required_level: 1u64,
                                leases: {},
                            },
                            relationships: {
                                b.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 5u64,
                                        },
                                    },
                                },
                                d.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 3u64,
                                        },
                                    },
                                },
                            },
                        },
                        d.to_string() => {
                            meta: {
                                name: "D",
                                valid_levels: v013.clone(),
                                current_level: 1u64,
                                required_level: 3u64,
                                leases: {},
                            },
                            relationships: {
                                a.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 1u64,
                                        },
                                    },
                                },
                                e.to_string() => {
                                    edge_id: AnyProperty,
                                    meta: {
                                        "1": {
                                            required_level: 1u64,
                                        },
                                    },
                                },
                            },
                        },
                        e.to_string() => {
                            meta: {
                                name: "E",
                                valid_levels: v01.clone(),
                                current_level: 0u64,
                                required_level: 0u64,
                                leases: {},
                            },
                            relationships: {},
                        },
        }}}});

        let (a_assertive_deps, a_opportunistic_deps) = t
            .all_assertive_and_opportunistic_dependencies(&ElementLevel {
                element_id: a.clone(),
                level: ONE,
            });
        assert_eq!(a_assertive_deps, []);
        assert_eq!(a_opportunistic_deps, []);

        let (b1_assertive_deps, b1_opportunistic_deps) = t
            .all_assertive_and_opportunistic_dependencies(&ElementLevel {
                element_id: b.clone(),
                level: ONE,
            });
        assert_eq!(b1_assertive_deps, []);
        assert_eq!(b1_opportunistic_deps, [b1_a2.clone()]);

        let (b5_assertive_deps, mut b5_opportunistic_deps) = t
            .all_assertive_and_opportunistic_dependencies(&ElementLevel {
                element_id: b.clone(),
                level: IndexedPowerLevel { level: 5, index: 2 },
            });
        let mut want_b5_opportunistic_deps = [b5_a3.clone(), b1_a2.clone()];
        b5_opportunistic_deps.sort();
        want_b5_opportunistic_deps.sort();
        assert_eq!(b5_assertive_deps, []);
        assert_eq!(b5_opportunistic_deps, want_b5_opportunistic_deps);

        let (mut c_assertive_deps, mut c_opportunistic_deps) = t
            .all_assertive_and_opportunistic_dependencies(&ElementLevel {
                element_id: c.clone(),
                level: ONE,
            });
        let mut want_c_assertive_deps =
            [c1_b5.clone(), c1_d3.clone(), d1_a1.clone(), d1_e1.clone()];
        c_assertive_deps.sort();
        want_c_assertive_deps.sort();
        assert_eq!(c_assertive_deps, want_c_assertive_deps);
        let mut want_c_opportunistic_deps = [b5_a3.clone(), b1_a2.clone()];
        c_opportunistic_deps.sort();
        want_c_opportunistic_deps.sort();
        assert_eq!(c_opportunistic_deps, want_c_opportunistic_deps);

        t.remove_assertive_dependency(&c1_d3).expect("remove_direct_dep failed");
        let (c_assertive_deps, c_opportunistic_deps) = t
            .all_assertive_and_opportunistic_dependencies(&ElementLevel {
                element_id: c.clone(),
                level: ONE,
            });
        assert_eq!(c_assertive_deps, [c1_b5.clone()]);
        assert_eq!(c_opportunistic_deps, [b5_a3.clone(), b1_a2.clone()]);
    }

    #[fuchsia::test]
    fn test_all_direct_and_indirect_dependencies() {
        let inspect = fuchsia_inspect::Inspector::default();
        let mut t = Topology::new(inspect.root(), 0);

        let a =
            t.add_element_with_inspect("A", vec![0, 1, 2, 3], 0, 0).expect("add_element failed");
        let b = t.add_element_with_inspect("B", vec![0, 1, 5], 0, 0).expect("add_element failed");
        let c = t.add_element_with_inspect("C", vec![0, 1], 0, 0).expect("add_element failed");
        let d = t.add_element_with_inspect("D", vec![0, 1, 3], 0, 0).expect("add_element failed");
        let e = t.add_element_with_inspect("E", vec![0, 1], 0, 0).expect("add_element failed");

        // C has direct assertive dependencies on B and D.
        // B only has opportunistic dependencies on A.
        // D has an assertive dependency on A.
        // D has an opportunistic dependency on E.
        //
        // C has a transitive opportunistic dependency on A[3] (through B[5]).
        // C has an *implicit* transitive opportunistic dependency on A[2] (through B[1]).
        // C has an *implicit* transitive assertive dependency on A (through D[1]).
        //
        // A    B    C    D    E
        // 1 <=========== 1 -> 1
        // 2 <- 1
        // 3 <- 5 <= 1 => 3
        let b1_a2 = Dependency {
            dependent: ElementLevel { element_id: b.clone(), level: ONE },
            requires: ElementLevel { element_id: a.clone(), level: TWO },
        };
        t.add_opportunistic_dependency(&b1_a2).expect("add_opportunistic_dependency failed");
        let b5_a3 = Dependency {
            dependent: ElementLevel {
                element_id: b.clone(),
                level: IndexedPowerLevel { level: 5, index: 2 },
            },
            requires: ElementLevel { element_id: a.clone(), level: THREE },
        };
        t.add_opportunistic_dependency(&b5_a3).expect("add_opportunistic_dependency failed");
        let c1_b5 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: ONE },
            requires: ElementLevel {
                element_id: b.clone(),
                level: IndexedPowerLevel { level: 5, index: 2 },
            },
        };
        t.add_assertive_dependency(&c1_b5).expect("add_assertive_dependency failed");
        let c1_d3 = Dependency {
            dependent: ElementLevel { element_id: c.clone(), level: ONE },
            requires: ElementLevel {
                element_id: d.clone(),
                level: IndexedPowerLevel { level: 3, index: 2 },
            },
        };
        t.add_assertive_dependency(&c1_d3).expect("add_assertive_dependency failed");
        let d1_a1 = Dependency {
            dependent: ElementLevel { element_id: d.clone(), level: ONE },
            requires: ElementLevel { element_id: a.clone(), level: ONE },
        };
        t.add_assertive_dependency(&d1_a1).expect("add_assertive_dependency failed");
        let d1_e1 = Dependency {
            dependent: ElementLevel { element_id: d.clone(), level: ONE },
            requires: ElementLevel { element_id: e.clone(), level: ONE },
        };
        t.add_opportunistic_dependency(&d1_e1).expect("add_opportunistic_dependency failed");

        let (a_assertive_deps, a_opportunistic_deps) =
            t.all_direct_and_indirect_dependencies(&ElementLevel {
                element_id: a.clone(),
                level: ONE,
            });
        assert_eq!(a_assertive_deps, []);
        assert_eq!(a_opportunistic_deps, []);

        let (b1_assertive_deps, b1_opportunistic_deps) =
            t.all_direct_and_indirect_dependencies(&ElementLevel {
                element_id: b.clone(),
                level: ONE,
            });
        assert_eq!(b1_assertive_deps, []);
        assert_eq!(b1_opportunistic_deps, [b1_a2.clone()]);

        let (b5_assertive_deps, mut b5_opportunistic_deps) = t
            .all_direct_and_indirect_dependencies(&ElementLevel {
                element_id: b.clone(),
                level: IndexedPowerLevel { level: 5, index: 2 },
            });
        let mut want_b5_opportunistic_deps = [b5_a3.clone(), b1_a2.clone()];
        b5_opportunistic_deps.sort();
        want_b5_opportunistic_deps.sort();
        assert_eq!(b5_assertive_deps, []);
        assert_eq!(b5_opportunistic_deps, want_b5_opportunistic_deps);

        let (mut c_assertive_deps, mut c_opportunistic_deps) = t
            .all_direct_and_indirect_dependencies(&ElementLevel {
                element_id: c.clone(),
                level: ONE,
            });
        let mut want_c_assertive_deps = [c1_b5.clone(), c1_d3.clone(), d1_a1.clone()];
        c_assertive_deps.sort();
        want_c_assertive_deps.sort();
        assert_eq!(c_assertive_deps, want_c_assertive_deps);
        let mut want_c_opportunistic_deps = [b5_a3.clone(), b1_a2.clone(), d1_e1.clone()];
        c_opportunistic_deps.sort();
        want_c_opportunistic_deps.sort();
        assert_eq!(c_opportunistic_deps, want_c_opportunistic_deps);

        t.remove_assertive_dependency(&c1_d3).expect("remove_direct_dep failed");
        let (c_assertive_deps, c_opportunistic_deps) =
            t.all_direct_and_indirect_dependencies(&ElementLevel {
                element_id: c.clone(),
                level: ONE,
            });
        assert_eq!(c_assertive_deps, [c1_b5.clone()]);
        assert_eq!(c_opportunistic_deps, [b5_a3.clone(), b1_a2.clone()]);
    }
}
