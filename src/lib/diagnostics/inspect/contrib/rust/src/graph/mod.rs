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
//!         "@topology": {
//!             "vertex-0": {
//!                 "@meta": {
//!                     "key-1": value,
//!                     ...
//!                     "key-i": value
//!                 },
//!                 "@relationships": {
//!                     "vertex-j": {
//!                         "@meta": {
//!                             "key-1": value,
//!                             ...
//!                             "key-i": value
//!                         }
//!                     },
//!                     ...
//!                     "vertex-k": {
//!                         "@meta": { ... },
//!                     },
//!                 }
//!             },
//!             ...
//!             "vertex-i": {
//!                 "@meta": { ...  },
//!                 "@relationships": { ... },
//!             }
//!         }
//!     }
//! }
//!
//! The `@topology` node contains all the vertices as children, each of the child names is the ID
//! of the vertex provided through the API.
//!
//! Each vertex has a metadata associated with it under the child `@meta`. Each of the child names
//! of meta is the key of the metadata field.
//!
//! Each vertex also has a child `@relationships` which contains all the outgoing edges of that
//! vertex. Each edge is identified by an incremental ID assigned at runtime and contains a property
//! `@to` which represents the vertex that has that incomimng edge. Similar to vertices, it also
//! has a `@meta` containing metadata key value pairs.
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
use fuchsia_inspect::{self as inspect, Property};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

/// A directed graph on top of Inspect.
pub struct Digraph<I> {
    _node: inspect::Node,
    topology_node: inspect::Node,
    _phantom: PhantomData<I>,
}

impl<I> Digraph<I>
where
    I: VertexId,
{
    /// Create a new directed graph under the given `parent` node.
    pub fn new(parent: &inspect::Node) -> Digraph<I> {
        let node = parent.create_child("fuchsia.inspect.Graph");
        let topology_node = node.create_child("@topology");
        Digraph { _node: node, topology_node, _phantom: PhantomData }
    }

    /// Add a new vertex to the graph identified by the given ID and with the given initial
    /// metadata.
    pub fn add_vertex<'a, M>(&self, id: I, initial_metadata: M) -> Vertex<I>
    where
        M: IntoIterator<Item = &'a MetadataItem<'a>>,
        M::IntoIter: Clone,
    {
        Vertex::new(id, &self.topology_node, initial_metadata)
    }
}

/// A metadata item used to initialize metadata key value pairs of nodes and edges.
pub struct MetadataItem<'a> {
    /// The key of the metadata field.
    key: Cow<'a, str>,
    /// The value of the metadata field.
    value: MetadataValue<'a>,
}

impl<'a> MetadataItem<'a> {
    /// Create a new metadata item with the given `key` and `value`.
    pub fn new(key: impl Into<Cow<'a, str>>, value: impl Into<MetadataValue<'a>>) -> Self {
        Self { key: key.into(), value: value.into() }
    }
}

/// A vertex of the graph. When this is dropped, all the outgoing edges and metadata fields will
/// removed from Inspect.
pub struct Vertex<I> {
    _node: inspect::Node,
    outgoing_edges_node: inspect::Node,
    metadata: GraphMetadata,
    incoming_edges: BTreeMap<usize, inspect::Node>,
    internal_id: usize,
    id: I,
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// The ID of a vertex.
pub trait VertexId {
    /// Fetches the ID of a vertex, which must have a string representation.
    fn get_id(&self) -> Cow<'_, str>;
}

impl<T: std::fmt::Display> VertexId for T {
    fn get_id(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{self}"))
    }
}

impl VertexId for str {
    fn get_id(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl<I> Vertex<I>
where
    I: VertexId,
{
    fn new<'a, M>(id: I, parent: &inspect::Node, initial_metadata: M) -> Self
    where
        M: IntoIterator<Item = &'a MetadataItem<'a>>,
    {
        let internal_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let metadata_iterator = initial_metadata.into_iter();
        parent.atomic_update(|parent| {
            let id_str = id.get_id();
            let node = parent.create_child(id_str.as_ref());
            let outgoing_edges_node = node.create_child("@relationships");
            let metadata = GraphMetadata::new(&node, metadata_iterator);
            Vertex {
                id,
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
        M: IntoIterator<Item = &'a MetadataItem<'a>>,
        M::IntoIter: Clone,
    {
        Edge::new(self, to, initial_metadata)
    }

    /// Get an exclusive reference to the metadata to modify it.
    pub fn meta(&mut self) -> &mut GraphMetadata {
        &mut self.metadata
    }
}

/// An Edge in the graph.
pub struct Edge {
    metadata: GraphMetadata,
    weak_node: inspect::Node,
}

impl Edge {
    fn new<'a, I, M>(from: &Vertex<I>, to: &mut Vertex<I>, initial_metadata: M) -> Self
    where
        I: VertexId,
        M: IntoIterator<Item = &'a MetadataItem<'a>>,
    {
        let to_id = to.id.get_id();
        let metadata_iterator = initial_metadata.into_iter();
        let (node, metadata) = from.outgoing_edges_node.atomic_update(|parent| {
            let node = parent.create_child(to_id.as_ref());
            let metadata = GraphMetadata::new(&node, metadata_iterator);
            (node, metadata)
        });
        // We store the REAL Node in the incoming edges and return an Edge holding a weak reference
        // to this real node. The raeson to do this is that we want to drop the Inspect node
        // associated with an Edge when any of the two vertices are dropped.
        let weak_node = node.clone_weak();
        to.incoming_edges.insert(from.internal_id, node);
        Self { metadata, weak_node }
    }

    /// Get an exclusive reference to the metadata to modify it.
    pub fn meta(&mut self) -> &mut GraphMetadata {
        &mut self.metadata
    }
}

impl Drop for Edge {
    fn drop(&mut self) {
        self.weak_node.forget();
    }
}

enum MetadataProperty {
    Int(inspect::IntProperty),
    Uint(inspect::UintProperty),
    Str(inspect::StringProperty),
    Bool(inspect::BoolProperty),
}

/// An enum encoding all the possible metadata value types and contents.
pub enum MetadataValue<'a> {
    Int(i64),
    Uint(u64),
    Str(&'a str),
    Bool(bool),
}

macro_rules! impl_from_for_metadata_value {
    ($([($($type:ty),*), $name:ident, $cast_type:ty]),*) => {
        $($(
            impl From<$type> for MetadataValue<'_> {
                fn from(value: $type) -> MetadataValue<'static> {
                    MetadataValue::$name(value as $cast_type)
                }
            }
        )*)*
    };
}

impl_from_for_metadata_value!([(i8, i16, i32, i64), Int, i64], [(u8, u16, u32, u64), Uint, u64]);

impl From<bool> for MetadataValue<'_> {
    fn from(value: bool) -> MetadataValue<'static> {
        MetadataValue::Bool(value)
    }
}

impl<'a> From<&'a str> for MetadataValue<'a> {
    fn from(value: &'a str) -> MetadataValue<'a> {
        MetadataValue::Str(value)
    }
}

/// The metadata of a vertex or edge in the graph.
pub struct GraphMetadata {
    map: BTreeMap<String, MetadataProperty>,
    node: inspect::Node,
}

impl GraphMetadata {
    fn new<'a>(
        parent: &inspect::Node,
        initial_metadata: impl Iterator<Item = &'a MetadataItem<'a>>,
    ) -> Self {
        let node = parent.create_child("@meta");
        let map = BTreeMap::default();
        let mut this = Self { node, map };
        for MetadataItem { key, value } in initial_metadata.into_iter() {
            this.insert_to_map(key.to_string(), value);
        }
        this
    }

    /// Set the value of the metadata field at `key` or insert a new one if not
    /// present.
    pub fn set<'a, 'b>(
        &mut self,
        key: impl Into<Cow<'a, str>>,
        value: impl Into<MetadataValue<'b>>,
    ) {
        let key: Cow<'a, str> = key.into();
        let value = value.into();
        match (self.map.get(key.as_ref()), value) {
            (Some(MetadataProperty::Int(property)), MetadataValue::Int(x)) => {
                property.set(x);
            }
            (Some(MetadataProperty::Uint(property)), MetadataValue::Uint(x)) => {
                property.set(x);
            }
            (Some(MetadataProperty::Bool(property)), MetadataValue::Bool(x)) => {
                property.set(x);
            }
            (Some(MetadataProperty::Str(property)), MetadataValue::Str(x)) => {
                property.set(x);
            }
            (_, value) => {
                self.insert_to_map(key.into_owned(), &value);
            }
        }
    }

    /// Remove the metadata field at `key`.
    pub fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    fn insert_to_map(&mut self, key: String, value: &MetadataValue<'_>) {
        match value {
            MetadataValue::Int(value) => {
                let property = self.node.create_int(&key, *value);
                self.map.insert(key, MetadataProperty::Int(property));
            }
            MetadataValue::Uint(value) => {
                let property = self.node.create_uint(&key, *value);
                self.map.insert(key, MetadataProperty::Uint(property));
            }
            MetadataValue::Str(value) => {
                let property = self.node.create_string(&key, value);
                self.map.insert(key, MetadataProperty::Str(property));
            }
            MetadataValue::Bool(value) => {
                let property = self.node.create_bool(&key, *value);
                self.map.insert(key, MetadataProperty::Bool(property));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;

    #[fuchsia::test]
    fn test_simple_graph() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root());

        // Create a new node with some properties.
        let vertex_foo = graph.add_vertex(
            "element-1",
            &[MetadataItem::new("name", "foo"), MetadataItem::new("level", 1u64)],
        );

        let mut vertex_bar = graph.add_vertex(
            "element-2",
            &[MetadataItem::new("name", "bar"), MetadataItem::new("level", 2i64)],
        );

        // Create a new edge.
        let _edge_foo_bar = vertex_foo.add_edge(
            &mut vertex_bar,
            &[
                MetadataItem::new("src", "on"),
                MetadataItem::new("dst", "off"),
                MetadataItem::new("type", "passive"),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "element-1": {
                        "@meta": {
                            name: "foo",
                            level: 1u64,
                        },
                        "@relationships": {
                            "element-2": {
                                "@meta": {
                                    "type": "passive",
                                    src: "on",
                                    dst: "off"
                                }
                            }
                        }
                    },
                    "element-2": {
                        "@meta": {
                            name: "bar",
                            level: 2i64,
                        },
                        "@relationships": {}
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_all_metadata_types_on_nodes() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root());

        // Create a new node with some properties.
        let mut vertex = graph.add_vertex(
            "test-node",
            &[
                MetadataItem::new("string_property", "i'm a string"),
                MetadataItem::new("int_property", 2i64),
                MetadataItem::new("uint_property", 4u64),
                MetadataItem::new("boolean_property", true),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node": {
                        "@meta": {
                            string_property: "i'm a string",
                            int_property: 2i64,
                            uint_property: 4u64,
                            boolean_property: true,
                        },
                        "@relationships": {}
                    },
                }
            }
        });

        // We can update all properties.
        vertex.meta().set("int_property", 1i64);
        vertex.meta().set("uint_property", 3u64);
        vertex.meta().set("boolean_property", false);
        vertex.meta().set("string_property", "hello world");

        // Or insert properties.
        vertex.meta().set("new_one", 123);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node": {
                        "@meta": {
                            string_property: "hello world",
                            int_property: 1i64,
                            uint_property: 3u64,
                            boolean_property: false,
                            new_one: 123i64,
                        },
                        "@relationships": {}
                    },
                }
            }
        });

        // Or remove them.
        vertex.meta().remove("string_property");
        vertex.meta().remove("int_property");
        vertex.meta().remove("uint_property");
        vertex.meta().remove("boolean_property");

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node": {
                        "@meta": {
                            new_one: 123i64,
                        },
                        "@relationships": {}
                    },
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_all_metadata_types_on_edges() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root());

        // Create a new node with some properties.
        let vertex_one = graph.add_vertex("test-node-1", &[]);
        let mut vertex_two = graph.add_vertex("test-node-2", &[]);
        let mut edge = vertex_one.add_edge(
            &mut vertex_two,
            &[
                MetadataItem::new("string_property", "i'm a string"),
                MetadataItem::new("int_property", 2i64),
                MetadataItem::new("uint_property", 4u64),
                MetadataItem::new("boolean_property", true),
            ],
        );

        // We can update all properties.
        edge.meta().set("int_property", 1i64);
        edge.meta().set("uint_property", 3u64);
        edge.meta().set("boolean_property", false);
        edge.meta().set("string_property", "hello world");

        // Or insert properties.
        edge.meta().set("new_one", 123);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node-1": {
                        "@meta": {},
                        "@relationships": {
                            "test-node-2": {
                                "@meta": {
                                    string_property: "hello world",
                                    int_property: 1i64,
                                    uint_property: 3u64,
                                    boolean_property: false,
                                    new_one: 123i64,
                                },
                            }
                        }
                    },
                    "test-node-2": {
                        "@meta": {},
                        "@relationships": {},
                    }
                }
            }
        });

        // Or remove them.
        edge.meta().remove("string_property");
        edge.meta().remove("int_property");
        edge.meta().remove("uint_property");
        edge.meta().remove("boolean_property");

        // Or even change the type.
        edge.meta().set("new_one", "no longer an int");

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node-1": {
                        "@meta": {},
                        "@relationships": {
                            "test-node-2": {
                                "@meta": {
                                    new_one: "no longer an int",
                                }
                            }
                        }
                    },
                    "test-node-2": {
                        "@meta": {},
                        "@relationships": {},
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_raii_semantics() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root());
        let mut foo = graph.add_vertex("foo", &[MetadataItem::new("hello", true)]);
        let bar = graph.add_vertex("bar", &[MetadataItem::new("hello", false)]);
        let mut baz = graph.add_vertex("baz", &[]);

        let edge = bar.add_edge(&mut foo, &[MetadataItem::new("hey", "hi")]);
        let _edge_to_baz = bar.add_edge(&mut baz, &[MetadataItem::new("good", "bye")]);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "foo": {
                        "@meta": {
                            hello: true,
                        },
                        "@relationships": {},
                    },
                    "bar": {
                        "@meta": {
                            hello: false,
                        },
                        "@relationships": {
                            "foo": {
                                "@meta": {
                                    hey: "hi",
                                },
                            },
                            "baz": {
                                "@meta": {
                                    good: "bye",
                                },
                            }
                        }
                    },
                    "baz": {
                        "@meta": {},
                        "@relationships": {}
                    }
                }
            }
        });

        // Dropping an edge removes it from the graph, along with all properties.
        drop(edge);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "foo": {
                        "@meta": {
                            hello: true,
                        },
                        "@relationships": {},
                    },
                    "bar": {
                        "@meta": {
                            hello: false,
                        },
                        "@relationships": {
                            "baz": {
                                "@meta": {
                                    good: "bye",
                                },
                            }
                        }
                    },
                    "baz": {
                        "@meta": {},
                        "@relationships": {}
                    }
                }
            }
        });

        // Dropping a node removes it from the graph along with all edges and properties.
        drop(bar);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "foo": {
                        "@meta": {
                            hello: true,
                        },
                        "@relationships": {}
                    },
                    "baz": {
                        "@meta": {},
                        "@relationships": {}
                    }
                }
            }
        });

        // Dropping all nodes leaves an empty graph.
        drop(foo);
        drop(baz);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {}
            }
        });
    }

    #[fuchsia::test]
    fn drop_target_semantics() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root());
        let vertex_one = graph.add_vertex("test-node-1", &[]);
        let mut vertex_two = graph.add_vertex("test-node-2", &[]);
        let _edge = vertex_one.add_edge(&mut vertex_two, &[]);
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node-1": {
                        "@meta": {},
                        "@relationships": {
                            "test-node-2": {
                                "@meta": {},
                            }
                        }
                    },
                    "test-node-2": {
                        "@meta": {},
                        "@relationships": {},
                    }
                }
            }
        });

        // Drop the target vertex.
        drop(vertex_two);

        // The edge is gone too regardless of us still holding it.
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "@topology": {
                    "test-node-1": {
                        "@meta": {},
                        "@relationships": {}
                    }
                }
            }
        });
    }
}
