// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    events::GraphObjectEventTracker,
    types::{EdgeMarker, VertexId},
    EdgeGraphMetadata, Metadata, Vertex,
};
use fuchsia_inspect as inspect;
use std::sync::atomic::{AtomicU64, Ordering};

/// An Edge in the graph.
#[derive(Debug)]
pub struct Edge {
    metadata: EdgeGraphMetadata,
    weak_node: inspect::Node,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl Edge {
    pub(crate) fn new<'a, I, M>(
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
            node.record_uint("edge_id", id);
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

    pub(crate) fn id(&self) -> u64 {
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
