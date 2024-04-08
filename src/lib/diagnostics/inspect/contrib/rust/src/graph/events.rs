// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{types::*, Metadata, MetadataValue};
use crate::{
    inspect_log,
    nodes::{BoundedListNode, NodeExt},
};
use fuchsia_inspect as inspect;
use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
};

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

    pub fn record_added<'a, M>(&self, id: &I, initial_metadata: M)
    where
        M: IntoIterator<Item = &'a Metadata<'a>>,
    {
        self.buffer.lock().unwrap().add_entry(|node| {
            node.record_time("@time");
            node.record_string("event", "add_vertex");
            node.record_string("id", id.get_id().as_ref());
            let meta_node = node.create_child("meta");
            record_metadata_items(&meta_node, initial_metadata.into_iter());
            node.record(meta_node);
        });
    }

    pub fn record_removed(&self, id: &str) {
        let mut buffer = self.buffer.lock().unwrap();
        inspect_log!(buffer, {
            event: "remove_vertex",
            id: id,
        });
    }
}

impl GraphObjectEventTracker<EdgeMarker> {
    pub fn record_added<'a, M>(&self, from: &str, to: &str, id: u64, initial_metadata: M)
    where
        M: IntoIterator<Item = &'a Metadata<'a>>,
    {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.add_entry(|node| {
            node.record_time("@time");
            node.record_string("event", "add_edge");
            node.record_string("from", from);
            node.record_string("to", to);
            node.record_uint("id", id);
            let meta_node = node.create_child("meta");
            record_metadata_items(&meta_node, initial_metadata.into_iter());
            node.record(meta_node);
        });
    }

    pub fn record_removed(&self, id: u64) {
        let mut buffer = self.buffer.lock().unwrap();
        inspect_log!(buffer, {
            event: "remove_edge",
            id: id,
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

fn record_metadata_items<'a, I>(node: &inspect::Node, metadata: I)
where
    I: Iterator<Item = &'a Metadata<'a>>,
{
    for meta_item in metadata {
        if !meta_item.track_events {
            continue;
        }
        match meta_item.value {
            MetadataValue::Int(value) => node.record_int(meta_item.key.as_ref(), value),
            MetadataValue::Uint(value) => node.record_uint(meta_item.key.as_ref(), value),
            MetadataValue::Double(value) => node.record_double(meta_item.key.as_ref(), value),
            MetadataValue::Str(ref value) => {
                node.record_string(meta_item.key.as_ref(), value.as_ref())
            }
            MetadataValue::Bool(value) => node.record_bool(meta_item.key.as_ref(), value),
        }
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
    async fn vertex_add() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        vertex_tracker.record_added(
            &123,
            &[
                Metadata::new("string_property", "i'm a string").track_events(),
                Metadata::new("int_property", 2i64).track_events(),
                Metadata::new("uint_property", 4u64).track_events(),
                Metadata::new("boolean_property", true).track_events(),
                Metadata::new("double_property", 2.5).track_events(),
                Metadata::new("untracked", 0),
            ],
        );
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "add_vertex",
                    id: "123",
                    meta: {
                        string_property: "i'm a string",
                        int_property: 2i64,
                        uint_property: 4u64,
                        boolean_property: true,
                        double_property: 2.5,
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    async fn vertex_remove() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        vertex_tracker.record_removed("20");
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "remove_vertex",
                    id: "20",
                }
            }
        });
    }

    #[fuchsia::test]
    async fn vertex_metadata_update() {
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
    async fn edge_add() {
        let inspector = inspect::Inspector::default();
        let tracker = GraphEventsTracker::new(inspector.root().create_child("events"), 1);
        let vertex_tracker = tracker.for_vertex::<u64>();
        let edge_tracker = vertex_tracker.for_edge();
        edge_tracker.record_added(
            "src",
            "dst",
            10,
            &[
                Metadata::new("string_property", "i'm a string").track_events(),
                Metadata::new("int_property", 2i64).track_events(),
                Metadata::new("uint_property", 4u64).track_events(),
                Metadata::new("boolean_property", true).track_events(),
                Metadata::new("double_property", 2.5).track_events(),
                Metadata::new("untracked", 0),
            ],
        );
        assert_data_tree!(inspector, root: {
            events: {
                "0": {
                    "@time": AnyProperty,
                    event: "add_edge",
                    from: "src",
                    to: "dst",
                    id: 10u64,
                    meta: {
                        string_property: "i'm a string",
                        int_property: 2i64,
                        uint_property: 4u64,
                        boolean_property: true,
                        double_property: 2.5,
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    async fn edge_remove() {
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
                    id: 20u64,
                }
            }
        });
    }

    #[fuchsia::test]
    async fn edge_metadata_update() {
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
    async fn circular_buffer_semantics() {
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
                    id: "30",
                },
                "2": {
                    "@time": AnyProperty,
                    event: "remove_vertex",
                    id: "40",
                }
            }
        });
    }
}
