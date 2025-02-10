// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core device layer.
//!
//! This crate contains the device layer for netstack3.

#![no_std]
#![warn(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;

#[path = "."]
mod internal {
    pub(super) mod api;
    pub(super) mod arp;
    pub(super) mod base;
    pub(super) mod blackhole;
    pub(super) mod config;
    pub(super) mod ethernet;
    pub(super) mod id;
    pub(super) mod loopback;
    pub(super) mod pure_ip;
    pub(super) mod queue;
    pub(super) mod socket;
    pub(super) mod state;
}

/// Blackhole devices.
pub mod blackhole {
    pub use crate::internal::base::BlackholeDeviceCounters;
    pub use crate::internal::blackhole::{
        BlackholeDevice, BlackholeDeviceId, BlackholePrimaryDeviceId, BlackholeWeakDeviceId,
    };
}

/// Ethernet devices.
pub mod ethernet {
    pub use crate::internal::base::EthernetDeviceCounters;
    pub use crate::internal::ethernet::{
        get_mac, get_mtu, join_link_multicast, leave_link_multicast, send_as_ethernet_frame_to_dst,
        send_ip_frame, set_mtu, DynamicEthernetDeviceState, EthernetCreationProperties,
        EthernetIpLinkDeviceDynamicStateContext, EthernetIpLinkDeviceStaticStateContext,
        EthernetLinkDevice, EthernetTimerId, MaxEthernetFrameSize, RecvEthernetFrameMeta,
        StaticEthernetDeviceState,
    };
    pub use crate::internal::id::{
        EthernetDeviceId, EthernetPrimaryDeviceId, EthernetWeakDeviceId,
    };
}

/// Loopback devices.
pub mod loopback {
    pub use crate::internal::loopback::{
        send_ip_frame, LoopbackCreationProperties, LoopbackDevice, LoopbackDeviceId,
        LoopbackPrimaryDeviceId, LoopbackRxQueueMeta, LoopbackTxQueueMeta, LoopbackWeakDeviceId,
    };
}

/// Marker traits controlling Device context behavior.
pub mod marker {
    pub use crate::internal::ethernet::UseArpFrameMetadataBlanket;
}

/// Pure IP devices.
pub mod pure_ip {
    pub use crate::internal::base::PureIpDeviceCounters;
    pub use crate::internal::pure_ip::{
        get_mtu, send_ip_frame, set_mtu, DynamicPureIpDeviceState, PureIpDevice,
        PureIpDeviceCreationProperties, PureIpDeviceId, PureIpDeviceReceiveFrameMetadata,
        PureIpDeviceStateContext, PureIpDeviceTxQueueFrameMetadata, PureIpHeaderParams,
        PureIpPrimaryDeviceId, PureIpWeakDeviceId,
    };
}

/// Device sockets.
pub mod socket {
    pub use crate::internal::socket::{
        AllSockets, AnyDeviceSockets, DeviceSocketAccessor, DeviceSocketApi,
        DeviceSocketBindingsContext, DeviceSocketContext, DeviceSocketCounters, DeviceSocketId,
        DeviceSocketMetadata, DeviceSocketTypes, DeviceSockets, EthernetFrame,
        EthernetHeaderParams, Frame, HeldDeviceSockets, HeldSockets, IpFrame, ParseSentFrameError,
        PrimaryDeviceSocketId, Protocol, ReceivedFrame, SentFrame, SocketId, SocketInfo,
        SocketState, SocketStateAccessor, Target, TargetDevice, WeakDeviceSocketId,
    };
}

/// Device RX and TX queueing.
pub mod queue {
    pub use crate::internal::queue::api::{ReceiveQueueApi, TransmitQueueApi};
    pub use crate::internal::queue::rx::{
        ReceiveDequeContext, ReceiveQueueBindingsContext, ReceiveQueueContext, ReceiveQueueHandler,
        ReceiveQueueState, ReceiveQueueTypes,
    };
    pub use crate::internal::queue::tx::{
        BufVecU8Allocator, TransmitDequeueContext, TransmitQueueBindingsContext,
        TransmitQueueCommon, TransmitQueueConfiguration, TransmitQueueContext,
        TransmitQueueHandler, TransmitQueueState,
    };
    pub use crate::internal::queue::{BatchSize, DequeueState, ReceiveQueueFullError};
}

pub use internal::api::{DeviceAnyApi, DeviceApi};
pub use internal::arp::{
    ArpConfigContext, ArpContext, ArpCounters, ArpNudCtx, ArpSenderContext, ArpState,
};
pub use internal::base::{
    DeviceClassMatcher, DeviceCollectionContext, DeviceCounters, DeviceIdAndNameMatcher,
    DeviceLayerEventDispatcher, DeviceLayerState, DeviceLayerStateTypes, DeviceLayerTimerId,
    DeviceLayerTypes, DeviceSendFrameError, Devices, DevicesIter, Ipv6DeviceLinkLayerAddr,
    OriginTracker, OriginTrackerContext,
};
pub use internal::config::{
    ArpConfiguration, ArpConfigurationUpdate, DeviceConfiguration, DeviceConfigurationContext,
    DeviceConfigurationUpdate, DeviceConfigurationUpdateError, NdpConfiguration,
    NdpConfigurationUpdate,
};
pub use internal::id::{BaseDeviceId, DeviceId, DeviceProvider, WeakDeviceId};
pub use internal::state::{DeviceStateSpec, IpLinkDeviceState, IpLinkDeviceStateInner};

/// Device layer test utilities.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::internal::ethernet::testutil::IPV6_MIN_IMPLIED_MAX_FRAME_SIZE;
}
