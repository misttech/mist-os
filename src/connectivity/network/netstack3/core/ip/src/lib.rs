// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core IP layer.
//!
//! This crate contains the IP layer for netstack3.

#![no_std]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;
extern crate fakestd as std;

#[path = "."]
mod internal {
    #[macro_use]
    pub(super) mod path_mtu;

    pub(super) mod api;
    pub(super) mod base;
    pub(super) mod datagram;
    pub(super) mod device;
    pub(super) mod forwarding;
    pub(super) mod gmp;
    pub(super) mod icmp;
    pub(super) mod ipv6;
    pub(super) mod raw;
    pub(super) mod reassembly;
    pub(super) mod socket;
    pub(super) mod types;
    pub(super) mod uninstantiable;
}

/// Shared code for implementing datagram sockets.
// TODO(https://fxbug.dev/339531757): Split this into its own crate so it
// doesn't have to be part of the IP module. It's currently here because ICMP
// echo sockets depend on it.
pub mod datagram {
    pub use crate::internal::datagram::spec_context::{
        DatagramSpecBoundStateContext, DatagramSpecStateContext,
        DualStackDatagramSpecBoundStateContext, NonDualStackDatagramSpecBoundStateContext,
    };
    pub use crate::internal::datagram::{
        close, collect_all_sockets, connect, create, disconnect_connected, get_bound_device,
        get_info, get_ip_hop_limits, get_ip_transparent, get_multicast_interface,
        get_options_device, get_sharing, get_shutdown_connected, listen, send_conn, send_to,
        set_device, set_ip_transparent, set_multicast_interface, set_multicast_membership,
        shutdown_connected, update_ip_hop_limit, update_sharing, with_other_stack_ip_options,
        with_other_stack_ip_options_and_default_hop_limits, with_other_stack_ip_options_mut,
        with_other_stack_ip_options_mut_if_unbound, BoundSocketState, BoundSocketStateType,
        BoundSockets, ConnInfo, ConnectError, DatagramBoundStateContext, DatagramFlowId,
        DatagramSocketMapSpec, DatagramSocketOptions, DatagramSocketSet, DatagramSocketSpec,
        DatagramStateContext, DualStackConnState, DualStackConverter,
        DualStackDatagramBoundStateContext, DualStackIpExt, EitherIpSocket, ExpectedConnError,
        ExpectedUnboundError, InUseError, IpExt, IpOptions, ListenerInfo,
        MulticastInterfaceSelector, MulticastMembershipInterfaceSelector, NonDualStackConverter,
        NonDualStackDatagramBoundStateContext, ReferenceState, SendError, SendToError,
        SetMulticastMembershipError, SocketHopLimits, SocketInfo, SocketState, StrongRc, WeakRc,
        WrapOtherStackIpOptions, WrapOtherStackIpOptionsMut,
    };

    /// Datagram socket test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::datagram::create_primary_id;
        pub use crate::internal::datagram::testutil::setup_fake_ctx_with_dualstack_conn_addrs;
    }
}

/// Definitions for devices at the IP layer.
pub mod device {
    pub use crate::internal::device::api::{
        AddIpAddrSubnetError, AddrSubnetAndManualConfigEither, DeviceIpAnyApi, DeviceIpApi,
        SetIpAddressPropertiesError,
    };
    pub use crate::internal::device::config::{
        IpDeviceConfigurationHandler, IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
        Ipv6DeviceConfigurationUpdate, UpdateIpConfigurationError,
    };
    pub use crate::internal::device::dad::{
        DadAddressContext, DadAddressStateRef, DadContext, DadEvent, DadHandler, DadStateRef,
        DadTimerId,
    };
    pub use crate::internal::device::opaque_iid::{OpaqueIid, OpaqueIidNonce, StableIidSecret};
    pub use crate::internal::device::route_discovery::{
        Ipv6DiscoveredRoute, Ipv6DiscoveredRoutesContext, Ipv6RouteDiscoveryBindingsContext,
        Ipv6RouteDiscoveryContext, Ipv6RouteDiscoveryState,
    };
    pub use crate::internal::device::router_solicitation::{
        RsContext, RsHandler, RsState, RsTimerId, MAX_RTR_SOLICITATION_DELAY,
        RTR_SOLICITATION_INTERVAL,
    };
    pub use crate::internal::device::slaac::{
        InnerSlaacTimerId, SlaacAddressEntry, SlaacAddressEntryMut, SlaacAddresses,
        SlaacAddrsMutAndConfig, SlaacBindingsContext, SlaacConfiguration, SlaacContext,
        SlaacCounters, SlaacState, SlaacTimerId, TemporarySlaacAddressConfiguration,
        SLAAC_MIN_REGEN_ADVANCE,
    };
    pub use crate::internal::device::state::{
        AddressIdIter, AssignedAddress, DefaultHopLimit, DualStackIpDeviceState, IpDeviceAddresses,
        IpDeviceConfiguration, IpDeviceFlags, IpDeviceMulticastGroups, IpDeviceStateBindingsTypes,
        IpDeviceStateIpExt, Ipv4AddrConfig, Ipv4AddressEntry, Ipv4AddressState,
        Ipv4DeviceConfiguration, Ipv4DeviceConfigurationAndFlags, Ipv6AddrConfig,
        Ipv6AddrManualConfig, Ipv6AddressEntry, Ipv6AddressFlags, Ipv6AddressState, Ipv6DadState,
        Ipv6DeviceConfiguration, Ipv6DeviceConfigurationAndFlags, Ipv6NetworkLearnedParameters,
        Lifetime, SlaacConfig, TemporarySlaacConfig,
    };
    pub use crate::internal::device::{
        add_ip_addr_subnet_with_config, clear_ipv4_device_state, clear_ipv6_device_state,
        del_ip_addr_inner, get_ipv4_addr_subnet, get_ipv6_hop_limit, is_ip_device_enabled,
        is_ip_forwarding_enabled, join_ip_multicast, join_ip_multicast_with_config,
        leave_ip_multicast, leave_ip_multicast_with_config, receive_igmp_packet,
        with_assigned_ipv4_addr_subnets, AddressRemovedReason, DelIpAddr, IpAddressId,
        IpAddressIdSpec, IpAddressIdSpecContext, IpAddressState, IpDeviceAddr,
        IpDeviceAddressContext, IpDeviceAddressIdContext, IpDeviceBindingsContext,
        IpDeviceConfigurationContext, IpDeviceEvent, IpDeviceIpExt, IpDeviceSendContext,
        IpDeviceStateContext, IpDeviceTimerId, Ipv4DeviceTimerId, Ipv6DeviceAddr,
        Ipv6DeviceConfigurationContext, Ipv6DeviceContext, Ipv6DeviceHandler, Ipv6DeviceTimerId,
        WithIpDeviceConfigurationMutInner, WithIpv6DeviceConfigurationMutInner,
    };

    /// IP device test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::device::slaac::testutil::{
            calculate_slaac_addr_sub, collect_slaac_timers_integration,
        };
        pub use crate::internal::device::testutil::with_assigned_ipv6_addr_subnets;
    }
}

/// Group management protocols.
pub mod gmp {
    pub use crate::internal::gmp::igmp::{
        IgmpContext, IgmpGroupState, IgmpState, IgmpStateContext, IgmpTimerId,
        IGMP_DEFAULT_UNSOLICITED_REPORT_INTERVAL,
    };
    pub use crate::internal::gmp::mld::{
        MldContext, MldGroupState, MldStateContext, MldTimerId,
        MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL,
    };
    pub use crate::internal::gmp::{
        GmpDelayedReportTimerId, GmpHandler, GmpQueryHandler, GmpStateRef, IpExt, MulticastGroupSet,
    };
}

/// The Internet Control Message Protocol (ICMP).
pub mod icmp {
    pub use crate::internal::icmp::socket::{
        BoundSockets, IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpEchoSocketApi,
        IcmpSocketId, IcmpSocketSet, IcmpSocketState, IcmpSockets,
        StateContext as IcmpSocketStateContext,
    };
    pub use crate::internal::icmp::{
        send_icmpv4_host_unreachable, send_icmpv6_address_unreachable, send_ndp_packet,
        IcmpBindingsContext, IcmpBindingsTypes, IcmpErrorCode, IcmpIpExt, IcmpIpTransportContext,
        IcmpRxCounters, IcmpRxCountersInner, IcmpState, IcmpStateContext, IcmpTxCounters,
        IcmpTxCountersInner, Icmpv4ErrorCode, Icmpv4StateBuilder, Icmpv6ErrorCode,
        InnerIcmpContext, InnerIcmpv4Context, NdpCounters, NdpRxCounters, NdpTxCounters,
        REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
    };

    /// ICMP test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::icmp::testutil::{
            neighbor_advertisement_ip_packet, neighbor_solicitation_ip_packet,
        };
    }
}

/// Marker traits controlling IP context behavior.
pub mod marker {
    pub use crate::internal::base::{UseIpSocketContextBlanket, UseTransportIpContextBlanket};
    pub use crate::internal::socket::{UseDeviceIpSocketHandlerBlanket, UseIpSocketHandlerBlanket};
}

/// Neighbor Unreachability Detection.
pub mod nud {
    pub use crate::internal::device::nud::api::{
        NeighborApi, NeighborRemovalError, StaticNeighborInsertionError,
    };
    pub use crate::internal::device::nud::{
        confirm_reachable, ConfirmationFlags, Delay, DelegateNudContext, DynamicNeighborState,
        DynamicNeighborUpdateSource, Event, EventDynamicState, EventKind, EventState, Incomplete,
        LinkResolutionContext, LinkResolutionNotifier, LinkResolutionResult, NeighborState,
        NudBindingsContext, NudBindingsTypes, NudConfigContext, NudContext, NudCounters,
        NudCountersInner, NudHandler, NudIcmpContext, NudIpHandler, NudSenderContext, NudState,
        NudTimerId, NudUserConfig, NudUserConfigUpdate, Reachable, Stale, UseDelegateNudContext,
        MAX_ENTRIES,
    };
    pub use crate::internal::device::state::RETRANS_TIMER_DEFAULT;

    /// NUD test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::device::nud::testutil::{
            assert_dynamic_neighbor_state, assert_dynamic_neighbor_with_addr,
            assert_neighbor_unknown, FakeLinkResolutionNotifier,
        };
    }
}

/// IP Layer definitions supporting sockets.
pub mod socket {
    pub use crate::internal::socket::ipv6_source_address_selection::{
        select_ipv6_source_address, SasCandidate,
    };
    pub use crate::internal::socket::{
        DefaultSendOptions, DeviceIpSocketHandler, IpSock, IpSockCreateAndSendError,
        IpSockCreationError, IpSockDefinition, IpSockSendError, IpSocketBindingsContext,
        IpSocketContext, IpSocketHandler, Mms, MmsError, SendOptions,
    };

    /// IP Socket test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::socket::testutil::{FakeDeviceConfig, FakeDualStackIpSocketCtx};
    }
}

/// Raw IP sockets.
pub mod raw {
    pub use crate::internal::raw::protocol::RawIpSocketProtocol;
    pub use crate::internal::raw::state::{RawIpSocketLockedState, RawIpSocketState};
    pub use crate::internal::raw::{
        RawIpSocketApi, RawIpSocketId, RawIpSocketMap, RawIpSocketMapContext,
        RawIpSocketStateContext, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes,
        RawIpSocketsIpExt, WeakRawIpSocketId,
    };
}

pub use internal::api::{RoutesAnyApi, RoutesApi};
pub use internal::base::{
    gen_ip_packet_id, receive_ipv4_packet, receive_ipv4_packet_action, receive_ipv6_packet,
    receive_ipv6_packet_action, resolve_route_to_destination, AddressStatus, DropReason,
    FilterHandlerProvider, HopLimits, IpCounters, IpDeviceContext, IpDeviceStateContext, IpExt,
    IpLayerBindingsContext, IpLayerContext, IpLayerEvent, IpLayerHandler, IpLayerIpExt,
    IpLayerTimerId, IpStateContext, IpStateInner, IpTransportContext, IpTransportDispatchContext,
    Ipv4PresentAddressStatus, Ipv4State, Ipv4StateBuilder, Ipv6PresentAddressStatus, Ipv6State,
    Ipv6StateBuilder, MulticastMembershipHandler, ReceivePacketAction, ResolveRouteError,
    SendIpPacketMeta, TransparentLocalDelivery, TransportIpContext, TransportReceiveError,
    DEFAULT_HOP_LIMITS, DEFAULT_TTL, IPV6_DEFAULT_SUBNET,
};
pub use internal::forwarding::{
    request_context_add_route, request_context_del_routes, AddRouteError, ForwardingTable,
    IpForwardingDeviceContext,
};
pub use internal::path_mtu::{PmtuCache, PmtuContext};
pub use internal::reassembly::{FragmentContext, FragmentTimerId, IpPacketFragmentCache};
pub use internal::types::{
    AddableEntry, AddableEntryEither, AddableMetric, Destination, Entry, EntryEither, Generation,
    IpTypesIpExt, Metric, NextHop, RawMetric, ResolvedRoute, RoutableIpAddr, WrapBroadcastMarker,
};

/// IP layer test utilities.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::internal::base::testutil::DualStackSendIpPacketMeta;
    pub use crate::internal::forwarding::testutil::{
        add_route, del_device_routes, del_routes_to_subnet,
    };
}
