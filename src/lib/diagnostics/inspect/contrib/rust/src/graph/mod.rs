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
//! `@to` which represents the vertex that has that incoming edge. Similar to vertices, it also
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

mod digraph;
mod edge;
pub(crate) mod events;
mod metadata;
mod types;
mod vertex;

pub use {
    digraph::{Digraph, DigraphOpts},
    edge::Edge,
    metadata::{EdgeGraphMetadata, Metadata, MetadataValue, VertexGraphMetadata},
    types::VertexId,
    vertex::Vertex,
};
