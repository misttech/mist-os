// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use block_server::{BlockServer, SessionManager};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use vfs::directory::helper::DirectlyMutable as _;

/// A directory with an entry for each partition, used to allow other components to discover and
/// connect to partitions.
/// Each entry has a "block" node which serves the fuchsia.hardware.block.partition.Partition
/// protocol.
pub struct PartitionsDirectory {
    node: Arc<vfs::directory::immutable::Simple>,
    entries: Mutex<BTreeMap<String, PartitionsDirectoryEntry>>,
}

impl std::fmt::Debug for PartitionsDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("PartitionDirectory").field("entries", &self.entries).finish()
    }
}

impl PartitionsDirectory {
    pub fn new(node: Arc<vfs::directory::immutable::Simple>) -> Self {
        Self { node, entries: Default::default() }
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    pub fn add_entry<SM: SessionManager + Send + Sync + 'static>(
        &self,
        name: &str,
        server: Weak<BlockServer<SM>>,
    ) {
        let entry = PartitionsDirectoryEntry::new(server);
        self.node.add_entry(name, entry.node.clone()).expect("Added an entry twice");
        self.entries.lock().unwrap().insert(name.to_string(), entry);
    }
}

pub struct PartitionsDirectoryEntry {
    node: Arc<vfs::directory::immutable::Simple>,
}

impl std::fmt::Debug for PartitionsDirectoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("PartitionDirectoryEntry").finish()
    }
}

impl PartitionsDirectoryEntry {
    pub fn new<SM: SessionManager + Send + Sync + 'static>(server: Weak<BlockServer<SM>>) -> Self {
        let node = vfs::directory::immutable::simple();
        node.add_entry(
            "block",
            vfs::service::host(move |requests| {
                let server = server.clone();
                async move {
                    if let Some(server) = server.upgrade() {
                        if let Err(e) = server.handle_requests(requests).await {
                            tracing::error!(?e, "Error handling requests");
                        }
                    }
                }
            }),
        )
        .unwrap();
        Self { node }
    }
}
