// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::types::*;
use super::MetadataValue;
use crate::nodes::BoundedListNode;
use fuchsia_inspect::{self as inspect, InspectTypeReparentable};
use fuchsia_sync::Mutex;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::{Arc, Weak};

pub struct MetaEventNode(inspect::Node);

impl MetaEventNode {
    pub fn new(parent: &inspect::Node) -> Self {
        Self(parent.create_child("meta"))
    }

    fn take_node(self) -> inspect::Node {
        self.0
    }
}

impl Deref for MetaEventNode {
    type Target = inspect::Node;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Data container for stats tracking of events in GraphEventsTracker.inner.buffer.
#[derive(Debug)]
pub struct ShadowEvent {
    time: zx::BootInstant,
}

// Aggregate for stats tracking of events in GraphEventsTracker.inner.buffer.
#[derive(Debug)]
pub struct ShadowBuffer {
    buffer: VecDeque<ShadowEvent>,
    capacity: usize,
}

impl ShadowBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { buffer: VecDeque::with_capacity(capacity), capacity }
    }
    // Add to internal vector, first trimming if it exceeds capacity.
    pub fn add_entry(&mut self, shadow_event: ShadowEvent) {
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(shadow_event);
    }
    // Report history duration.
    pub fn history_duration(&self) -> zx::BootDuration {
        if self.buffer.len() < 2 {
            return zx::BootDuration::ZERO;
        }
        self.buffer.back().unwrap().time - self.buffer.front().unwrap().time
    }
    // Report whether capacity reached.
    pub fn at_capacity(&self) -> bool {
        self.buffer.len() == self.capacity
    }
}

// Simple container to share a mutex.
#[derive(Debug)]
pub struct Inner {
    // List of events in chronological order.
    buffer: BoundedListNode,
    // Matching list of shadow data for each event in the write-only buffer.
    // Digraph reads `shadow` to create lazy stats over `buffer`.
    shadow: ShadowBuffer,
}

#[derive(Debug)]
pub struct GraphEventsTracker {
    // Write-only event list, and its "shadow" metadata for computing stats.
    inner: Arc<Mutex<Inner>>,
}

impl GraphEventsTracker {
    pub fn new(list_node: inspect::Node, max_events: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                buffer: BoundedListNode::new(list_node, max_events),
                shadow: ShadowBuffer::new(max_events),
            })),
        }
    }

    pub fn for_vertex<I>(&self) -> GraphObjectEventTracker<VertexMarker<I>> {
        GraphObjectEventTracker { inner: self.inner.clone(), _phantom: PhantomData }
    }

    // Allows closure-capture of weak reference for lazy node pattern.
    pub fn history_stats_accessor(&self) -> HistoryStatsAccessor {
        HistoryStatsAccessor(Arc::downgrade(&self.inner))
    }
}

#[derive(Clone)]
pub struct HistoryStatsAccessor(Weak<Mutex<Inner>>);

impl HistoryStatsAccessor {
    pub fn history_duration(&self) -> zx::BootDuration {
        self.0
            .upgrade()
            .map_or(zx::BootDuration::ZERO, |inner| inner.lock().shadow.history_duration())
    }
    pub fn at_capacity(&self) -> bool {
        self.0.upgrade().map_or(false, |inner| inner.lock().shadow.at_capacity())
    }
}

#[derive(Debug)]
pub struct GraphObjectEventTracker<T> {
    inner: Arc<Mutex<Inner>>,
    _phantom: PhantomData<T>,
}

impl<I> GraphObjectEventTracker<VertexMarker<I>>
where
    I: VertexId,
{
    pub fn for_edge(&self) -> GraphObjectEventTracker<EdgeMarker> {
        GraphObjectEventTracker { inner: self.inner.clone(), _phantom: PhantomData }
    }

    pub fn record_added(&self, id: &I, meta_event_node: MetaEventNode) {
        let meta_event_node = meta_event_node.take_node();
        let instant = zx::BootInstant::get();
        let mut inner = self.inner.lock();
        inner.buffer.add_entry(|node| {
            node.record_int("@time", instant.into_nanos());
            node.record_string("event", "add_vertex");
            node.record_string("vertex_id", id.get_id().as_ref());
            let _ = meta_event_node.reparent(node);
            node.record(meta_event_node);
        });
        inner.shadow.add_entry(ShadowEvent { time: instant });
    }

    pub fn record_removed(&self, id: &str) {
        let instant = zx::BootInstant::get();
        let mut inner = self.inner.lock();
        inner.buffer.add_entry(|node| {
            node.record_int("@time", instant.into_nanos());
            node.record_string("event", "remove_vertex");
            node.record_string("vertex_id", id.get_id().as_ref());
        });
        inner.shadow.add_entry(ShadowEvent { time: instant });
    }
}

impl GraphObjectEventTracker<EdgeMarker> {
    pub fn record_added(&self, from: &str, to: &str, id: u64, meta_event_node: MetaEventNode) {
        let meta_event_node = meta_event_node.take_node();
        let instant = zx::BootInstant::get();
        let mut inner = self.inner.lock();
        inner.buffer.add_entry(|node| {
            node.record_int("@time", instant.into_nanos());
            node.record_string("event", "add_edge");
            node.record_string("from", from);
            node.record_string("to", to);
            node.record_uint("edge_id", id);
            let _ = meta_event_node.reparent(node);
            node.record(meta_event_node);
        });
        inner.shadow.add_entry(ShadowEvent { time: instant });
    }

    pub fn record_removed(&self, id: u64) {
        let instant = zx::BootInstant::get();
        let mut inner = self.inner.lock();
        inner.buffer.add_entry(|node| {
            node.record_int("@time", instant.into_nanos());
            node.record_string("event", "remove_edge");
            node.record_uint("edge_id", id);
        });
        inner.shadow.add_entry(ShadowEvent { time: instant });
    }
}

impl<T> GraphObjectEventTracker<T>
where
    T: GraphObject,
{
    pub fn metadata_updated(&self, id: &T::Id, key: &str, value: &MetadataValue<'_>) {
        let instant = zx::BootInstant::get();
        let mut inner = self.inner.lock();
        inner.buffer.add_entry(|node| {
            node.record_int("@time", instant.into_nanos());
            node.record_string("event", "update_key");
            node.record_string("key", key);
            value.record_inspect(node, "update");
            T::write_to_node(node, id);
        });
        inner.shadow.add_entry(ShadowEvent { time: instant });
    }

    pub fn metadata_dropped(&self, id: &T::Id, key: &str) {
        let instant = zx::BootInstant::get();
        let mut inner = self.inner.lock();
        inner.buffer.add_entry(|node| {
            node.record_int("@time", instant.into_nanos());
            node.record_string("event", "drop_key");
            node.record_string("key", key);
            T::write_to_node(node, id);
        });
        inner.shadow.add_entry(ShadowEvent { time: instant });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fuchsia_inspect::DiagnosticsHierarchyGetter;

    impl GraphEventsTracker {
        fn shadow_buffer_len(&self) -> usize {
            self.inner.lock().shadow.buffer.len()
        }
        fn shadow_time_of(&self, index: usize) -> zx::BootInstant {
            self.inner.lock().shadow.buffer.get(index).unwrap().time
        }
    }

    #[test]
    fn tracker_starts_empty() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        assert_data_tree!(inspector, root: {
            events: {}
        });
        assert_eq!(tracker.shadow_buffer_len(), 0);
    }

    fn get_time(inspector: &inspect::Inspector, path: &[&str]) -> zx::BootInstant {
        let instant = inspector
            .get_diagnostics_hierarchy()
            .get_property_by_path(path)
            .and_then(|p| p.int())
            .unwrap();
        zx::BootInstant::from_nanos(instant)
    }

    #[fuchsia::test]
    fn vertex_add() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        let meta_event_node = MetaEventNode::new(inspector.root());
        meta_event_node.record_bool("placeholder", true);
        vertex_tracker.record_added(&123, meta_event_node);
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "add_vertex",
                    vertex_id: "123",
                    meta: {
                        placeholder: true,
                    }
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 1);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
    }

    #[fuchsia::test]
    fn vertex_remove() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        vertex_tracker.record_removed("20");
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "remove_vertex",
                    vertex_id: "20",
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 1);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
    }

    #[fuchsia::test]
    fn vertex_metadata_update() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        vertex_tracker.metadata_updated(&10, "foo", &MetadataValue::Uint(3));
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "update_key",
                    vertex_id: "10",
                    key: "foo",
                    update: 3u64,
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 1);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
    }

    #[fuchsia::test]
    fn vertex_metadata_drop() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 2);
        let vertex_tracker = tracker.for_vertex::<u64>();
        vertex_tracker.metadata_updated(&10, "foo", &MetadataValue::Uint(3));
        vertex_tracker.metadata_dropped(&10, "foo");
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "update_key",
                    vertex_id: "10",
                    key: "foo",
                    update: 3u64,
                },
                "1": {
                    "@time": AnyProperty,
                    event: "drop_key",
                    vertex_id: "10",
                    key: "foo",
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 2);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
        assert_eq!(get_time(&inspector, &["events", "1", "@time"]), tracker.shadow_time_of(1));
    }

    #[fuchsia::test]
    fn edge_add() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        let edge_tracker = vertex_tracker.for_edge();
        let meta_event_node = MetaEventNode::new(inspector.root());
        meta_event_node.record_bool("placeholder", true);
        edge_tracker.record_added("src", "dst", 10, meta_event_node);
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "add_edge",
                    from: "src",
                    to: "dst",
                    edge_id: 10u64,
                    meta: {
                        placeholder: true,
                    }
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 1);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
    }

    #[fuchsia::test]
    fn edge_remove() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        let edge_tracker = vertex_tracker.for_edge();
        edge_tracker.record_removed(20);
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "remove_edge",
                    edge_id: 20u64,
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 1);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
    }

    #[fuchsia::test]
    fn edge_metadata_update() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        let edge_tracker = vertex_tracker.for_edge();
        edge_tracker.metadata_updated(&10, "foo", &MetadataValue::Uint(3));
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "update_key",
                    edge_id: 10u64,
                    key: "foo",
                    update: 3u64,
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 1);
        assert_eq!(get_time(&inspector, &["events", "0", "@time"]), tracker.shadow_time_of(0));
    }

    #[fuchsia::test]
    fn circular_buffer_semantics() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 2);
        let vertex_tracker = tracker.for_vertex::<u64>();
        vertex_tracker.record_removed("20");
        vertex_tracker.record_removed("30");
        vertex_tracker.record_removed("40");
        assert_data_tree!(inspector, root: {
            events: {
                "1": {
                    "@time": AnyProperty,
                    event: "remove_vertex",
                    vertex_id: "30",
                },
                "2": {
                    "@time": AnyProperty,
                    event: "remove_vertex",
                    vertex_id: "40",
                }
            }
        });
        assert_eq!(tracker.shadow_buffer_len(), 2);
        assert_eq!(get_time(&inspector, &["events", "1", "@time"]), tracker.shadow_time_of(0));
        assert_eq!(get_time(&inspector, &["events", "2", "@time"]), tracker.shadow_time_of(1));
    }
}
