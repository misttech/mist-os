// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{FsNode, FsNodeHandle, WeakFsNodeHandle};
use starnix_lifecycle::AtomicU64Counter;
use starnix_sync::Mutex;
use starnix_uapi::errors::Errno;
use starnix_uapi::ino_t;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

pub struct FsNodeCache {
    /// The next node ID to allocate.
    next_ino: Option<AtomicU64Counter>,

    /// The FsNodes that have been created for this file system.
    nodes: Mutex<HashMap<ino_t, WeakFsNodeHandle>>,
}

impl Default for FsNodeCache {
    fn default() -> Self {
        Self::new(false)
    }
}

impl FsNodeCache {
    pub fn new(uses_external_node_ids: bool) -> Self {
        Self {
            next_ino: if uses_external_node_ids { None } else { Some(AtomicU64Counter::new(1)) },
            nodes: Mutex::new(HashMap::new()),
        }
    }

    pub fn uses_external_node_ids(&self) -> bool {
        self.next_ino.is_none()
    }

    pub fn allocate_ino(&self) -> Option<ino_t> {
        self.next_ino.as_ref().map(|counter| counter.next())
    }

    pub fn allocate_ino_range(&self, size: usize) -> Option<Range<ino_t>> {
        assert!(size > 0);
        self.next_ino.as_ref().map(|counter| {
            let start = counter.add(size as u64);
            Range { start: start as ino_t, end: start + size as ino_t }
        })
    }

    pub fn insert_node(&self, node: &FsNodeHandle) {
        let node_key = node.node_key();
        let mut nodes = self.nodes.lock();
        nodes.insert(node_key, Arc::downgrade(node));
    }

    pub fn remove_node(&self, node: &FsNode) {
        let node_key = node.node_key();
        let mut nodes = self.nodes.lock();
        if let Some(weak_node) = nodes.get(&node_key) {
            if weak_node.strong_count() == 0 {
                nodes.remove(&node_key);
            }
        }
    }

    pub fn get_and_validate_or_create_node<V, C>(
        &self,
        node_key: ino_t,
        validate_fn: V,
        create_fn: C,
    ) -> Result<FsNodeHandle, Errno>
    where
        V: FnOnce(&FsNodeHandle) -> bool,
        C: FnOnce() -> Result<FsNodeHandle, Errno>,
    {
        let mut nodes = self.nodes.lock();
        match nodes.entry(node_key) {
            Entry::Vacant(entry) => {
                let node = create_fn()?;
                entry.insert(Arc::downgrade(&node));
                Ok(node)
            }
            Entry::Occupied(mut entry) => {
                if let Some(node) = entry.get().upgrade() {
                    if validate_fn(&node) {
                        return Ok(node);
                    }
                }
                let node = create_fn()?;
                entry.insert(Arc::downgrade(&node));
                Ok(node)
            }
        }
    }
}
