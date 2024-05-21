// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{types::*, MetadataValue};
use crate::{
    inspect_log,
    nodes::{BoundedListNode, NodeExt},
};
use fuchsia_inspect::{self as inspect, InspectTypeReparentable};
use std::{
    marker::PhantomData,
    ops::Deref,
    sync::{Arc, Mutex},
};

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

#[derive(Debug)]
pub struct GraphEventsTracker {
    buffer: Arc<Mutex<BoundedListNode>>,
}

impl GraphEventsTracker {
    pub fn new(list_node: inspect::Node, max_events: usize) -> Self {
        Self { buffer: Arc::new(Mutex::new(BoundedListNode::new(list_node, max_events))) }
    }

    pub fn for_vertex<I>(&self) -> GraphObjectEventTracker<VertexMarker<I>> {
        GraphObjectEventTracker { buffer: self.buffer.clone(), _phantom: PhantomData }
    }
}

#[derive(Debug)]
pub struct GraphObjectEventTracker<T> {
    buffer: Arc<Mutex<BoundedListNode>>,
    _phantom: PhantomData<T>,
}

impl<I> GraphObjectEventTracker<VertexMarker<I>>
where
    I: VertexId,
{
    pub fn for_edge(&self) -> GraphObjectEventTracker<EdgeMarker> {
        GraphObjectEventTracker { buffer: self.buffer.clone(), _phantom: PhantomData }
    }

    pub fn record_added(&self, id: &I, meta_event_node: MetaEventNode) {
        let meta_event_node = meta_event_node.take_node();
        self.buffer.lock().unwrap().add_entry(|node| {
            node.record_time("@time");
            node.record_string("event", "add_vertex");
            node.record_string("vertex_id", id.get_id().as_ref());
            let _ = meta_event_node.reparent(&node);
            node.record(meta_event_node);
        });
    }

    pub fn record_removed(&self, id: &str) {
        let mut buffer = self.buffer.lock().unwrap();
        inspect_log!(buffer, {
            event: "remove_vertex",
            vertex_id: id,
        });
    }
}

impl GraphObjectEventTracker<EdgeMarker> {
    pub fn record_added(&self, from: &str, to: &str, id: u64, meta_event_node: MetaEventNode) {
        let mut buffer = self.buffer.lock().unwrap();
        let meta_event_node = meta_event_node.take_node();
        buffer.add_entry(|node| {
            node.record_time("@time");
            node.record_string("event", "add_edge");
            node.record_string("from", from);
            node.record_string("to", to);
            node.record_uint("edge_id", id);
            let _ = meta_event_node.reparent(&node);
            node.record(meta_event_node);
        });
    }

    pub fn record_removed(&self, id: u64) {
        let mut buffer = self.buffer.lock().unwrap();
        inspect_log!(buffer, {
            event: "remove_edge",
            edge_id: id,
        });
    }
}

impl<T> GraphObjectEventTracker<T>
where
    T: GraphObject,
{
    pub fn metadata_updated(&self, id: &T::Id, key: &str, value: &MetadataValue<'_>) {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.add_entry(|node| {
            node.record_time("@time");
            node.record_string("event", "update_key");
            node.record_string("key", key);
            value.record_inspect(&node, "update");
            T::write_to_node(node, id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};

    #[test]
    fn tracker_starts_empty() {
        let inspector = inspect::Inspector::default();
        let _tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        assert_data_tree!(inspector, root: {
            events: {}
        });
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
    }
}
