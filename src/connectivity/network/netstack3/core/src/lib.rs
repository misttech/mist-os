// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.

#![no_std]
// TODO(https://fxbug.dev/339502691): Return to the default limit once lock
// ordering no longer causes overflows.
#![recursion_limit = "256"]
// In case we roll the toolchain and something we're using as a feature has been
// stabilized.
#![allow(stable_features)]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

// TODO(https://github.com/rust-lang-nursery/portability-wg/issues/11): remove
// this module.
extern crate fakealloc as alloc;

mod api;
mod context;
mod counters;
mod lock_ordering;
mod marker;
mod state;
mod time;
mod transport;

#[cfg(any(test, feature = "testutils"))]
pub mod testutil;

/// The device layer.
pub mod device {
    #[path = "."]
    pub(crate) mod integration {
        mod base;
        mod ethernet;
        mod loopback;
        mod pure_ip;
        mod socket;

        pub(crate) use base::{
            device_state, device_state_and_core_ctx, get_mtu, ip_device_state,
            ip_device_state_and_core_ctx,
        };
    }

    // Re-exported types.
    pub use netstack3_base::DeviceNameMatcher;
    pub use netstack3_device::ethernet::{
        EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice, EthernetWeakDeviceId,
        MaxEthernetFrameSize, RecvEthernetFrameMeta,
    };
    pub use netstack3_device::loopback::{
        LoopbackCreationProperties, LoopbackDevice, LoopbackDeviceId,
    };
    pub use netstack3_device::pure_ip::{
        PureIpDevice, PureIpDeviceCreationProperties, PureIpDeviceId,
        PureIpDeviceReceiveFrameMetadata, PureIpHeaderParams, PureIpWeakDeviceId,
    };
    pub use netstack3_device::queue::{
        BatchSize, ReceiveQueueBindingsContext, TransmitQueueBindingsContext,
        TransmitQueueConfiguration,
    };
    pub use netstack3_device::{
        ArpConfiguration, ArpConfigurationUpdate, DeviceClassMatcher, DeviceConfiguration,
        DeviceConfigurationUpdate, DeviceConfigurationUpdateError, DeviceId,
        DeviceIdAndNameMatcher, DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceProvider,
        DeviceSendFrameError, NdpConfiguration, NdpConfigurationUpdate, WeakDeviceId,
    };
}

/// Device socket API.
pub mod device_socket {
    pub use netstack3_base::{FrameDestination, SendFrameErrorReason};
    pub use netstack3_device::socket::{
        DeviceSocketBindingsContext, DeviceSocketMetadata, DeviceSocketTypes, EthernetFrame,
        EthernetHeaderParams, Frame, IpFrame, Protocol, ReceivedFrame, SentFrame, SocketId,
        SocketInfo, TargetDevice, WeakDeviceSocketId,
    };
}

/// Generic netstack errors.
pub mod error {
    pub use netstack3_base::{
        AddressResolutionFailed, ExistsError, LocalAddressError, NotFoundError, NotSupportedError,
        RemoteAddressError, SocketError, ZonedAddressError,
    };
}

/// Framework for packet filtering.
pub mod filter {
    mod integration;

    pub use netstack3_filter::{
        Action, AddressMatcher, AddressMatcherType, FilterApi, FilterBindingsContext,
        FilterBindingsTypes, Hook, InterfaceMatcher, InterfaceProperties, IpRoutines, NatRoutines,
        PacketMatcher, PortMatcher, ProofOfEgressCheck, Routine, Routines, Rule, TransparentProxy,
        TransportProtocolMatcher, Tuple, UninstalledRoutine, ValidationError,
    };
}

/// Facilities for inspecting stack state for debugging.
pub mod inspect {
    pub use netstack3_base::{Inspectable, InspectableValue, Inspector, InspectorDeviceExt};
}

/// Methods for dealing with ICMP sockets.
pub mod icmp {
    pub use netstack3_icmp_echo::{IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpSocketId};
}

/// The Internet Protocol, versions 4 and 6.
pub mod ip {
    #[path = "."]
    pub(crate) mod integration {
        mod base;
        mod device;
        mod multicast_forwarding;
        mod raw;

        pub(crate) use device::{CoreCtxWithIpDeviceConfiguration, IpAddrCtxSpec};
    }

    // Re-exported types.
    pub use netstack3_base::{SubnetMatcher, WrapBroadcastMarker};
    pub use netstack3_ip::device::{
        AddIpAddrSubnetError, AddrSubnetAndManualConfigEither, AddressRemovedReason,
        CommonAddressProperties, IidSecret, IpAddressState, IpDeviceConfiguration,
        IpDeviceConfigurationUpdate, IpDeviceEvent, Ipv4AddrConfig,
        Ipv4DeviceConfigurationAndFlags, Ipv4DeviceConfigurationUpdate, Ipv6AddrManualConfig,
        Ipv6DeviceConfiguration, Ipv6DeviceConfigurationAndFlags, Ipv6DeviceConfigurationUpdate,
        Lifetime, PreferredLifetime, SetIpAddressPropertiesError, SlaacConfiguration,
        SlaacConfigurationUpdate, TemporarySlaacAddressConfiguration, UpdateIpConfigurationError,
    };
    pub use netstack3_ip::multicast_forwarding::{
        ForwardMulticastRouteError, MulticastForwardingDisabledError, MulticastForwardingEvent,
        MulticastRoute, MulticastRouteKey, MulticastRouteStats, MulticastRouteTarget,
    };
    pub use netstack3_ip::raw::{
        RawIpSocketIcmpFilter, RawIpSocketIcmpFilterError, RawIpSocketId, RawIpSocketProtocol,
        RawIpSocketSendToError, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes,
        WeakRawIpSocketId,
    };
    pub use netstack3_ip::socket::{
        IpSockCreateAndSendError, IpSockCreationError, IpSockSendError,
    };
    pub use netstack3_ip::{IpLayerEvent, ResolveRouteError};
}

/// Types and utilities for dealing with neighbors.
pub mod neighbor {
    // Re-exported types.
    pub use netstack3_ip::nud::{
        Event, EventDynamicState, EventKind, EventState, LinkResolutionContext,
        LinkResolutionNotifier, LinkResolutionResult, NeighborRemovalError, NudUserConfig,
        NudUserConfigUpdate, StaticNeighborInsertionError, MAX_ENTRIES,
    };
}

/// Types and utilities for dealing with routes.
pub mod routes {
    // Re-exported types.
    pub use netstack3_base::WrapBroadcastMarker;
    pub use netstack3_ip::{
        AddRouteError, AddableEntry, AddableEntryEither, AddableMetric, Entry, EntryEither,
        Generation, Mark, MarkDomain, MarkMatcher, MarkMatchers, Metric, NextHop, RawMetric,
        ResolvedRoute, RoutableIpAddr, RoutingTableId, Rule, RuleAction, RuleMatcher,
        TrafficOriginMatcher,
    };
}

/// Common types for dealing with sockets.
pub mod socket {
    pub use netstack3_datagram::{
        ConnInfo, ConnectError, ExpectedConnError, ExpectedUnboundError, ListenerInfo,
        MulticastInterfaceSelector, MulticastMembershipInterfaceSelector, SendError, SendToError,
        SetMulticastMembershipError, SocketInfo,
    };

    pub use netstack3_base::socket::{
        AddrIsMappedError, NotDualStackCapableError, SetDualStackEnabledError, ShutdownType,
        StrictlyZonedAddr,
    };
}

/// Useful synchronization primitives.
pub mod sync {
    // We take all of our dependencies directly from base for symmetry with the
    // other crates. However, we want to explicitly have all the dependencies in
    // GN so we can assert the dependencies on the crate variants. This defeats
    // rustc's unused dependency check.
    use netstack3_sync as _;

    pub use netstack3_base::sync::{
        DebugReferences, DynDebugReferences, LockGuard, MapRcNotifier, Mutex, PrimaryRc,
        RcNotifier, RwLock, RwLockReadGuard, RwLockWriteGuard, StrongRc, WeakRc,
    };
    pub use netstack3_base::{RemoveResourceResult, RemoveResourceResultWithContext};
}

/// Methods for dealing with TCP sockets.
pub mod tcp {
    pub use netstack3_base::{FragmentedPayload, Payload, PayloadLen};
    pub use netstack3_tcp::{
        AcceptError, BindError, BoundInfo, Buffer, BufferLimits, BufferSizes, ConnectError,
        ConnectionError, ConnectionInfo, IntoBuffers, ListenError, ListenerNotifier, NoConnection,
        OriginalDestinationError, ReceiveBuffer, SendBuffer, SetDeviceError, SetReuseAddrError,
        SocketAddr, SocketInfo, SocketOptions, TcpBindingsTypes, TcpSocketId, UnboundInfo,
        DEFAULT_FIN_WAIT2_TIMEOUT,
    };
}

/// Miscellaneous and common types.
pub mod types {
    pub use netstack3_base::{Counter, WorkQueueReport};
}

/// Methods for dealing with UDP sockets.
pub mod udp {
    pub use netstack3_udp::{
        SendError, SendToError, UdpBindingsTypes, UdpPacketMeta, UdpReceiveBindingsContext,
        UdpRemotePort, UdpSocketId,
    };
}

pub use api::CoreApi;
pub use context::{CoreCtx, UnlockedCoreCtx};
pub use inspect::Inspector;
pub use marker::{BindingsContext, BindingsTypes, CoreContext, IpBindingsContext, IpExt};
pub use netstack3_base::{
    CtxPair, DeferredResourceRemovalContext, EventContext, InstantBindingsTypes, InstantContext,
    ReferenceNotifiers, RngContext, TimerBindingsTypes, TimerContext, TracingContext,
};
pub use state::{StackState, StackStateBuilder};
pub use time::{AtomicInstant, Instant, TimerId};

// Re-export useful macros.
pub use netstack3_device::for_any_device_id;
pub use netstack3_macros::context_ip_bounds;
