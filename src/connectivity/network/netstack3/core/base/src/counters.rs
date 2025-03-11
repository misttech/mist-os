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

    /// Adds the provided value to the counter.
    pub fn add(&self, n: u64) {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        let _: u64 = v.fetch_add(n, Ordering::Relaxed);
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

/// A context that stores counters.
///
/// `CounterContext` exposes access to counters for observation and debugging.
pub trait CounterContext<T> {
    /// Call the function with an immutable reference to counter type T.
    fn with_counters<O, F: FnOnce(&T) -> O>(&self, cb: F) -> O;

    /// Increments the counter returned by the callback.
    fn increment<F: FnOnce(&T) -> &Counter>(&self, cb: F) {
        self.with_counters(|counters| cb(counters).increment());
    }

    /// Adds the provided value to the counter returned by the callback.
    fn add<F: FnOnce(&T) -> &Counter>(&self, n: u64, cb: F) {
        self.with_counters(|counters| cb(counters).add(n))
    }
}

/// A context that provides access to per-resource counters for observation and
/// debugging.
pub trait ResourceCounterContext<R, T>: CounterContext<T> {
    /// Call `cb` with an immutable reference to the set of counters on `resource`.
    fn with_per_resource_counters<O, F: FnOnce(&T) -> O>(&self, resource: &R, cb: F) -> O;

    /// Increments both the per-resource and stackwide versions of
    /// the counter returned by the callback.
    fn increment_both<F: Fn(&T) -> &Counter>(&self, resource: &R, cb: F) {
        self.with_per_resource_counters(resource, |counters| cb(counters).increment());
        self.with_counters(|counters| cb(counters).increment());
    }
}
