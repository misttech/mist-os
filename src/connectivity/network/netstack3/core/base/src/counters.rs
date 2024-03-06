// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common counter abstractions.

use core::sync::atomic::{AtomicU64, Ordering};

/// An atomic counter for packet statistics, e.g. IPv4 packets received.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    /// Increments the counter value by 1.
    pub fn increment(&self) {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        let _: u64 = v.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically retrieves the counter value as a `u64`.
    pub fn get(&self) -> u64 {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        v.load(Ordering::Relaxed)
    }
}
