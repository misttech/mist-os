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
mod context;
mod convert;
mod counters;
mod data_structures;
mod lock_ordering;
mod state;
mod time;
mod trace;
mod work_queue;

#[cfg(test)]
pub mod benchmarks;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;

pub mod ip;
pub mod socket;
pub mod transport;

/// The device layer.
pub mod device {
    pub(crate) mod arp;
    pub(crate) mod base;
    pub(crate) mod ethernet;
    pub(crate) mod id;
    pub(crate) mod integration;
    pub(crate) mod link;
    pub(crate) mod loopback;
    pub(crate) mod ndp;
    pub(crate) mod queue;
    pub(crate) mod socket;
    mod state;

    pub(crate) use base::*;
    pub(crate) use id::*;

    // Re-exported freestanding functions.
    //
    // TODO(https://fxbug.dev/133996): Replace freestanding functions with API
    // objects.
    pub use base::{
        add_ethernet_device_with_state, add_ip_addr_subnet, add_loopback_device_with_state,
        del_ip_addr, flush_neighbor_table, get_all_ip_addr_subnets,
        get_ipv4_configuration_and_flags, get_ipv6_configuration_and_flags, get_routing_metric,
        handle_queued_rx_packets, insert_static_neighbor_entry, inspect_devices, inspect_neighbors,
        receive_frame, remove_ethernet_device, remove_loopback_device, remove_neighbor_table_entry,
        set_ip_addr_properties, set_tx_queue_configuration, transmit_queued_tx_frames,
        update_ipv4_configuration, update_ipv6_configuration,
    };
    pub use ethernet::resolve_ethernet_link_addr;

    // Re-exported types.
    pub use base::{
        DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceSendFrameError, DevicesVisitor,
        InspectDeviceState, NeighborVisitor, RemoveDeviceResult, RemoveDeviceResultWithContext,
    };
    pub use ethernet::{EthernetLinkDevice, MaxEthernetFrameSize};
    pub use id::{DeviceId, EthernetDeviceId, EthernetWeakDeviceId, WeakDeviceId};
    pub use loopback::LoopbackDeviceId;
    pub use queue::tx::TransmitQueueConfiguration;
}

/// Device socket API.
pub mod device_socket {
    // Re-exported functions.
    //
    // TODO(https://fxbug.dev/133996): Replace freestanding functions with API
    // objects.
    pub use crate::device::socket::{
        create, get_info, remove, send_datagram, send_frame, set_device, set_device_and_protocol,
    };

    // Re-exported types.
    pub use crate::device::{
        base::FrameDestination,
        socket::{
            DeviceSocketTypes, EthernetFrame, Frame, NonSyncContext, Protocol, ReceivedFrame,
            SendDatagramError, SendDatagramParams, SendFrameError, SendFrameParams, SentFrame,
            SocketId, SocketInfo, TargetDevice,
        },
    };
}

// Allow direct public access to the error module. This module is unlikely to
// evolve poorly or have sealed traits. We can revisit if this becomes hard to
// uphold.
pub mod error;

/// Facilities for inspecting stack state for debugging.
pub mod inspect {
    // Re-exported functions.
    //
    // TODO(https://fxbug.dev/133996): Replace freestanding functions with API
    // objects.
    pub use crate::counters::inspect_counters;

    // Re-exported types.
    pub use crate::counters::{CounterVisitor, StackCounters};
}

/// Useful synchronization primitives.
pub mod sync {
    // TODO(https://fxbug.dev/110884): Support single-threaded variants of types
    // exported from this module.

    #[cfg(all(feature = "instrumented", not(loom)))]
    use netstack3_sync_instrumented as netstack3_sync;

    // Don't perform recursive lock checks when benchmarking so that the benchmark
    // results are not affected by the extra bookkeeping.
    #[cfg(all(not(feature = "instrumented"), not(loom)))]
    use netstack3_sync_not_instrumented as netstack3_sync;

    #[cfg(loom)]
    use netstack3_sync_loom as netstack3_sync;

    // Exclusively re-exports from the sync crate.
    pub use netstack3_sync::{
        rc::{
            DebugReferences, MapNotifier as MapRcNotifier, Notifier as RcNotifier,
            Primary as PrimaryRc, Strong as StrongRc, Weak as WeakRc,
        },
        LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    };
}

/// Miscellaneous and common types.
pub mod types {
    pub use crate::work_queue::WorkQueueReport;
}

pub use context::{
    BindingsTypes, EventContext, InstantBindingsTypes, InstantContext, NonSyncContext,
    ReferenceNotifiers, RngContext, SyncCtx, TimerContext, TracingContext,
};
pub use ip::forwarding::{select_device_for_gateway, set_routes};
pub use time::{handle_timer, Instant, TimerId};

pub(crate) use trace::trace_duration;
