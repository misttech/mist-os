// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{events::GraphEventsTracker, Metadata, Vertex, VertexId};
use fuchsia_inspect as inspect;
use std::marker::PhantomData;

/// A directed graph on top of Inspect.
#[derive(Debug)]
pub struct Digraph<I> {
    _node: inspect::Node,
    topology_node: inspect::Node,
    events_tracker: Option<GraphEventsTracker>,
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

    #[fuchsia::test]
    fn test_simple_graph() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        // Create a new node with some properties.
        let vertex_foo = graph
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
                }
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
    fn test_all_metadata_types_on_edges() {
        let inspector = inspect::Inspector::default();

        // Create a new graph.
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());

        // Create a new node with some properties.
        let vertex_one = graph.add_vertex("test-node-1", []);
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
        let bar = graph.add_vertex("bar", [Metadata::new("hello", false)]);
        let mut baz = graph.add_vertex("baz", []);

        let edge = bar.add_edge(&mut foo, [Metadata::new("hey", "hi")]);
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
                                "edge_id": edge.id(),
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
                "topology": {}
            }
        });
    }

    #[fuchsia::test]
    fn drop_target_semantics() {
        let inspector = inspect::Inspector::default();
        let graph = Digraph::new(inspector.root(), DigraphOpts::default());
        let vertex_one = graph.add_vertex("test-node-1", []);
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
                "topology": {}
            }
        });
    }
}
