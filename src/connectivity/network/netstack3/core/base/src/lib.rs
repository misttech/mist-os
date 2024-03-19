// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Base type definitions for netstack3 core.
//!
//! This crate contains definitions common to the other netstack3 core crates
//! and is the base dependency for most of them.

#![no_std]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;

mod context;
mod counters;
mod inspect;
mod time;

pub use context::ContextPair;
pub use counters::Counter;
pub use inspect::{Inspectable, InspectableValue, Inspector, InspectorDeviceExt};
pub use time::{Instant, InstantBindingsTypes, InstantContext, TimerBindingsTypes, TimerContext2};

/// Test utilities provided to all crates.
pub mod testutil {
    pub use crate::time::testutil::{FakeInstant, FakeInstantCtx};
}
