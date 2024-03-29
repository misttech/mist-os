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
mod api;
mod context;
mod convert;
mod counters;
mod data_structures;
mod lock_ordering;
mod marker;
mod state;
mod time;
mod trace;
mod transport;
mod uninstantiable;
mod work_queue;

#[cfg(test)]
pub mod benchmarks;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;

/// The device layer.
pub mod device {
    pub(crate) mod api;
    pub(crate) mod arp;
    pub(crate) mod base;
    pub(crate) mod config;
    pub(crate) mod ethernet;
    pub(crate) mod id;
    pub(crate) mod integration;
    pub(crate) mod link;
    pub(crate) mod loopback;
    pub(crate) mod ndp;
    pub(crate) mod pure_ip;
    pub(crate) mod queue;
    pub(crate) mod socket;
    mod state;

    pub(crate) use base::*;
    pub(crate) use id::*;

    // Re-exported types.
    pub use base::{
        DeviceClassMatcher, DeviceIdAndNameMatcher, DeviceLayerEventDispatcher,
        DeviceLayerStateTypes, DeviceSendFrameError,
    };
    pub use config::{
        ArpConfiguration, ArpConfigurationUpdate, DeviceConfiguration, DeviceConfigurationUpdate,
        DeviceConfigurationUpdateError, NdpConfiguration, NdpConfigurationUpdate,
    };
    pub use ethernet::{
        EthernetCreationProperties, EthernetLinkDevice, MaxEthernetFrameSize, RecvEthernetFrameMeta,
    };
    pub use id::{DeviceId, DeviceProvider, EthernetDeviceId, EthernetWeakDeviceId, WeakDeviceId};
    pub use loopback::{LoopbackCreationProperties, LoopbackDevice, LoopbackDeviceId};
    pub use pure_ip::{
        PureIpDevice, PureIpDeviceCreationProperties, PureIpDeviceId,
        PureIpDeviceReceiveFrameMetadata, PureIpWeakDeviceId,
    };
    pub use queue::tx::TransmitQueueConfiguration;
}

/// Device socket API.
pub mod device_socket {
    pub use crate::device::{
        base::FrameDestination,
        socket::{
            DeviceSocketBindingsContext, DeviceSocketTypes, EthernetFrame, Frame, Protocol,
            ReceivedFrame, SendDatagramError, SendDatagramParams, SendFrameError, SendFrameParams,
            SentFrame, SocketId, SocketInfo, TargetDevice,
        },
    };
}

// Allow direct public access to the error module. This module is unlikely to
// evolve poorly or have sealed traits. We can revisit if this becomes hard to
// uphold.
pub mod error;

/// Framework for packet filtering.
pub mod filter {
    pub(crate) mod integration;

    pub(crate) use integration::FilterHandlerProvider;
    #[cfg(test)]
    pub(crate) use netstack3_filter::testutil::NoopImpl;
    pub use netstack3_filter::{
        Action, AddressMatcher, AddressMatcherType, FilterApi, FilterBindingsTypes, Hook,
        InterfaceMatcher, InterfaceProperties, IpRoutines, NatRoutines, PacketMatcher, PortMatcher,
        Routine, Rule, State, TransportProtocolMatcher, UninstalledRoutine, ValidationError,
    };
    pub(crate) use netstack3_filter::{
        FilterContext, FilterHandler, FilterImpl, FilterIpContext, ForwardedPacket, IpPacket,
        MaybeTransportPacket, NestedWithInnerIpPacket, RxPacket, TransportPacketSerializer,
        TxPacket, ValidState, Verdict,
    };
}

/// Facilities for inspecting stack state for debugging.
pub mod inspect {
    pub use netstack3_base::{Inspectable, InspectableValue, Inspector, InspectorDeviceExt};
}

/// Methods for dealing with ICMP sockets.
pub mod icmp {
    pub use crate::ip::icmp::socket::{
        IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpSocketId, SocketInfo,
    };
}

/// The Internet Protocol, versions 4 and 6.
pub mod ip {
    #[macro_use]
    pub(crate) mod path_mtu;

    pub(crate) mod api;
    pub(crate) mod base;
    pub(crate) mod device;
    pub(crate) mod forwarding;
    pub(crate) mod gmp;
    pub(crate) mod icmp;
    pub(crate) mod reassembly;
    pub(crate) mod socket;
    pub(crate) mod types;

    mod integration;
    mod ipv6;

    pub(crate) use base::*;

    // Re-exported types.
    pub use crate::algorithm::STABLE_IID_SECRET_KEY_BYTES;
    pub use base::{IpLayerEvent, ResolveRouteError};
    pub use device::{
        api::{AddIpAddrSubnetError, AddrSubnetAndManualConfigEither, SetIpAddressPropertiesError},
        config::{
            IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationUpdate, UpdateIpConfigurationError,
        },
        slaac::{SlaacConfiguration, TemporarySlaacAddressConfiguration},
        state::{
            IpDeviceConfiguration, Ipv4AddrConfig, Ipv4DeviceConfigurationAndFlags,
            Ipv6AddrManualConfig, Ipv6DeviceConfiguration, Ipv6DeviceConfigurationAndFlags,
            Lifetime,
        },
        AddressRemovedReason, IpAddressState, IpDeviceEvent,
    };
    pub use socket::{IpSockCreateAndSendError, IpSockCreationError, IpSockSendError};
}

/// Types and utilities for dealing with neighbors.
pub mod neighbor {
    // Re-exported types.
    pub use crate::ip::device::nud::{
        api::{NeighborRemovalError, StaticNeighborInsertionError},
        Event, EventDynamicState, EventKind, EventState, LinkResolutionContext,
        LinkResolutionNotifier, LinkResolutionResult, NudUserConfig, NudUserConfigUpdate,
        MAX_ENTRIES,
    };
}

/// Types and utilities for dealing with routes.
pub mod routes {
    // Re-exported types.
    pub use crate::ip::forwarding::AddRouteError;
    pub use crate::ip::types::{
        AddableEntry, AddableEntryEither, AddableMetric, Entry, EntryEither, Generation, Metric,
        NextHop, RawMetric, ResolvedRoute, RoutableIpAddr, WrapBroadcastMarker,
    };
}

/// Common types for dealing with sockets.
pub mod socket {
    pub(crate) mod address;
    mod base;
    pub(crate) mod datagram;

    pub(crate) use base::*;

    pub use address::{AddrIsMappedError, StrictlyZonedAddr};
    pub use base::{NotDualStackCapableError, SetDualStackEnabledError, ShutdownType};
    pub use datagram::{
        ConnectError, ExpectedConnError, ExpectedUnboundError, MulticastInterfaceSelector,
        MulticastMembershipInterfaceSelector, SendError, SendToError, SetMulticastMembershipError,
    };
}

/// Useful synchronization primitives.
pub mod sync {
    pub(crate) mod types;
    // TODO(https://fxbug.dev/42062225): Support single-threaded variants of types
    // exported from this module.

    // Exclusively re-exports from the sync crate.
    pub use netstack3_sync::{
        rc::{
            DebugReferences, MapNotifier as MapRcNotifier, Notifier as RcNotifier,
            Primary as PrimaryRc, Strong as StrongRc, Weak as WeakRc,
        },
        LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    };

    pub use types::{RemoveResourceResult, RemoveResourceResultWithContext};
}

/// Methods for dealing with TCP sockets.
pub mod tcp {
    pub use crate::transport::tcp::{
        buffer::{
            Buffer, BufferLimits, IntoBuffers, ReceiveBuffer, RingBuffer, SendBuffer, SendPayload,
        },
        segment::Payload,
        socket::{
            AcceptError, BindError, BoundInfo, ConnectError, ConnectionInfo, ListenError,
            ListenerNotifier, NoConnection, SetDeviceError, SetReuseAddrError, SocketAddr,
            SocketInfo, TcpBindingsTypes, TcpSocketId, UnboundInfo,
        },
        state::Takeable,
        BufferSizes, ConnectionError, SocketOptions, DEFAULT_FIN_WAIT2_TIMEOUT,
    };
}

/// Miscellaneous and common types.
pub mod types {
    pub use crate::work_queue::WorkQueueReport;
}

/// Methods for dealing with UDP sockets.
pub mod udp {
    pub use crate::transport::udp::{
        ConnInfo, ListenerInfo, SendError, SendToError, SocketInfo, UdpBindingsTypes,
        UdpReceiveBindingsContext, UdpRemotePort, UdpSocketId,
    };
}

pub use api::CoreApi;
pub use context::{
    CoreCtx, EventContext, InstantBindingsTypes, InstantContext, ReferenceNotifiers, RngContext,
    TimerBindingsTypes, TimerContext, TimerContext2, TracingContext, UnlockedCoreCtx,
};
pub use inspect::Inspector;
pub use marker::{BindingsContext, BindingsTypes, CoreContext, IpBindingsContext, IpExt};
pub use state::StackState;
pub use time::{Instant, TimerId};

// Re-export useful macros.
pub use netstack3_macros::context_ip_bounds;
pub(crate) use trace::trace_duration;
