// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::broker::{Lease, LeaseID};
use crate::topology::{Dependency, Element, Topology};
use crate::ElementID;
use either::Either;
use fuchsia_inspect::{ArrayProperty, Property};
use fuchsia_inspect_contrib::graph as igraph;
use fuchsia_inspect_contrib::graph::{Digraph, DigraphOpts};
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use futures::FutureExt;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use {fidl_fuchsia_power_broker as fpb, fuchsia_inspect as inspect};

const ADD_ELEMENT_EVENT: &str = "add_element";
const ADD_DEPENDENCY_EVENT: &str = "add_dep";
const UPDATE_DEP_LEVEL_EVENT: &str = "update_dep";
const REMOVE_DEP_EVENT: &str = "rm_dep";
const UPDATE_LEVEL_EVENT: &str = "update_level";
const REMOVE_ELEMENT_EVENT: &str = "rm_element";
const CREATE_LEASE_EVENT: &str = "create_lease";
const REMOVE_LEASE_EVENT: &str = "rm_lease";
const UPDATE_LEASE_STATUS_EVENT: &str = "update_lease";
const UNSET: &str = "unset";

const ELEMENT_ID: &str = "element_id";
const ELEMENT_NAME: &str = "element_name";
const LEVEL: &str = "level";
const LEASE_ID: &str = "lease_id";
const STATUS: &str = "status";
const TIME: &str = "@time";
const EDGE_ID: &str = "edge_id";
const DEP_ELEMENT: &str = "dependent_element";
const REQ_ELEMENT: &str = "required_element";
const OPPORTUNISTIC: &str = "opportunistic";
const DEPENDENT_LEVEL: &str = "dependent_level";
const CURRENT_LEVEL: &str = "current_level";
const REQUIRED_LEVEL: &str = "required_level";

#[derive(Debug)]
pub struct TopologyInspect {
    graph: Digraph,
    synthetic_graph: Digraph,
    events: Option<RefCell<BoundedListNode>>,
    _node: inspect::Node,
    shadow: Arc<Mutex<ShadowBuffer>>,
}

#[derive(Debug)]
pub struct ElementData {
    current_level: Option<Either<inspect::UintProperty, inspect::StringProperty>>,
    required_level: Option<Either<inspect::UintProperty, inspect::StringProperty>>,
    // { LeaseId  => { Level => LeaseStatus } }
    leases: HashMap<LeaseID, LeaseData>,
    _node: inspect::Node,
    leases_node: inspect::Node,
}

#[derive(Debug)]
struct LeaseData {
    node: inspect::Node,
    status: Option<inspect::UintProperty>,
}

impl igraph::VertexMetadata for ElementData {
    type Id = ElementID;
    type EdgeMeta = DependencyData;
}

impl ElementData {
    const CURRENT_LEVEL: &str = "current_level";
    const REQUIRED_LEVEL: &str = "required_level";
    const NAME: &str = "name";
    const LEASES: &str = "leases";

    fn new(
        node: inspect::Node,
        name: &str,
        valid_levels: &[fpb::PowerLevel],
        current_level: fpb::PowerLevel,
        required_level: fpb::PowerLevel,
    ) -> Self {
        node.record_string(Self::NAME, name);
        Self::record_valid_levels(&node, valid_levels);
        let leases_node = node.create_child(Self::LEASES);
        Self {
            current_level: Some(Either::Left(
                node.create_uint(Self::CURRENT_LEVEL, current_level as u64),
            )),
            required_level: Some(Either::Left(
                node.create_uint(Self::REQUIRED_LEVEL, required_level as u64),
            )),
            leases: Default::default(),
            leases_node,
            _node: node,
        }
    }

    fn unsatisfiable(node: inspect::Node, name: &str, valid_levels: &[fpb::PowerLevel]) -> Self {
        node.record_string(Self::NAME, name);
        Self::record_valid_levels(&node, valid_levels);
        let leases_node = node.create_child(Self::LEASES);
        Self {
            current_level: Some(Either::Right(node.create_string(Self::CURRENT_LEVEL, UNSET))),
            required_level: Some(Either::Right(node.create_string(Self::REQUIRED_LEVEL, UNSET))),
            leases: Default::default(),
            _node: node,
            leases_node,
        }
    }

    fn synthetic(node: inspect::Node, name: &str, valid_levels: &[fpb::PowerLevel]) -> Self {
        node.record_string(Self::NAME, name);
        Self::record_valid_levels(&node, valid_levels);
        let leases_node = node.create_child(Self::LEASES);
        Self {
            current_level: None,
            required_level: None,
            _node: node,
            leases: Default::default(),
            leases_node,
        }
    }

    fn update_level(&self, update_level: &UpdateLevelInfo) {
        if let Some(level) = update_level.current {
            self.inner_update_level(&self.current_level, level);
        }
        if let Some(level) = update_level.required {
            self.inner_update_level(&self.required_level, level)
        }
    }

    fn inner_update_level(
        &self,
        property: &Option<Either<inspect::UintProperty, inspect::StringProperty>>,
        level: fpb::PowerLevel,
    ) {
        match property {
            None => {}
            Some(Either::Left(property)) => property.set(level as u64),
            Some(Either::Right(_)) => {
                unreachable!("we shouldn't be setting level for an unsatisfiable element");
            }
        }
    }

    fn create_lease(&mut self, lease_id: LeaseID, level: fpb::PowerLevel) {
        match self.leases.get_mut(&lease_id) {
            Some(_) => unreachable!("We can't call into create lease twice"),
            None => {
                let lease_node = self.leases_node.create_child(format!("{lease_id}"));
                lease_node.record_uint(LEVEL, level as u64);
                self.leases
                    .insert(lease_id.to_owned(), LeaseData { node: lease_node, status: None });
            }
        }
    }

    fn set_lease_status(&mut self, lease_id: LeaseID, status: fpb::LeaseStatus) {
        let status = status.into_primitive() as u64;
        match self.leases.get_mut(&lease_id) {
            None => unreachable!("we must have a lease created if we got here"),
            Some(LeaseData { status: Some(status_property), .. }) => {
                status_property.set(status);
            }
            Some(lease) => {
                lease.status = Some(lease.node.create_uint(STATUS, status));
            }
        }
    }

    fn remove_lease(&mut self, lease_id: LeaseID) {
        self.leases.remove(&lease_id);
    }

    fn record_valid_levels(node: &inspect::Node, valid_levels: &[fpb::PowerLevel]) {
        const VALID_LEVELS: &str = "valid_levels";
        let prop = node.create_uint_array(VALID_LEVELS, valid_levels.len());
        for (idx, v) in valid_levels.iter().enumerate() {
            prop.set(idx, *v);
        }
        node.record(prop);
    }
}

#[derive(Debug)]
pub struct DependencyData {
    // Map from dp_level => (rq_level, opportunistic)
    levels: HashMap<fpb::PowerLevel, DepLevelData>,
    node: inspect::Node,
}

impl DependencyData {
    fn new(
        meta_node: inspect::Node,
        dp_level: fpb::PowerLevel,
        rq_level: fpb::PowerLevel,
        is_opportunistic: bool,
    ) -> Self {
        let mut levels = HashMap::new();
        levels
            .insert(dp_level, DepLevelData::new(&meta_node, dp_level, rq_level, is_opportunistic));
        Self { levels, node: meta_node }
    }

    fn remove(&mut self, dp_level: fpb::PowerLevel) {
        self.levels.remove(&dp_level);
    }

    fn update_levels(
        &mut self,
        dp_level: fpb::PowerLevel,
        rq_level: fpb::PowerLevel,
        is_opportunistic: bool,
    ) {
        match self.levels.get_mut(&dp_level) {
            Some(data) => data.update(rq_level, is_opportunistic),
            None => {
                self.levels.insert(
                    dp_level,
                    DepLevelData::new(&self.node, dp_level, rq_level, is_opportunistic),
                );
            }
        }
    }
}

#[derive(Debug)]
struct DepLevelData {
    rq_level: inspect::UintProperty,
    opportunistic: Option<inspect::BoolProperty>,
    node: inspect::Node,
}

impl DepLevelData {
    fn new(
        parent: &inspect::Node,
        dp_level: fpb::PowerLevel,
        rq_level: fpb::PowerLevel,
        is_opportunistic: bool,
    ) -> Self {
        let node = parent.create_child(format!("{dp_level}"));
        Self {
            rq_level: node.create_uint(REQUIRED_LEVEL, rq_level as u64),
            opportunistic: is_opportunistic.then(|| node.create_bool(OPPORTUNISTIC, true)),
            node,
        }
    }

    fn update(&mut self, rq_level: fpb::PowerLevel, is_opportunistic: bool) {
        self.node.atomic_update(|_| {
            self.rq_level.set(rq_level as u64);
            match &self.opportunistic {
                Some(opportunistic) => opportunistic.set(is_opportunistic),
                None => {
                    self.opportunistic =
                        is_opportunistic.then(|| self.node.create_bool(OPPORTUNISTIC, true))
                }
            }
        })
    }
}

impl igraph::EdgeMetadata for DependencyData {}

impl TopologyInspect {
    pub fn new(node: inspect::Node, max_events: usize) -> Self {
        let graph = igraph::Digraph::new(&node, DigraphOpts::default());
        let synthetic_inspect_node = node.create_child("fuchsia.inspect.synthetic.Graph");
        let synthetic_graph =
            igraph::Digraph::new(&synthetic_inspect_node, igraph::DigraphOpts::default());
        node.record(synthetic_inspect_node);
        let shadow = Arc::new(Mutex::new(ShadowBuffer::new(max_events)));
        let mut events = None;
        if max_events != 0 {
            events =
                Some(RefCell::new(BoundedListNode::new(node.create_child("events"), max_events)));
            let shadow_weak = Arc::downgrade(&shadow);
            node.record_lazy_child("stats", move || {
                let shadow_weak = shadow_weak.clone();
                async move {
                    let inspector = inspect::Inspector::default();
                    if let Some(shadow) = shadow_weak.upgrade() {
                        let shadow_ref = shadow.lock().unwrap();
                        let root = inspector.root();
                        root.record_uint("event_capacity", max_events as u64);
                        let duration = shadow_ref.history_duration().into_nanos();
                        root.record_int("history_duration_ns", duration);
                        if shadow_ref.at_capacity() {
                            root.record_int("at_capacity_history_duration_ns", duration);
                        }
                    }
                    Ok(inspector)
                }
                .boxed()
            });
        }
        Self { graph, synthetic_graph, events, shadow, _node: node }
    }

    pub fn record_unsatisfiable_element(
        &self,
        id: ElementID,
        name: &str,
        valid_levels: &[fpb::PowerLevel],
    ) -> igraph::Vertex<ElementData> {
        let instant = zx::BootInstant::get();
        let vertex = self.graph.add_vertex(id.clone(), |meta_node| {
            ElementData::unsatisfiable(meta_node, name, valid_levels)
        });
        if let Some(events) = &self.events {
            events.borrow_mut().add_entry(|node| {
                node.record_int("@time", instant.into_nanos());
                node.record_child(ADD_ELEMENT_EVENT, |node| {
                    node.record_uint(ELEMENT_ID, *id);
                    node.record_string(CURRENT_LEVEL, UNSET);
                    node.record_string(REQUIRED_LEVEL, UNSET);
                });
            });
            self.shadow.lock().unwrap().add_entry(ShadowEvent { time: instant });
        }
        vertex
    }

    pub fn on_add_element(
        &self,
        element_id: ElementID,
        name: &str,
        synthetic: bool,
        valid_levels: Vec<fpb::PowerLevel>,
        current_level: fpb::PowerLevel,
        required_level: fpb::PowerLevel,
    ) -> igraph::Vertex<ElementData> {
        let instant = zx::BootInstant::get();
        let inspect_vertex = if synthetic {
            self.synthetic_graph.add_vertex(element_id, |meta_node| {
                ElementData::synthetic(meta_node, name, &valid_levels)
            })
        } else {
            let vertex = self.graph.add_vertex(element_id.clone(), |meta_node| {
                ElementData::new(meta_node, name, &valid_levels, current_level, required_level)
            });
            if let Some(events) = &self.events {
                events.borrow_mut().add_entry(|node| {
                    node.record_int("@time", instant.into_nanos());
                    node.record_child(ADD_ELEMENT_EVENT, |node| {
                        node.record_uint(ELEMENT_ID, *element_id);
                        node.record_uint(CURRENT_LEVEL, current_level as u64);
                        node.record_uint(REQUIRED_LEVEL, required_level as u64);
                    });
                });
            }
            vertex
        };
        inspect_vertex
    }

    pub fn on_add_dependency(
        &self,
        elements: &HashMap<ElementID, Element>,
        dep: &Dependency,
        is_assertive: bool,
    ) {
        let (dp_id, rq_id) = (dep.dependent.element_id, dep.requires.element_id);
        let (Some(dp), Some(rq)) = (elements.get(&dp_id), elements.get(&rq_id)) else {
            // elements[dp_id] and elements[rq_id] guaranteed by prior validation
            log::error!(dep:?; "Failed to add inspect for dependency.");
            return;
        };
        let (dp_level, rq_level) = (dep.dependent.level, dep.requires.level);
        let mut inspect_edges = dp.inspect_edges.borrow_mut();
        let is_opportunistic = !is_assertive;
        match inspect_edges.get_mut(&rq_id) {
            None => {
                let mut dp_vertex = dp.inspect_vertex.as_ref().unwrap().borrow_mut();
                let mut rq_vertex = rq.inspect_vertex.as_ref().unwrap().borrow_mut();
                let edge = dp_vertex.add_edge(&mut rq_vertex, |meta_node| {
                    DependencyData::new(meta_node, dp_level.level, rq_level.level, is_opportunistic)
                });
                inspect_edges.insert(rq_id, edge);
                self.maybe_record_event(dp, ADD_DEPENDENCY_EVENT, |node| {
                    node.record_uint(DEP_ELEMENT, *dp_id);
                    node.record_uint(REQ_ELEMENT, *rq_id);
                    node.record_uint(DEPENDENT_LEVEL, dp_level.level as u64);
                    node.record_uint(REQUIRED_LEVEL, rq_level.level as u64);
                    if is_opportunistic {
                        node.record_bool(OPPORTUNISTIC, true);
                    }
                });
            }
            Some(edge) => {
                edge.maybe_update_meta(|meta| {
                    meta.update_levels(dp_level.level, rq_level.level, is_opportunistic);
                });
                self.maybe_record_event(dp, UPDATE_DEP_LEVEL_EVENT, |node| {
                    node.record_uint(EDGE_ID, edge.id());
                    node.record_uint(DEPENDENT_LEVEL, dp_level.level as u64);
                    node.record_uint(REQUIRED_LEVEL, rq_level.level as u64);
                    if is_opportunistic {
                        node.record_bool(OPPORTUNISTIC, true);
                    }
                });
            }
        }
    }

    pub fn on_remove_dependency(&self, elements: &HashMap<ElementID, Element>, dep: &Dependency) {
        // elements[dp_id] and elements[rq_id] guaranteed by prior validation
        let (dp_id, rq_id) = (&dep.dependent.element_id, &dep.requires.element_id);
        let Some(dp) = elements.get(dp_id) else {
            log::error!(dp_id:?; "Missing element for removal");
            return;
        };
        let mut dp_edges = dp.inspect_edges.borrow_mut();
        let Some(edge) = dp_edges.get_mut(rq_id) else {
            log::error!(rq_id:?; "Missing edge for removal");
            return;
        };
        let dp_level = dep.dependent.level;
        edge.maybe_update_meta(|meta| {
            meta.remove(dp_level.level);
        });
        self.maybe_record_event(dp, REMOVE_DEP_EVENT, |node| {
            node.record_uint(EDGE_ID, edge.id());
            node.record_uint(DEPENDENT_LEVEL, dp_level.level as u64);
        });
    }

    pub fn on_update_level(&self, element: &Element, info: UpdateLevelInfo) {
        let Some(ref inspect_vertex) = element.inspect_vertex else {
            return;
        };
        let mut vertex = inspect_vertex.borrow_mut();
        vertex.meta().update_level(&info);
        self.maybe_record_event(element, UPDATE_LEVEL_EVENT, |node| {
            node.record_uint(ELEMENT_ID, *element.id);
            if let Some(level) = info.current {
                node.record_uint(CURRENT_LEVEL, level.into());
            }
            if let Some(level) = info.required {
                node.record_uint(REQUIRED_LEVEL, level.into());
            }
        });
    }

    pub fn on_remove_element(&self, element: &Element) {
        self.maybe_record_event(element, REMOVE_ELEMENT_EVENT, |node| {
            node.record_uint(ELEMENT_ID, *element.id);
            node.record_string(ELEMENT_NAME, &element.name);
        });
    }

    pub fn on_create_lease_and_claims(
        &self,
        element: &Element,
        lease_id: LeaseID,
        level: fpb::PowerLevel,
    ) {
        let Some(ref vertex) = element.inspect_vertex else {
            return;
        };
        vertex.borrow_mut().meta().create_lease(lease_id, level);
        self.maybe_record_event(element, CREATE_LEASE_EVENT, |node| {
            node.record_uint(ELEMENT_ID, *element.id);
            node.record_uint(LEASE_ID, *lease_id);
        });
    }

    pub fn on_update_lease_status(
        &self,
        element: &Element,
        lease: &Lease,
        status: &fpb::LeaseStatus,
    ) {
        let Some(ref inspect_vertex) = element.inspect_vertex else {
            return;
        };
        inspect_vertex.borrow_mut().meta().set_lease_status(lease.id, *status);
        self.maybe_record_event(element, UPDATE_LEASE_STATUS_EVENT, |node| {
            node.record_uint(ELEMENT_ID, *element.id);
            node.record_uint(LEASE_ID, *lease.id);
            node.record_uint(STATUS, status.into_primitive() as u64);
        });
    }

    pub fn on_drop_lease(&self, element: &Element, lease: &Lease) {
        let Some(ref inspect_vertex) = element.inspect_vertex else {
            return;
        };
        inspect_vertex.borrow_mut().meta().remove_lease(lease.id);
        self.maybe_record_event(element, REMOVE_LEASE_EVENT, |node| {
            node.record_uint(ELEMENT_ID, *element.id);
            node.record_uint(LEASE_ID, *lease.id);
            node.record_uint(LEVEL, lease.underlying_element_level.level.into());
        });
    }

    fn maybe_record_event(
        &self,
        element: &Element,
        event_name: &str,
        callback: impl FnOnce(&inspect::Node),
    ) {
        if let Some(events) = &self.events {
            if !element.synthetic {
                let instant = zx::BootInstant::get();
                events.borrow_mut().add_entry(|node| {
                    node.record_int(TIME, instant.into_nanos());
                    node.record_child(event_name, callback);
                });
                self.shadow.lock().unwrap().add_entry(ShadowEvent { time: instant })
            }
        }
    }
}

#[derive(Default)]
pub struct UpdateLevelInspectWriter {
    events: HashMap<ElementID, UpdateLevelInfo>,
}

pub struct UpdateLevelInfo {
    timestamp: zx::BootInstant,
    required: Option<fpb::PowerLevel>,
    current: Option<fpb::PowerLevel>,
}

impl UpdateLevelInfo {
    pub fn required(level: fpb::PowerLevel) -> Self {
        Self { timestamp: zx::BootInstant::get(), current: None, required: Some(level) }
    }
}

impl UpdateLevelInspectWriter {
    pub fn update_current_level(&mut self, element_id: ElementID, level: fpb::PowerLevel) {
        let entry = self.events.entry(element_id).or_insert_with(|| UpdateLevelInfo {
            timestamp: zx::BootInstant::get(),
            required: None,
            current: None,
        });
        entry.current = Some(level);
    }

    pub fn update_required_level(&mut self, element_id: ElementID, level: fpb::PowerLevel) {
        let entry = self.events.entry(element_id).or_insert_with(|| UpdateLevelInfo {
            timestamp: zx::BootInstant::get(),
            required: None,
            current: None,
        });
        entry.required = Some(level);
    }

    pub fn commit(self, topology: &Topology) {
        let mut events = self.events.into_iter().collect::<Vec<_>>();
        events.sort_by(|(_, a), (_, b)| a.timestamp.cmp(&b.timestamp));
        for (element_id, data) in events {
            if let Some(element) = topology.get_element(&element_id) {
                topology.inspect().on_update_level(element, data);
            }
        }
    }
}

#[derive(Debug)]
struct ShadowEvent {
    time: zx::BootInstant,
}

#[derive(Debug)]
struct ShadowBuffer {
    buffer: VecDeque<ShadowEvent>,
    capacity: usize,
}

impl ShadowBuffer {
    fn new(capacity: usize) -> Self {
        Self { buffer: VecDeque::with_capacity(capacity), capacity }
    }

    fn add_entry(&mut self, shadow_event: ShadowEvent) {
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(shadow_event);
    }

    fn history_duration(&self) -> zx::BootDuration {
        if self.buffer.len() < 2 {
            return zx::BootDuration::ZERO;
        }
        self.buffer.back().unwrap().time - self.buffer.front().unwrap().time
    }

    fn at_capacity(&self) -> bool {
        self.buffer.len() == self.capacity
    }
}
