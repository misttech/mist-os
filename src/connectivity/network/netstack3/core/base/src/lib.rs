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
mod data_structures;
mod event;
mod frame;
mod inspect;
mod resource_references;
mod rng;
mod time;
mod trace;

pub use context::{ContextPair, ContextProvider, CtxPair, NonTestCtxMarker};
pub use counters::{Counter, CounterContext, ResourceCounterContext};
pub use event::{CoreEventContext, EventContext};
pub use frame::{ReceivableFrameMeta, RecvFrameContext, SendFrameContext, SendableFrameMeta};
pub use inspect::{Inspectable, InspectableValue, Inspector, InspectorDeviceExt};
pub use resource_references::{
    DeferredResourceRemovalContext, ReferenceNotifiers, RemoveResourceResult,
    RemoveResourceResultWithContext,
};
pub use rng::RngContext;
pub use time::{
    local_timer_heap::LocalTimerHeap, CoreTimerContext, HandleableTimer, Instant,
    InstantBindingsTypes, InstantContext, IntoCoreTimerCtx, NestedIntoCoreTimerCtx,
    TimerBindingsTypes, TimerContext, TimerHandler,
};
pub use trace::TracingContext;

/// Reference counted hash map data structure.
pub mod ref_counted_hash_map {
    pub use crate::data_structures::ref_counted_hash_map::{
        InsertResult, RefCountedHashMap, RefCountedHashSet, RemoveResult,
    };
}

/// Sync utilities common to netstack3.
pub mod sync {
    // TODO(https://fxbug.dev/42062225): Support single-threaded variants of
    // types exported from this module.

    // Exclusively re-exports from the sync crate.
    pub use netstack3_sync::{
        rc::{
            DebugReferences, DynDebugReferences, MapNotifier as MapRcNotifier,
            Notifier as RcNotifier, Primary as PrimaryRc, Strong as StrongRc, Weak as WeakRc,
        },
        LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    };
}

/// Test utilities provided to all crates.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    mod fake_bindings;
    pub use crate::event::testutil::FakeEventCtx;
    pub use crate::frame::testutil::{FakeFrameCtx, WithFakeFrameContext};
    pub use crate::rng::testutil::{new_rng, run_with_many_seeds, FakeCryptoRng};
    pub use crate::time::testutil::{
        FakeInstant, FakeInstantCtx, FakeTimerCtx, FakeTimerCtxExt, InstantAndData,
        WithFakeTimerContext,
    };
    pub use crate::trace::testutil::FakeTracingCtx;
    pub use fake_bindings::FakeBindingsCtx;
}
