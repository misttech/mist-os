// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.

#![no_std]
// In case we roll the toolchain and something we're using as a feature has been
// stabilized.
#![allow(stable_features)]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]
// Turn off checks for dead code, but only when building for benchmarking.
// benchmarking. This allows the benchmarks to be written as part of the crate,
// with access to test utilities, without a bunch of build errors due to unused
// code. These checks are turned back on in the 'benchmark' module.
#![cfg_attr(benchmark, allow(dead_code, unused_imports, unused_macros))]

// TODO(https://github.com/rust-lang-nursery/portability-wg/issues/11): remove
// this module.
extern crate fakealloc as alloc;

// TODO(https://github.com/dtolnay/thiserror/pull/64): remove this module.
extern crate fakestd as std;

#[macro_use]
mod macros;

mod algorithm;
#[cfg(test)]
pub mod benchmarks;
pub mod context;
pub(crate) mod convert;
pub mod counters;
pub mod data_structures;
pub mod device;
pub mod error;
pub mod ip;
mod lock_ordering;
pub mod socket;
pub mod state;
pub mod sync;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;
pub mod time;
mod trace;
pub mod transport;
pub mod work_queue;

use crate::{context::RngContext, device::DeviceId};
pub use context::{BindingsTypes, NonSyncContext, ReferenceNotifiers, SyncCtx};
pub use ip::forwarding::{select_device_for_gateway, set_routes};
pub use time::{handle_timer, Instant, TimerId};
pub use work_queue::WorkQueueReport;

pub(crate) use trace::trace_duration;
