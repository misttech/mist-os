// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//! # Inspect Graph
//!
//! This module provides an abstraction over a Directed Graph on Inspect.
//!
//! The graph has vertices and edges. Edges have an origin and destination vertex.
//! Each vertex and edge can have a set of key value pairs of associated metadata.
//!
//! The resulting graph has the following schema:
//!
//! {
//!     "fuchsia.inspect.Graph": {
//!         "topology": {
//!             "vertex-0": {
//!                 "meta": {
//!                     "key-1": value,
//!                     ...
//!                     "key-i": value
//!                 },
//!                 "relationships": {
//!                     "vertex-j": {
//!                         "meta": {
//!                             "key-1": value,
//!                             ...
//!                             "key-i": value
//!                         }
//!                     },
//!                     ...
//!                     "vertex-k": {
//!                         "meta": { ... },
//!                     },
//!                 }
//!             },
//!             ...
//!             "vertex-i": {
//!                 "meta": { ...  },
//!                 "relationships": { ... },
//!             }
//!         }
//!     }
//! }
//!
//! The `topology` node contains all the vertices as children, each of the child names is the ID
//! of the vertex provided through the API.
//!
//! Each vertex has a metadata associated with it under the child `meta`. Each of the child names
//! of meta is the key of the metadata field.
//!
//! Each vertex also has a child `relationships` which contains all the outgoing edges of that
//! vertex. Each edge is identified by an incremental ID assigned at runtime and contains a property
//! `@to` which represents the vertex that has that incomimng edge. Similar to vertices, it also
//! has a `meta` containing metadata key value pairs.
//!
//! ## Semantics
//!
//! This API follows regular inspect semantics with the following important detail: Dropping a
//! Vertex results in the deletion of all the associated metadata as well as all the associated
//! outgoing and incoming edges from the Inspect VMO. This is especially important for Edges
//! given that the program may still be holding an Edge struct, but if any of the nodes associated
//! with that edge is dropped, the Edge data will be considered as removed from the Inspect VMO and
//! operations on the Edge will be no-ops.
//!
//! ## Tracking events
//!
//! The API supports tracking changes to the Graph topology (add/remove edge/node) as well as
//! changes to selected metadata properties. By default, nothing is tracked. If you wish to track
//! events, then you must pass `DigraphOpts::default().track_events(N)` specifying the maximum number
//! of events that will be tracked in a circular buffer in which the oldest events are rolled out.
//!
//! Even when tracking events is enabled, by default no metadata properties are tracked. If you
//! wish to enable tracking a metadata property, call `.track_events()`  on the `Metadata`
//! passed when initializing the metadata of an edge or node.
//!
//! The events will be present on a `events` node under the `fuchsia.inspect.Graph` node.
//!
//! ### Add vertex
//!
//! An event tracking the addition of a vertex, contains the following properties:
//!
//! - `@time`: the time when the edge was added.
//! - `event`: the name of the event: `"add_vertex"`.
//! - `id`: the given vertex id.
//! - `meta`: a node containing the initial values of the metadata properties set to be tracked.
//!
//! ### Add edge
//!
//! An event tracking the addition of an edge, contains the following properties:
//!
//! - `@time`: the time when the edge was added.
//! - `from`: the ID of th origin vertex.
//! - `to`: the ID of th destination vertex.
//! - `event`: the name of the event: `"add_edge"`.
//! - `id`: an internally generated Edge ID. Every edge added to the graph will carry a unique
//!   incremental ID.
//! - `meta`: a node containing the initial values of the metadata properties set to be tracked.
//!
//! ### Update key
//!
//! An event tracking the update of a metadata value, contains the following properties:
//!
//! - `@time`: the time when the metadata value was updated.
//! - `event`: the name of the event: `"update_key"`.
//! - `key`: the name of the key that was updated.
//! - One of `edge_id` or `vertex_id` indicating the edge or vertex to which this metadata property
//!   belongs.
//! - `update`: the new value of the property.
//!
//! ### Remove vertex
//!
//! An event tracking the removal of a vertex, contains the following properties:
//!
//! - `@time`: the time when the edge was removed.
//! - `event`: the name of the event: `"remove_vertex"`.
//! - `id`: the given vertex id.
//!
//! ### Remove edge
//!
//! An event tracking the removal of an edge, contains the following properties:
//!
//! - `@time`: the time when the edge was removed.
//! - `event`: the name of the event: `"remove_edge"`.
//! - `id`: the internally generated Edge ID.
//!
use fuchsia_inspect as inspect;
use std::{
    collections::BTreeMap,
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

mod events;
mod metadata;
mod types;
use events::*;
use types::*;
pub use {
    metadata::{EdgeGraphMetadata, Metadata, MetadataValue, VertexGraphMetadata},
    types::VertexId,
};

/// A directed graph on top of Inspect.
pub struct Digraph<I> {
    _node: inspect::Node,
    topology_node: inspect::Node,
    events_tracker: Option<GraphEventsTracker>,
    _phantom: PhantomData<I>,
}

/// Options used to configure the `Digraph`.
#[derive(Default)]
pub struct DigraphOpts {
    max_events: usize,
}

impl DigraphOpts {
    /// Allows to track topology and metadata changes in the graph. This allows to reproduce
    /// previous states of the graph that led to the current one. Defaults to 0 which means that
    /// no events will be tracked. When not zero, this is the maximum number of events that will be
    /// recorded.
    pub fn track_events(mut self, events: usize) -> Self {
        self.max_events = events;
        self
    }
}

impl<I> Digraph<I>
where
    I: VertexId,
{
    /// Create a new directed graph under the given `parent` node.
    pub fn new(parent: &inspect::Node, options: DigraphOpts) -> Digraph<I> {
        let node = parent.create_child("fuchsia.inspect.Graph");
        let mut events_tracker = None;
        if options.max_events > 0 {
            let list_node = node.create_child("events");
            events_tracker = Some(GraphEventsTracker::new(list_node, options.max_events));
        }
        let topology_node = node.create_child("topology");
        Digraph { _node: node, topology_node, events_tracker, _phantom: PhantomData }
    }

    /// Add a new vertex to the graph identified by the given ID and with the given initial
    /// metadata.
    pub fn add_vertex<'a, M>(&self, id: I, initial_metadata: M) -> Vertex<I>
    where
        M: IntoIterator<Item = &'a Metadata<'a>>,
        M::IntoIter: Clone,
    {
        Vertex::new(
            id,
            &self.topology_node,
            initial_metadata,
            self.events_tracker.as_ref().map(|e| e.for_vertex()),
        )
    }
}

/// A vertex of the graph. When this is dropped, all the outgoing edges and metadata fields will
/// removed from Inspect.
pub struct Vertex<I: VertexId> {
    _node: inspect::Node,
    outgoing_edges_node: inspect::Node,
    metadata: VertexGraphMetadata<I>,
    incoming_edges: BTreeMap<u64, inspect::Node>,
    internal_id: u64,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl<I> Vertex<I>
where
    I: VertexId,
{
    fn new<'a, M>(
        id: I,
        parent: &inspect::Node,
        initial_metadata: M,
        events_tracker: Option<GraphObjectEventTracker<VertexMarker<I>>>,
    ) -> Self
    where
        M: IntoIterator<Item = &'a Metadata<'a>>,
        M::IntoIter: Clone,
    {
        let internal_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let metadata_iterator = initial_metadata.into_iter();
        parent.atomic_update(|parent| {
            let id_str = id.get_id();
            let node = parent.create_child(id_str.as_ref());
            let outgoing_edges_node = node.create_child("relationships");
            if let Some(ref events_tracker) = events_tracker {
                events_tracker.record_added(&id, metadata_iterator.clone());
            }
            let metadata = VertexGraphMetadata::new(&node, id, metadata_iterator, events_tracker);
            Vertex {
                internal_id,
                _node: node,
                outgoing_edges_node,
                metadata,
                incoming_edges: BTreeMap::new(),
            }
        })
    }

    /// Add a new edge to the graph originating at this vertex and going to the vertex `to` with the
    /// given metadata.
    pub fn add_edge<'a, M>(&self, to: &mut Vertex<I>, initial_metadata: M) -> Edge
    where
        M: IntoIterator<Item = &'a Metadata<'a>>,
        M::IntoIter: Clone,
    {
        Edge::new(self, to, initial_metadata, self.metadata.events_tracker().map(|e| e.for_edge()))
    }

    /// Get an exclusive reference to the metadata to modify it.
    pub fn meta(&mut self) -> &mut VertexGraphMetadata<I> {
        &mut self.metadata
    }

    fn id(&self) -> &I {
        &self.metadata.id()
    }
}

impl<I> Drop for Vertex<I>
where
    I: VertexId,
{
    fn drop(&mut self) {
        if let Some(ref events_tracker) = self.metadata.events_tracker() {
            events_tracker.record_removed(self.id().get_id().as_ref())
        }
    }
}

/// An Edge in the graph.
pub struct Edge {
    metadata: EdgeGraphMetadata,
    weak_node: inspect::Node,
}

impl Edge {
    fn new<'a, I, M>(
        from: &Vertex<I>,
        to: &mut Vertex<I>,
        initial_metadata: M,
        events_tracker: Option<GraphObjectEventTracker<EdgeMarker>>,
    ) -> Self
    where
        I: VertexId,
        M: IntoIterator<Item = &'a Metadata<'a>>,
        M::IntoIter: Clone,
    {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let to_id = to.id().get_id();
        let metadata_iterator = initial_metadata.into_iter();
        let (node, metadata) = from.outgoing_edges_node.atomic_update(|parent| {
            let node = parent.create_child(to_id.as_ref());
            node.record_uint("id", id);
            if let Some(ref events_tracker) = events_tracker {
                events_tracker.record_added(
                    from.id().get_id().as_ref(),
                    to_id.as_ref(),
                    id,
                    metadata_iterator.clone(),
                );
            }
            let metadata = EdgeGraphMetadata::new(&node, id, metadata_iterator, events_tracker);
            (node, metadata)
        });
        // We store the REAL Node in the incoming edges and return an Edge holding a weak reference
        // to this real node. The reason to do this is that we want to drop the Inspect node
        // associated with an Edge when any of the two vertices are dropped.
        let weak_node = node.clone_weak();
        to.incoming_edges.insert(from.internal_id, node);
        Self { metadata, weak_node }
    }

    /// Get an exclusive reference to the metadata to modify it.
    pub fn meta(&mut self) -> &mut EdgeGraphMetadata {
        &mut self.metadata
    }

    fn id(&self) -> u64 {
        self.metadata.id()
    }
}

impl Drop for Edge {
    fn drop(&mut self) {
        self.weak_node.forget();
        if let Some(ref events_tracker) = self.metadata.events_tracker() {
            events_tracker.record_removed(self.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};

    #[fuchsia::test]
    fn test_simple_graph() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        // Create a new node with some properties.
        let vertex_foo = graph
            .add_vertex("element-1", &[Metadata::new("name", "foo"), Metadata::new("level", 1u64)]);

        let mut vertex_bar = graph
            .add_vertex("element-2", &[Metadata::new("name", "bar"), Metadata::new("level", 2i64)]);

        // Create a new edge.
        let edge_foo_bar = vertex_foo.add_edge(
            &mut vertex_bar,
            &[
                Metadata::new("src", "on"),
                Metadata::new("dst", "off"),
                Metadata::new("type", "passive"),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "element-1": {
                        "meta": {
                            name: "foo",
                            level: 1u64,
                        },
                        "relationships": {
                            "element-2": {
                                "id": edge_foo_bar.id(),
                                "meta": {
                                    "type": "passive",
                                    src: "on",
                                    dst: "off"
                                }
                            }
                        }
                    },
                    "element-2": {
                        "meta": {
                            name: "bar",
                            level: 2i64,
                        },
                        "relationships": {}
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_all_metadata_types_on_nodes() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        // Create a new node with some properties.
        let mut vertex = graph.add_vertex(
            "test-node",
            &[
                Metadata::new("string_property", "i'm a string"),
                Metadata::new("int_property", 2i64),
                Metadata::new("uint_property", 4u64),
                Metadata::new("boolean_property", true),
                Metadata::new("double_property", 2.5),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node": {
                        "meta": {
                            string_property: "i'm a string",
                            int_property: 2i64,
                            uint_property: 4u64,
                            boolean_property: true,
                            double_property: 2.5f64,
                        },
                        "relationships": {}
                    },
                }
            }
        });

        // We can update all properties.
        vertex.meta().set("int_property", 1i64);
        vertex.meta().set("uint_property", 3u64);
        vertex.meta().set("double_property", 4.25);
        vertex.meta().set("boolean_property", false);
        vertex.meta().set("string_property", "hello world");

        // Or insert properties.
        vertex.meta().set("new_one", 123);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node": {
                        "meta": {
                            string_property: "hello world",
                            int_property: 1i64,
                            uint_property: 3u64,
                            double_property: 4.25f64,
                            boolean_property: false,
                            new_one: 123i64,
                        },
                        "relationships": {}
                    },
                }
            }
        });

        // Or remove them.
        vertex.meta().remove("string_property");
        vertex.meta().remove("int_property");
        vertex.meta().remove("uint_property");
        vertex.meta().remove("double_property");
        vertex.meta().remove("boolean_property");

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node": {
                        "meta": {
                            new_one: 123i64,
                        },
                        "relationships": {}
                    },
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_all_metadata_types_on_edges() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        // Create a new node with some properties.
        let vertex_one = graph.add_vertex("test-node-1", &[]);
        let mut vertex_two = graph.add_vertex("test-node-2", &[]);
        let mut edge = vertex_one.add_edge(
            &mut vertex_two,
            &[
                Metadata::new("string_property", "i'm a string"),
                Metadata::new("int_property", 2i64),
                Metadata::new("uint_property", 4u64),
                Metadata::new("boolean_property", true),
                Metadata::new("double_property", 2.5),
            ],
        );

        // We can update all properties.
        edge.meta().set("int_property", 1i64);
        edge.meta().set("uint_property", 3u64);
        edge.meta().set("double_property", 4.25);
        edge.meta().set("boolean_property", false);
        edge.meta().set("string_property", "hello world");

        // Or insert properties.
        edge.meta().set("new_one", 123);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "id": edge.id(),
                                "meta": {
                                    string_property: "hello world",
                                    int_property: 1i64,
                                    uint_property: 3u64,
                                    double_property: 4.25f64,
                                    boolean_property: false,
                                    new_one: 123i64,
                                },
                            }
                        }
                    },
                    "test-node-2": {
                        "meta": {},
                        "relationships": {},
                    }
                }
            }
        });

        // Or remove them.
        edge.meta().remove("string_property");
        edge.meta().remove("int_property");
        edge.meta().remove("uint_property");
        edge.meta().remove("double_property");
        edge.meta().remove("boolean_property");

        // Or even change the type.
        edge.meta().set("new_one", "no longer an int");

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "id": edge.id(),
                                "meta": {
                                    new_one: "no longer an int",
                                }
                            }
                        }
                    },
                    "test-node-2": {
                        "meta": {},
                        "relationships": {},
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_raii_semantics() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());
        let mut foo = graph.add_vertex("foo", &[Metadata::new("hello", true)]);
        let bar = graph.add_vertex("bar", &[Metadata::new("hello", false)]);
        let mut baz = graph.add_vertex("baz", &[]);

        let edge = bar.add_edge(&mut foo, &[Metadata::new("hey", "hi")]);
        let edge_to_baz = bar.add_edge(&mut baz, &[Metadata::new("good", "bye")]);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "foo": {
                        "meta": {
                            hello: true,
                        },
                        "relationships": {},
                    },
                    "bar": {
                        "meta": {
                            hello: false,
                        },
                        "relationships": {
                            "foo": {
                                "id": edge.id(),
                                "meta": {
                                    hey: "hi",
                                },
                            },
                            "baz": {
                                "id": edge_to_baz.id(),
                                "meta": {
                                    good: "bye",
                                },
                            }
                        }
                    },
                    "baz": {
                        "meta": {},
                        "relationships": {}
                    }
                }
            }
        });

        // Dropping an edge removes it from the graph, along with all properties.
        drop(edge);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "foo": {
                        "meta": {
                            hello: true,
                        },
                        "relationships": {},
                    },
                    "bar": {
                        "meta": {
                            hello: false,
                        },
                        "relationships": {
                            "baz": {
                                "id": edge_to_baz.id(),
                                "meta": {
                                    good: "bye",
                                },
                            }
                        }
                    },
                    "baz": {
                        "meta": {},
                        "relationships": {}
                    }
                }
            }
        });

        // Dropping a node removes it from the graph along with all edges and properties.
        drop(bar);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "foo": {
                        "meta": {
                            hello: true,
                        },
                        "relationships": {}
                    },
                    "baz": {
                        "meta": {},
                        "relationships": {}
                    }
                }
            }
        });

        // Dropping all nodes leaves an empty graph.
        drop(foo);
        drop(baz);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {}
            }
        });
    }

    #[fuchsia::test]
    fn drop_target_semantics() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());
        let vertex_one = graph.add_vertex("test-node-1", &[]);
        let mut vertex_two = graph.add_vertex("test-node-2", &[]);
        let edge = vertex_one.add_edge(&mut vertex_two, &[]);
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "id": edge.id(),
                                "meta": {},
                            }
                        }
                    },
                    "test-node-2": {
                        "meta": {},
                        "relationships": {},
                    }
                }
            }
        });

        // Drop the target vertex.
        drop(vertex_two);

        // The edge is gone too regardless of us still holding it.
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {}
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn track_events() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default().track_events(5));
        let mut vertex_one = graph.add_vertex(
            "test-node-1",
            &[Metadata::new("name", "foo"), Metadata::new("level", 1u64).track_events()],
        );
        let mut vertex_two = graph.add_vertex("test-node-2", &[Metadata::new("name", "bar")]);
        let mut edge = vertex_one.add_edge(
            &mut vertex_two,
            &[
                Metadata::new("some-property", 10i64).track_events(),
                Metadata::new("other", "not tracked"),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": {
                    "0": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "id": "test-node-1",
                        "meta": {
                            "level": 1u64,
                        }
                    },
                    "1": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "id": "test-node-2",
                        "meta": {}
                    },
                    "2": {
                        "@time": AnyProperty,
                        "from": "test-node-1",
                        "to": "test-node-2",
                        "event": "add_edge",
                        "id": edge.id(),
                        "meta": {
                            "some-property": 10i64,
                        }
                    },
                },
                "topology": {
                    "test-node-1": {
                        "meta": {
                            name: "foo",
                            level: 1u64,
                        },
                        "relationships": {
                            "test-node-2": {
                                "id": edge.id(),
                                "meta": {
                                    "some-property": 10i64,
                                    "other": "not tracked",
                                }
                            }
                        }
                    },
                    "test-node-2": {
                        "meta": {
                            name: "bar",
                        },
                        "relationships": {}
                    }
                }
            }
        });

        // The following updates will be reflected in the events.
        edge.meta().set("some-property", 123i64);
        vertex_one.meta().set("level", 2u64);

        // The following updates won't be reflected in the events.
        vertex_one.meta().set("name", "hello");
        vertex_two.meta().set("name", "world");
        edge.meta().set("other", "goodbye");

        //This change must roll out one event since it'll be the 6th one and we only track 5 events.
        vertex_one.meta().set("level", 3u64);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": {
                    "1": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "id": "test-node-2",
                        "meta": {}
                    },
                    "2": {
                        "@time": AnyProperty,
                        "from": "test-node-1",
                        "to": "test-node-2",
                        "event": "add_edge",
                        "id": edge.id(),
                        "meta": {
                            "some-property": 10i64,
                        }
                    },
                    "3": {
                        "@time": AnyProperty,
                        "event": "update_key",
                        "key": "some-property",
                        "update": 123i64,
                        "edge_id": edge.id(),
                    },
                    "4": {
                        "@time": AnyProperty,
                        "key": "level",
                        "update": 2u64,
                        "event": "update_key",
                        "vertex_id": "test-node-1",
                    },
                    "5": {
                        "@time": AnyProperty,
                        "event": "update_key",
                        "key": "level",
                        "update": 3u64,
                        "vertex_id": "test-node-1",
                    },
                },
                "topology": {
                    "test-node-1": {
                        "meta": {
                            name: "hello",
                            level: 3u64,
                        },
                        "relationships": {
                            "test-node-2": {
                                "id": edge.id(),
                                "meta": {
                                    "some-property": 123i64,
                                    "other": "goodbye",
                                }
                            }
                        }
                    },
                    "test-node-2": {
                        "meta": {
                            name: "world",
                        },
                        "relationships": {}
                    }
                }
            }
        });

        // Dropped events are tracked
        let edge_id = edge.id();
        drop(edge);
        drop(vertex_one);
        drop(vertex_two);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": contains {
                    "6": {
                        "@time": AnyProperty,
                        "event": "remove_edge",
                        "id": edge_id,
                    },
                    "7": {
                        "@time": AnyProperty,
                        "event": "remove_vertex",
                        "id": "test-node-1",
                    },
                    "8": {
                        "@time": AnyProperty,
                        "event": "remove_vertex",
                        "id": "test-node-2",
                    }
                },
                "topology": {}
            }
        });
    }
}
