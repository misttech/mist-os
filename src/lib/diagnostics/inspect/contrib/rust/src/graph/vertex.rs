// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    events::GraphObjectEventTracker, types::VertexMarker, Edge, Metadata, VertexGraphMetadata,
    VertexId,
};
use fuchsia_inspect as inspect;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

/// A vertex of the graph. When this is dropped, all the outgoing edges and metadata fields will
/// removed from Inspect.
#[derive(Debug)]
pub struct Vertex<I: VertexId> {
    _node: inspect::Node,
    metadata: VertexGraphMetadata<I>,
    pub(crate) incoming_edges: BTreeMap<u64, inspect::Node>,
    pub(crate) outgoing_edges_node: inspect::Node,
    pub(crate) internal_id: u64,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl<I> Vertex<I>
where
    I: VertexId,
{
    pub(crate) fn new<'a, M>(
        id: I,
        parent: &inspect::Node,
        initial_metadata: M,
        events_tracker: Option<GraphObjectEventTracker<VertexMarker<I>>>,
    ) -> Self
    where
        M: IntoIterator<Item = Metadata<'a>>,
    {
        let internal_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let metadata_iterator = initial_metadata.into_iter();
        parent.atomic_update(|parent| {
            let id_str = id.get_id();
            let node = parent.create_child(id_str.as_ref());
            let outgoing_edges_node = node.create_child("relationships");
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
        M: IntoIterator<Item = Metadata<'a>>,
    {
        Edge::new(self, to, initial_metadata, self.metadata.events_tracker().map(|e| e.for_edge()))
    }

    /// Get an exclusive reference to the metadata to modify it.
    pub fn meta(&mut self) -> &mut VertexGraphMetadata<I> {
        &mut self.metadata
    }

    pub(crate) fn id(&self) -> &I {
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
