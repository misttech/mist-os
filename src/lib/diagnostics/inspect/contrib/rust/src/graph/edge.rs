// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::events::GraphObjectEventTracker;
use super::types::{EdgeMarker, VertexId};
use super::{EdgeGraphMetadata, Metadata, Vertex};
use fuchsia_inspect as inspect;
use fuchsia_sync::{RwLock, RwLockWriteGuard};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

/// An Edge in the graph.
#[derive(Debug)]
pub struct Edge {
    state: Arc<RwLock<EdgeState>>,
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
        M: IntoIterator<Item = Metadata<'a>>,
    {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let to_id = to.id().get_id();
        let metadata_iterator = initial_metadata.into_iter();
        let (node, metadata) = from.outgoing_edges_node.atomic_update(|parent| {
            let node = parent.create_child(to_id.as_ref());
            node.record_uint("edge_id", id);
            let metadata = EdgeGraphMetadata::new(
                &node,
                id,
                metadata_iterator,
                events_tracker,
                from.id().get_id().as_ref(),
                to_id.as_ref(),
            );
            (node, metadata)
        });
        let state = Arc::new(RwLock::new(EdgeState::Active { metadata, _node: node }));
        Self { state }
    }

    /// Get an exclusive reference to the metadata to modify it.
    pub fn meta(&mut self) -> EdgeGraphMetadataRef<'_> {
        let lock = self.state.write();
        EdgeGraphMetadataRef { lock }
    }

    pub(crate) fn id(&self) -> u64 {
        self.state.read().metadata().id()
    }

    pub(crate) fn weak_ref(&self) -> WeakEdgeRef {
        WeakEdgeRef(Arc::downgrade(&self.state))
    }
}

pub struct EdgeGraphMetadataRef<'a> {
    lock: RwLockWriteGuard<'a, EdgeState>,
}

impl Deref for EdgeGraphMetadataRef<'_> {
    type Target = EdgeGraphMetadata;

    fn deref(&self) -> &Self::Target {
        self.lock.metadata()
    }
}

impl DerefMut for EdgeGraphMetadataRef<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.lock.metadata_mut()
    }
}

#[derive(Debug)]
enum EdgeState {
    Active { metadata: EdgeGraphMetadata, _node: inspect::Node },
    Gone { metadata: EdgeGraphMetadata },
}

impl EdgeState {
    pub fn mark_as_gone(&mut self) {
        match self {
            Self::Active { metadata, .. } => {
                let id = metadata.id();
                *self = Self::Gone { metadata: EdgeGraphMetadata::noop(id) };
            }
            Self::Gone { .. } => {}
        }
    }

    fn metadata_mut(&mut self) -> &mut EdgeGraphMetadata {
        match self {
            EdgeState::Active { metadata, .. } | EdgeState::Gone { metadata, .. } => metadata,
        }
    }

    fn metadata(&self) -> &EdgeGraphMetadata {
        match self {
            EdgeState::Active { metadata, .. } | EdgeState::Gone { metadata, .. } => metadata,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WeakEdgeRef(Weak<RwLock<EdgeState>>);

impl WeakEdgeRef {
    pub fn mark_as_gone(&self) {
        if let Some(value) = self.0.upgrade() {
            value.write().mark_as_gone();
        }
    }

    pub fn is_valid(&self) -> bool {
        self.0.upgrade().map(|v| matches!(*v.read(), EdgeState::Active { .. })).unwrap_or(false)
    }
}

impl Drop for Edge {
    fn drop(&mut self) {
        if let Some(events_tracker) = self.state.read().metadata().events_tracker() {
            events_tracker.record_removed(self.id());
        }
    }
}
