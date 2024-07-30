// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A helper struct to generate unused capability ids.
/// This is clonable and thread safe. There should generally be one of these
/// for each `fuchsia.component.sandbox.CapabilityStore` connection.
#[derive(Clone)]
pub struct CapabilityIdGenerator {
    next_id: Arc<AtomicU64>,
}

impl CapabilityIdGenerator {
    pub fn new() -> Self {
        Self { next_id: Arc::new(AtomicU64::new(0)) }
    }

    /// Get the next free id.
    pub fn next(&self) -> u64 {
        self.range(1)
    }

    /// Get a range of free ids of size `size`.
    /// This returns the first id in the range.
    pub fn range(&self, size: u64) -> u64 {
        self.next_id.fetch_add(size, Ordering::Relaxed)
    }
}
