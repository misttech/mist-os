// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::events::GraphEventsTracker;
use super::{Metadata, Vertex, VertexId};
use fuchsia_inspect as inspect;
use futures::FutureExt;
use std::marker::PhantomData;

/// A directed graph on top of Inspect.
#[derive(Debug)]
pub struct Digraph<I> {
    _node: inspect::Node,
    topology_node: inspect::Node,
    events_tracker: Option<GraphEventsTracker>,
    _stats_node: Option<inspect::LazyNode>,
    _phantom: PhantomData<I>,
}

/// Options used to configure the `Digraph`.
#[derive(Debug, Default)]
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
        let mut _stats_node = None;
        if options.max_events > 0 {
            let list_node = node.create_child("events");
            events_tracker = Some(GraphEventsTracker::new(list_node, options.max_events));
            let accessor_0 = events_tracker.as_ref().unwrap().history_stats_accessor();
            _stats_node = Some(node.create_lazy_child("stats", move || {
                let accessor_1 = accessor_0.clone();
                async move {
                    let inspector = inspect::Inspector::default();
                    let root = inspector.root();
                    root.record_uint("event_capacity", options.max_events as u64);
                    let duration = accessor_1.history_duration().into_nanos();
                    root.record_int("history_duration_ns", duration);
                    if accessor_1.at_capacity() {
                        root.record_int("at_capacity_history_duration_ns", duration);
                    }
                    Ok(inspector)
                }
                .boxed()
            }));
        }
        let topology_node = node.create_child("topology");
        Digraph { _node: node, topology_node, events_tracker, _stats_node, _phantom: PhantomData }
    }

    /// Add a new vertex to the graph identified by the given ID and with the given initial
    /// metadata.
    pub fn add_vertex<'a, M>(&self, id: I, initial_metadata: M) -> Vertex<I>
    where
        M: IntoIterator<Item = Metadata<'a>>,
    {
        Vertex::new(
            id,
            &self.topology_node,
            initial_metadata,
            self.events_tracker.as_ref().map(|e| e.for_vertex()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fuchsia_inspect::reader::snapshot::Snapshot;
    use fuchsia_inspect::{DiagnosticsHierarchyGetter, Inspector};
    use inspect_format::{BlockIndex, BlockType};
    use std::collections::BTreeSet;

    fn history_duration_from_events(
        inspector: &inspect::Inspector,
        first_index: &str,
        last_index: &str,
    ) -> i64 {
        let first_path = &["fuchsia.inspect.Graph", "events", first_index, "@time"];
        let first_time = inspector
            .get_diagnostics_hierarchy()
            .get_property_by_path(first_path)
            .and_then(|p| p.int())
            .unwrap();
        let last_path = &["fuchsia.inspect.Graph", "events", last_index, "@time"];
        let last_time = inspector
            .get_diagnostics_hierarchy()
            .get_property_by_path(last_path)
            .and_then(|p| p.int())
            .unwrap();
        last_time - first_time
    }

    #[fuchsia::test]
    fn test_simple_graph() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        // Create a new node with some properties.
        let mut vertex_foo = graph
            .add_vertex("element-1", [Metadata::new("name", "foo"), Metadata::new("level", 1u64)]);

        let mut vertex_bar = graph
            .add_vertex("element-2", [Metadata::new("name", "bar"), Metadata::new("level", 2i64)]);

        // Create a new edge.
        let edge_foo_bar = vertex_foo.add_edge(
            &mut vertex_bar,
            [
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
                                "edge_id": edge_foo_bar.id(),
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
            [
                Metadata::new("string_property", "i'm a string"),
                Metadata::new("int_property", 2i64),
                Metadata::new("uint_property", 4u64),
                Metadata::new("boolean_property", true),
                Metadata::new("double_property", 2.5),
                Metadata::new("intvec_property", vec![16i64, 32i64, 64i64]),
                Metadata::new("uintvec_property", vec![16u64, 32u64, 64u64]),
                Metadata::new("doublevec_property", vec![16.0, 32.0, 64.0]),
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
                            intvec_property: vec![16i64, 32i64, 64i64],
                            uintvec_property: vec![16u64, 32u64, 64u64],
                            doublevec_property: vec![16.0, 32.0, 64.0],
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
        vertex.meta().set("intvec_property", vec![15i64, 31i64, 63i64]);
        vertex.meta().set("uintvec_property", vec![15u64, 31u64, 63u64]);
        vertex.meta().set("doublevec_property", vec![15.0, 31.0, 63.0]);

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
                            // TODO(https://fxbug.dev/338660036): feature addition later.
                            intvec_property: vec![16i64, 32i64, 64i64],
                            uintvec_property: vec![16u64, 32u64, 64u64],
                            doublevec_property: vec![16.0, 32.0, 64.0],
                        },
                        "relationships": {}
                    },
                },
            }
        });

        // Or remove them.
        vertex.meta().remove("string_property");
        vertex.meta().remove("int_property");
        vertex.meta().remove("uint_property");
        vertex.meta().remove("double_property");
        vertex.meta().remove("boolean_property");
        vertex.meta().remove("intvec_property");
        vertex.meta().remove("uintvec_property");
        vertex.meta().remove("doublevec_property");

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
    fn test_dynamic_metadata_on_nodes() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default().track_events(8));

        // Create a new node with no properties.
        let mut vertex = graph.add_vertex("test-node", []);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                topology: {
                    "test-node": {
                        meta: {},
                        relationships: {}
                    },
                },
                events: {
                    "0": {
                         "@time": AnyProperty,
                         "event": "add_vertex",
                         "vertex_id": "test-node",
                         "meta": {}
                    },
                },
                stats: {
                    event_capacity: 8u64,
                    history_duration_ns: 0i64,
                }
            }
        });

        // We can dynamically set new properties and record their events.
        vertex.meta().set_and_track("int_property", 1i64);
        vertex.meta().set_and_track("uint_property", 3u64);
        vertex.meta().set_and_track("double_property", 4.25);
        vertex.meta().set_and_track("boolean_property", false);
        vertex.meta().set_and_track("string_property", "hello world");
        vertex.meta().set_and_track("intvec_property", vec![15i64, 31i64, 63i64]);
        vertex.meta().set_and_track("uintvec_property", vec![15u64, 31u64, 63u64]);
        vertex.meta().set_and_track("doublevec_property", vec![15.0, 31.0, 63.0]);

        let mut history_duration = history_duration_from_events(&inspector, "1", "8");
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                topology: {
                    "test-node": {
                        meta: {
                            string_property: "hello world",
                            int_property: 1i64,
                            uint_property: 3u64,
                            double_property: 4.25f64,
                            boolean_property: false,
                            intvec_property: vec![15i64, 31i64, 63i64],
                            uintvec_property: vec![15u64, 31u64, 63u64],
                            doublevec_property: vec![15.0, 31.0, 63.0],
                        },
                        relationships: {}
                    },
                },
                events: {
                    "1": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "int_property",
                        update: 1i64,
                        vertex_id: "test-node"
                    },
                    "2": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "uint_property",
                        update: 3u64,
                        vertex_id: "test-node"
                    },
                    "3": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "double_property",
                        update: 4.25,
                        vertex_id: "test-node"
                    },
                    "4": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "boolean_property",
                        update: false,
                        vertex_id: "test-node"
                    },
                    "5": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "string_property",
                        update: "hello world",
                        vertex_id: "test-node"
                    },
                    "6": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "intvec_property",
                        update: vec![15i64, 31i64, 63i64],
                        vertex_id: "test-node"
                    },
                    "7": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "uintvec_property",
                        update: vec![15u64, 31u64, 63u64],
                        "vertex_id": "test-node"
                    },
                    "8": {
                        "@time": AnyProperty,
                        event: "update_key",
                        key: "doublevec_property",
                        update: vec![15.0, 31.0, 63.0],
                        vertex_id: "test-node"
                    }
                },
                stats: {
                    event_capacity: 8u64,
                    history_duration_ns: history_duration,
                    at_capacity_history_duration_ns: history_duration,
                },
            }
        });

        // Or remove them.
        vertex.meta().remove_and_track("string_property");
        vertex.meta().remove_and_track("int_property");
        vertex.meta().remove_and_track("uint_property");
        vertex.meta().remove_and_track("double_property");
        vertex.meta().remove_and_track("boolean_property");
        vertex.meta().remove_and_track("intvec_property");
        vertex.meta().remove_and_track("uintvec_property");
        vertex.meta().remove_and_track("doublevec_property");

        history_duration = history_duration_from_events(&inspector, "9", "16");
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                topology: {
                    "test-node": {
                        meta: {},
                        relationships: {}
                    },
                },
                events: {
                    "9": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "string_property",
                        vertex_id: "test-node"
                    },
                    "10": {
                        "@time": AnyProperty,
                        "event": "drop_key",
                        key: "int_property",
                        vertex_id: "test-node"
                    },
                    "11": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "uint_property",
                        vertex_id: "test-node"
                    },
                    "12": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "double_property",
                        vertex_id: "test-node"
                    },
                    "13": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "boolean_property",
                        vertex_id: "test-node"
                    },
                    "14": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "intvec_property",
                        vertex_id: "test-node"
                    },
                    "15": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "uintvec_property",
                        vertex_id: "test-node"
                    },
                    "16": {
                        "@time": AnyProperty,
                        event: "drop_key",
                        key: "doublevec_property",
                        vertex_id: "test-node"
                    }
                },
                "stats": {
                    event_capacity: 8u64,
                    history_duration_ns: history_duration,
                    at_capacity_history_duration_ns: history_duration,
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
        let mut vertex_one = graph.add_vertex("test-node-1", []);
        let mut vertex_two = graph.add_vertex("test-node-2", []);
        let mut edge = vertex_one.add_edge(
            &mut vertex_two,
            [
                Metadata::new("string_property", "i'm a string"),
                Metadata::new("int_property", 2i64),
                Metadata::new("uint_property", 4u64),
                Metadata::new("boolean_property", true),
                Metadata::new("double_property", 2.5),
                Metadata::new("intvec_property", vec![16i64, 32i64, 64i64]),
                Metadata::new("uintvec_property", vec![16u64, 32u64, 64u64]),
                Metadata::new("doublevec_property", vec![16.0, 32.0, 64.0]),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "edge_id": edge.id(),
                                "meta": {
                                    string_property: "i'm a string",
                                    int_property: 2i64,
                                    uint_property: 4u64,
                                    double_property: 2.5,
                                    boolean_property: true,
                                    intvec_property: vec![16i64, 32i64, 64i64],
                                    uintvec_property: vec![16u64, 32u64, 64u64],
                                    doublevec_property: vec![16.0, 32.0, 64.0],
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

        // We can update all properties.
        edge.meta().set("int_property", 1i64);
        edge.meta().set("uint_property", 3u64);
        edge.meta().set("double_property", 4.25);
        edge.meta().set("boolean_property", false);
        edge.meta().set("string_property", "hello world");
        edge.meta().set("intvec_property", vec![15i64, 31i64, 63i64]);
        edge.meta().set("uintvec_property", vec![15u64, 31u64, 63u64]);
        edge.meta().set("doublevec_property", vec![15.0, 31.0, 63.0]);

        // Or insert properties.
        edge.meta().set("new_one", 123);

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "edge_id": edge.id(),
                                "meta": {
                                    string_property: "hello world",
                                    int_property: 1i64,
                                    uint_property: 3u64,
                                    double_property: 4.25f64,
                                    boolean_property: false,
                                    new_one: 123i64,
                                    // TODO(https://fxbug.dev/338660036): feature addition later.
                                    intvec_property: vec![16i64, 32i64, 64i64],
                                    uintvec_property: vec![16u64, 32u64, 64u64],
                                    doublevec_property: vec![16.0, 32.0, 64.0],
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
        edge.meta().remove("intvec_property");
        edge.meta().remove("uintvec_property");
        edge.meta().remove("doublevec_property");

        // Or even change the type.
        edge.meta().set("new_one", "no longer an int");

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "edge_id": edge.id(),
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
        let mut foo = graph.add_vertex("foo", [Metadata::new("hello", true)]);
        let mut bar = graph.add_vertex("bar", [Metadata::new("hello", false)]);
        let mut baz = graph.add_vertex("baz", []);

        let edge_foo = bar.add_edge(&mut foo, [Metadata::new("hey", "hi")]);
        let edge_to_baz = bar.add_edge(&mut baz, [Metadata::new("good", "bye")]);

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
                                "edge_id": edge_foo.id(),
                                "meta": {
                                    hey: "hi",
                                },
                            },
                            "baz": {
                                "edge_id": edge_to_baz.id(),
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
        drop(edge_foo);

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
                                "edge_id": edge_to_baz.id(),
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
                "topology": {},
            }
        });
    }

    #[fuchsia::test]
    fn drop_target_semantics() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());
        let mut vertex_one = graph.add_vertex("test-node-1", []);
        let mut vertex_two = graph.add_vertex("test-node-2", []);
        let edge = vertex_one.add_edge(&mut vertex_two, []);
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "topology": {
                    "test-node-1": {
                        "meta": {},
                        "relationships": {
                            "test-node-2": {
                                "edge_id": edge.id(),
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
            [Metadata::new("name", "foo"), Metadata::new("level", 1u64).track_events()],
        );
        let mut vertex_two = graph.add_vertex("test-node-2", [Metadata::new("name", "bar")]);
        let mut edge = vertex_one.add_edge(
            &mut vertex_two,
            [
                Metadata::new("some-property", 10i64).track_events(),
                Metadata::new("other", "not tracked"),
            ],
        );

        let mut history_duration = history_duration_from_events(&inspector, "0", "2");
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": {
                    "0": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "vertex_id": "test-node-1",
                        "meta": {
                            "level": 1u64,
                        }
                    },
                    "1": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "vertex_id": "test-node-2",
                        "meta": {}
                    },
                    "2": {
                        "@time": AnyProperty,
                        "from": "test-node-1",
                        "to": "test-node-2",
                        "event": "add_edge",
                        "edge_id": edge.id(),
                        "meta": {
                            "some-property": 10i64,
                        }
                    },
                },
                "stats": {
                    event_capacity: 5u64,
                    history_duration_ns: history_duration,
                },
                "topology": {
                    "test-node-1": {
                        "meta": {
                            name: "foo",
                            level: 1u64,
                        },
                        "relationships": {
                            "test-node-2": {
                                "edge_id": edge.id(),
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

        history_duration = history_duration_from_events(&inspector, "1", "5");
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": {
                    "1": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "vertex_id": "test-node-2",
                        "meta": {}
                    },
                    "2": {
                        "@time": AnyProperty,
                        "from": "test-node-1",
                        "to": "test-node-2",
                        "event": "add_edge",
                        "edge_id": edge.id(),
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
                "stats": {
                    event_capacity: 5u64,
                    history_duration_ns: history_duration,
                    at_capacity_history_duration_ns: history_duration,
                },
                "topology": {
                    "test-node-1": {
                        "meta": {
                            name: "hello",
                            level: 3u64,
                        },
                        "relationships": {
                            "test-node-2": {
                                "edge_id": edge.id(),
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

        // The list size is 5, so the events range from "4" to "8".
        history_duration = history_duration_from_events(&inspector, "4", "8");
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": contains {
                    "6": {
                        "@time": AnyProperty,
                        "event": "remove_edge",
                        "edge_id": edge_id,
                    },
                    "7": {
                        "@time": AnyProperty,
                        "event": "remove_vertex",
                        "vertex_id": "test-node-1",
                    },
                    "8": {
                        "@time": AnyProperty,
                        "event": "remove_vertex",
                        "vertex_id": "test-node-2",
                    }
                },
                "stats": {
                    event_capacity: 5u64,
                    history_duration_ns: history_duration,
                    at_capacity_history_duration_ns: history_duration,
                },
                "topology": {}
            }
        });
    }

    #[test]
    fn graph_with_nested_meta() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default().track_events(3));

        // Create a new node with some properties.
        let mut vertex = graph.add_vertex(
            "test-node",
            [
                Metadata::nested(
                    "nested",
                    [
                        Metadata::new("int", 2i64).track_events(),
                        Metadata::nested("nested2", [Metadata::new("boolean", false)]),
                    ],
                ),
                Metadata::nested("other_nested", [Metadata::new("string", "hello")]),
            ],
        );

        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": {
                    "0": {
                        "@time": AnyProperty,
                        "event": "add_vertex",
                        "vertex_id": "test-node",
                        "meta": {
                            "nested": {
                                "int": 2i64,
                            }
                        }
                    }
                },
                "stats": {
                    event_capacity: 3u64,
                    history_duration_ns: 0i64,
                },
                "topology": {
                    "test-node": {
                        "meta": {
                            nested: {
                                int: 2i64,
                                nested2: {
                                    boolean: false
                                }
                            },
                            other_nested: {
                                string: "hello",
                            }
                        },
                        "relationships": {}
                    },
                }
            }
        });

        vertex.meta().set("nested/int", 5i64);
        vertex.meta().set("nested/nested2/boolean", true);

        let history_duration = history_duration_from_events(&inspector, "0", "1");
        assert_data_tree!(inspector, root: {
            "fuchsia.inspect.Graph": {
                "events": {
                    "0": contains {},
                    "1": {
                        "@time": AnyProperty,
                        "key": "nested/int",
                        "update": 5i64,
                        "event": "update_key",
                        "vertex_id": "test-node",
                    },
                },
                "stats": {
                    event_capacity: 3u64,
                    history_duration_ns: history_duration,
                },
                "topology": {
                    "test-node": {
                        "meta": {
                            nested: {
                                int: 5i64,
                                nested2: {
                                    boolean: true
                                }
                            },
                            other_nested: {
                                string: "hello",
                            }
                        },
                        "relationships": {}
                    },
                }
            }
        });
    }

    #[fuchsia::test]
    fn validate_inspect_vmo_on_edge_drop() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());
        let mut vertex_a = graph.add_vertex("a", []);
        let mut vertex_b = graph.add_vertex("b", []);

        let blocks_before = non_free_blocks(&inspector);

        // Creating an edge and dropping it, is a no-op operation on the blocks in the VMO. All of
        // them should be gone.
        let edge_a_b = vertex_a.add_edge(&mut vertex_b, [Metadata::new("src", "on")]);
        drop(edge_a_b);

        let blocks_after = non_free_blocks(&inspector);
        assert_eq!(
            blocks_after.symmetric_difference(&blocks_before).collect::<BTreeSet<_>>(),
            BTreeSet::new()
        );
    }

    #[fuchsia::test]
    fn validate_inspect_vmo_on_dest_vertex_drop() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());
        let mut vertex_a = graph.add_vertex("a", []);

        let initial_blocks = non_free_blocks(&inspector);

        let mut vertex_b = graph.add_vertex("b", []);
        let _edge_a_b = vertex_a.add_edge(&mut vertex_b, [Metadata::new("src", "on")]);

        // Dropping the vertex should drop all of the edge and vertex_b blocks.
        drop(vertex_b);

        let after_blocks = non_free_blocks(&inspector);
        // There should be no difference between the remaining blocks and the original ones before
        // the operations executed.
        assert_eq!(
            after_blocks.symmetric_difference(&initial_blocks).collect::<BTreeSet<_>>(),
            BTreeSet::new()
        );
    }

    #[fuchsia::test]
    fn validate_inspect_vmo_on_source_vertex_drop() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        let initial_blocks = non_free_blocks(&inspector);
        let mut vertex_a = graph.add_vertex("a", []);

        let before_blocks = non_free_blocks(&inspector);
        let mut vertex_b = graph.add_vertex("b", []);
        let vertex_b_blocks = non_free_blocks(&inspector)
            .difference(&before_blocks)
            .cloned()
            .collect::<BTreeSet<_>>();

        let _edge_a_b = vertex_a.add_edge(&mut vertex_b, [Metadata::new("src", "on")]);

        // Dropping the vertex should drop all of the edge and vertex_a blocks, except for the
        // blocks shared with vertex_b (for example string references for the strings "meta" and
        // "relationships".
        drop(vertex_a);

        let mut expected = initial_blocks.union(&vertex_b_blocks).cloned().collect::<BTreeSet<_>>();
        // These two blocks are expected, since they are the "meta" and "relationships" strings.
        expected.insert((BlockIndex::new(14), BlockType::StringReference));
        expected.insert((BlockIndex::new(16), BlockType::StringReference));
        let after_blocks = non_free_blocks(&inspector);
        assert_eq!(after_blocks, expected);
    }

    fn non_free_blocks(inspector: &Inspector) -> BTreeSet<(BlockIndex, BlockType)> {
        let snapshot = Snapshot::try_from(inspector).unwrap();
        snapshot
            .scan()
            .filter(|block| block.block_type() != BlockType::Free)
            .map(|b| (b.index(), b.block_type()))
            .collect::<BTreeSet<_>>()
    }
}
