// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::gpt::GptManager;
use block_server::{BlockServer, SessionManager};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use vfs::directory::helper::DirectlyMutable as _;

/// A directory of instances of the fuchsia.storagehost.PartitionService service.
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
        self.node.remove_all_entries();
        self.entries.lock().unwrap().clear();
    }

    pub fn add_entry<SM: SessionManager + Send + Sync + 'static>(
        &self,
        name: &str,
        block_server: Weak<BlockServer<SM>>,
        gpt_manager: Weak<GptManager>,
        gpt_index: usize,
    ) {
        let entry = PartitionsDirectoryEntry::new(block_server, gpt_manager, gpt_index);
        self.node.add_entry(name, entry.node.clone()).expect("Added an entry twice");
        self.entries.lock().unwrap().insert(name.to_string(), entry);
    }
}

/// A node which hosts an instance of fuchsia.storagehost.PartitionService.
pub struct PartitionsDirectoryEntry {
    node: Arc<vfs::directory::immutable::Simple>,
}

impl std::fmt::Debug for PartitionsDirectoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("PartitionDirectoryEntry").finish()
    }
}

impl PartitionsDirectoryEntry {
    pub fn new<SM: SessionManager + Send + Sync + 'static>(
        block_server: Weak<BlockServer<SM>>,
        gpt_manager: Weak<GptManager>,
        gpt_index: usize,
    ) -> Self {
        let node = vfs::directory::immutable::simple();
        node.add_entry(
            "volume",
            vfs::service::host(move |requests| {
                let server = block_server.clone();
                async move {
                    if let Some(server) = server.upgrade() {
                        if let Err(err) = server.handle_requests(requests).await {
                            tracing::error!(?err, "Error handling requests");
                        }
                    }
                }
            }),
        )
        .unwrap();
        node.add_entry(
            "partition",
            vfs::service::host(move |requests| {
                let manager = gpt_manager.clone();
                async move {
                    if let Some(manager) = manager.upgrade() {
                        if let Err(err) =
                            manager.handle_partitions_requests(gpt_index, requests).await
                        {
                            tracing::error!(?err, "Error handling requests");
                        }
                    }
                }
            }),
        )
        .unwrap();

        Self { node }
    }
}
