// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides an atomic wrapper around [`zx::Time`].

use fuchsia_zircon as zx;
use std::sync::atomic::{AtomicI64, Ordering};

/// An atomic wrapper around [`zx::Time`].
#[derive(Debug, Default)]
pub struct AtomicTime(AtomicI64);

impl From<zx::Time> for AtomicTime {
    fn from(t: zx::Time) -> Self {
        Self::new(t)
    }
}

impl AtomicTime {
    /// Creates an [`AtomicTime`].
    pub fn new(time: zx::Time) -> Self {
        Self(AtomicI64::new(time.into_nanos()))
    }

    /// Loads a [`zx::Time`].
    pub fn load(&self, order: Ordering) -> zx::Time {
        let Self(atomic_time) = self;
        zx::Time::from_nanos(atomic_time.load(order))
    }

    /// Stores a [`zx::Time`].
    pub fn store(&self, val: zx::Time, order: Ordering) {
        let Self(atomic_time) = self;
        atomic_time.store(val.into_nanos(), order)
    }
}
