// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core IP layer.
//!
//! This crate contains the IP layer for netstack3.

#![no_std]
#![warn(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;
extern crate fakestd as std;

#[path = "."]
mod internal {
    #[macro_use]
    pub(super) mod path_mtu;

    pub(super) mod api;
    pub(super) mod base;
    pub(super) mod device;
    pub(super) mod fragmentation;
    pub(super) mod gmp;
    pub(super) mod icmp;
    pub(super) mod ipv6;
    pub(super) mod multicast_forwarding;
    pub(super) mod raw;
    pub(super) mod reassembly;
    pub(super) mod routing;
    pub(super) mod sas;
    pub(super) mod socket;
    pub(super) mod types;
    pub(super) mod uninstantiable;
}

/// Definitions for devices at the IP layer.
pub mod device {
    pub use crate::internal::device::api::{
        AddIpAddrSubnetError, AddrSubnetAndManualConfigEither, DeviceIpAnyApi, DeviceIpApi,
        SetIpAddressPropertiesError,
    };
    pub use crate::internal::device::config::{
        IpDeviceConfigurationAndFlags, IpDeviceConfigurationHandler, IpDeviceConfigurationUpdate,
        Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate, UpdateIpConfigurationError,
    };
    pub use crate::internal::device::dad::{
        DadAddressContext, DadAddressStateRef, DadContext, DadEvent, DadHandler, DadStateRef,
        DadTimerId,
    };
    pub use crate::internal::device::opaque_iid::{IidSecret, OpaqueIid, OpaqueIidNonce};
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
        SlaacAddrsMutAndConfig, SlaacBindingsContext, SlaacConfiguration, SlaacConfigurationUpdate,
        SlaacContext, SlaacCounters, SlaacState, SlaacTimerId, TemporarySlaacAddressConfiguration,
        SLAAC_MIN_REGEN_ADVANCE,
    };
    pub use crate::internal::device::state::{
        AddressId, AddressIdIter, AssignedAddressState, CommonAddressProperties, DefaultHopLimit,
        DualStackIpDeviceState, IpDeviceAddresses, IpDeviceConfiguration, IpDeviceFlags,
        IpDeviceMulticastGroups, IpDeviceStateBindingsTypes, IpDeviceStateIpExt, Ipv4AddrConfig,
        Ipv4AddressEntry, Ipv4AddressState, Ipv4DeviceConfiguration, Ipv6AddrConfig,
        Ipv6AddrManualConfig, Ipv6AddrSlaacConfig, Ipv6AddressEntry, Ipv6AddressFlags,
        Ipv6AddressState, Ipv6DadState, Ipv6DeviceConfiguration, Ipv6NetworkLearnedParameters,
        Lifetime, PreferredLifetime, PrimaryAddressId, SlaacConfig, TemporarySlaacConfig,
        WeakAddressId,
    };
    pub use crate::internal::device::{
        add_ip_addr_subnet_with_config, clear_ipv4_device_state, clear_ipv6_device_state,
        del_ip_addr_inner, get_ipv4_addr_subnet, get_ipv6_hop_limit, is_ip_device_enabled,
        is_ip_multicast_forwarding_enabled, is_ip_unicast_forwarding_enabled, join_ip_multicast,
        join_ip_multicast_with_config, leave_ip_multicast, leave_ip_multicast_with_config,
        receive_igmp_packet, AddressRemovedReason, DelIpAddr, IpAddressIdSpec,
        IpAddressIdSpecContext, IpAddressState, IpDeviceAddressContext, IpDeviceBindingsContext,
        IpDeviceConfigurationContext, IpDeviceEvent, IpDeviceIpExt, IpDeviceSendContext,
        IpDeviceStateContext, IpDeviceTimerId, Ipv4DeviceTimerId, Ipv6DeviceConfigurationContext,
        Ipv6DeviceContext, Ipv6DeviceHandler, Ipv6DeviceTimerId, WithIpDeviceConfigurationMutInner,
        WithIpv6DeviceConfigurationMutInner,
    };

    /// IP device test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::device::slaac::testutil::{
            calculate_slaac_addr_sub, collect_slaac_timers_integration,
        };
        pub use crate::internal::device::testutil::{
            with_assigned_ipv4_addr_subnets, with_assigned_ipv6_addr_subnets,
        };
    }
}

/// Group management protocols.
pub mod gmp {
    pub use crate::internal::gmp::igmp::{
        IgmpConfigMode, IgmpContext, IgmpContextMarker, IgmpSendContext, IgmpStateContext,
        IgmpTimerId, IgmpTypeLayout, IGMP_DEFAULT_UNSOLICITED_REPORT_INTERVAL,
    };
    pub use crate::internal::gmp::mld::{
        MldConfigMode, MldContext, MldContextMarker, MldSendContext, MldStateContext, MldTimerId,
        MldTypeLayout, MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL,
    };
    pub use crate::internal::gmp::{
        GmpGroupState, GmpHandler, GmpQueryHandler, GmpState, GmpStateRef, GmpTimerId, IpExt,
        MulticastGroupSet,
    };
}

/// The Internet Control Message Protocol (ICMP).
pub mod icmp {
    pub use crate::internal::icmp::{
        send_icmpv4_host_unreachable, send_icmpv6_address_unreachable, send_ndp_packet,
        EchoTransportContextMarker, IcmpBindingsContext, IcmpBindingsTypes, IcmpIpTransportContext,
        IcmpRxCounters, IcmpRxCountersInner, IcmpState, IcmpStateContext, IcmpTxCounters,
        IcmpTxCountersInner, Icmpv4StateBuilder, InnerIcmpContext, InnerIcmpv4Context, NdpCounters,
        NdpRxCounters, NdpTxCounters, REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
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
    pub use crate::internal::socket::{
        OptionDelegationMarker, UseDeviceIpSocketHandlerBlanket, UseIpSocketHandlerBlanket,
    };
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
    pub use crate::internal::socket::{
        DefaultIpSocketOptions, DelegatedRouteResolutionOptions, DelegatedSendOptions,
        DeviceIpSocketHandler, IpSock, IpSockCreateAndSendError, IpSockCreationError,
        IpSockDefinition, IpSockSendError, IpSocketBindingsContext, IpSocketContext,
        IpSocketHandler, MmsError, RouteResolutionOptions, SendOneShotIpPacketError, SendOptions,
        SocketHopLimits,
    };

    /// IP Socket test utilities.
    #[cfg(any(test, feature = "testutils"))]
    pub mod testutil {
        pub use crate::internal::socket::testutil::{
            FakeDeviceConfig, FakeDualStackIpSocketCtx, FakeIpSocketCtx, InnerFakeIpSocketCtx,
        };
    }
}

/// Multicast Forwarding
pub mod multicast_forwarding {
    pub use crate::internal::multicast_forwarding::api::{
        MulticastForwardingApi, MulticastForwardingDisabledError,
    };
    pub use crate::internal::multicast_forwarding::counters::MulticastForwardingCounters;
    pub use crate::internal::multicast_forwarding::packet_queue::MulticastForwardingPendingPackets;
    pub use crate::internal::multicast_forwarding::route::{
        ForwardMulticastRouteError, MulticastRoute, MulticastRouteKey, MulticastRouteStats,
        MulticastRouteTarget, MulticastRouteTargets,
    };
    pub use crate::internal::multicast_forwarding::state::{
        MulticastForwardingEnabledState, MulticastForwardingPendingPacketsContext,
        MulticastForwardingState, MulticastForwardingStateContext, MulticastRouteTable,
        MulticastRouteTableContext,
    };
    pub use crate::internal::multicast_forwarding::{
        MulticastForwardingBindingsContext, MulticastForwardingBindingsTypes,
        MulticastForwardingDeviceContext, MulticastForwardingEvent, MulticastForwardingTimerId,
    };
}

/// Raw IP sockets.
pub mod raw {
    pub use crate::internal::raw::counters::RawIpSocketCounters;
    pub use crate::internal::raw::filter::RawIpSocketIcmpFilter;
    pub use crate::internal::raw::protocol::RawIpSocketProtocol;
    pub use crate::internal::raw::state::{RawIpSocketLockedState, RawIpSocketState};
    pub use crate::internal::raw::{
        RawIpSocketApi, RawIpSocketIcmpFilterError, RawIpSocketId, RawIpSocketMap,
        RawIpSocketMapContext, RawIpSocketSendToError, RawIpSocketStateContext,
        RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes, WeakRawIpSocketId,
    };
}

pub use internal::api::{RoutesAnyApi, RoutesApi};
pub use internal::base::{
    gen_ip_packet_id, receive_ipv4_packet, receive_ipv4_packet_action, receive_ipv6_packet,
    receive_ipv6_packet_action, resolve_output_route_to_destination, AddressStatus,
    BaseTransportIpContext, DeviceIpLayerMetadata, DropReason, FilterHandlerProvider, HopLimits,
    IpCounters, IpDeviceConfirmReachableContext, IpDeviceContext, IpDeviceEgressStateContext,
    IpDeviceIngressStateContext, IpDeviceMtuContext, IpLayerBindingsContext, IpLayerContext,
    IpLayerEvent, IpLayerHandler, IpLayerIpExt, IpLayerTimerId, IpPacketDestination,
    IpRouteTableContext, IpRouteTablesContext, IpSendFrameError, IpSendFrameErrorReason,
    IpStateContext, IpStateInner, IpTransportContext, IpTransportDispatchContext,
    Ipv4PresentAddressStatus, Ipv4State, Ipv4StateBuilder, Ipv6PresentAddressStatus, Ipv6State,
    Ipv6StateBuilder, MulticastMembershipHandler, ReceiveIpPacketMeta, ReceivePacketAction,
    ResolveRouteError, RoutingTableId, SendIpPacketMeta, TransparentLocalDelivery,
    TransportIpContext, TransportReceiveError, DEFAULT_HOP_LIMITS, DEFAULT_TTL,
    IPV6_DEFAULT_SUBNET,
};
pub use internal::fragmentation::FragmentationCounters;
pub use internal::path_mtu::{PmtuCache, PmtuContext};
pub use internal::reassembly::{FragmentContext, FragmentTimerId, IpPacketFragmentCache};
pub use internal::routing::rules::{
    Mark, MarkDomain, MarkMatcher, MarkMatchers, Marks, Rule, RuleAction, RuleMatcher, RulesTable,
    TrafficOriginMatcher,
};
pub use internal::routing::{
    request_context_add_route, request_context_del_routes, AddRouteError, IpRoutingDeviceContext,
    NonLocalSrcAddrPolicy, PacketOrigin, RoutingTable,
};
pub use internal::sas::IpSasHandler;
pub use internal::types::{
    AddableEntry, AddableEntryEither, AddableMetric, Destination, Entry, EntryEither, Generation,
    InternalForwarding, Metric, NextHop, RawMetric, ResolvedRoute, RoutableIpAddr,
};

/// IP layer test utilities.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::internal::base::testutil::DualStackSendIpPacketMeta;
    pub use crate::internal::routing::testutil::{
        add_route, del_device_routes, del_routes_to_subnet, set_rules,
    };
}
