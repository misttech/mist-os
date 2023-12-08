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
mod convert;
mod data_structures;
mod lock_ordering;
mod trace;

#[cfg(test)]
pub mod benchmarks;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;

pub mod context;
pub mod counters;
pub mod error;
pub mod ip;
pub mod socket;
pub mod state;
pub mod sync;
pub mod time;
pub mod transport;
pub mod work_queue;

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

/// Miscellaneous and common types.
pub mod types {
    pub use crate::work_queue::WorkQueueReport;
}

use crate::{context::RngContext, device::DeviceId};
pub use context::{BindingsTypes, NonSyncContext, ReferenceNotifiers, SyncCtx};
pub use ip::forwarding::{select_device_for_gateway, set_routes};
pub use time::{handle_timer, Instant, TimerId};

pub(crate) use trace::trace_duration;
