// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides an atomic wrapper around [`zx::MonotonicTime`].

use fuchsia_zircon as zx;
use std::sync::atomic::{AtomicI64, Ordering};

/// An atomic wrapper around [`zx::MonotonicTime`].
#[derive(Debug, Default)]
pub struct AtomicMonotonicTime(AtomicI64);

impl From<zx::MonotonicTime> for AtomicMonotonicTime {
    fn from(t: zx::MonotonicTime) -> Self {
        Self::new(t)
    }
}

impl AtomicMonotonicTime {
    /// Creates an [`AtomicTime`].
    pub fn new(time: zx::MonotonicTime) -> Self {
        Self(AtomicI64::new(time.into_nanos()))
    }

    /// Loads a [`zx::MonotonicTime`].
    pub fn load(&self, order: Ordering) -> zx::MonotonicTime {
        let Self(atomic_time) = self;
        zx::Time::from_nanos(atomic_time.load(order))
    }

    /// Stores a [`zx::MonotonicTime`].
    pub fn store(&self, val: zx::MonotonicTime, order: Ordering) {
        let Self(atomic_time) = self;
        atomic_time.store(val.into_nanos(), order)
    }
}
