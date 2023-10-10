// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types and utilities for working with packet statistic counters.

use core::sync::atomic::{AtomicUsize, Ordering};

/// An atomic counter for packet statistics, e.g. IPv4 packets received.
#[derive(Debug, Default)]
pub(crate) struct Counter(AtomicUsize);

impl Counter {
    pub(crate) fn increment(&self) {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        let _: usize = v.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn get(&self) -> usize {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        v.load(Ordering::Relaxed)
    }
}
