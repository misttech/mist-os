// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types and helpers used in local-delivery of packets.

use core::num::NonZeroU16;

use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};
use net_types::SpecifiedAddr;
use netstack3_base::IpExt;
use packet_formats::ip::DscpAndEcn;
use packet_formats::ipv4::options::Ipv4Option;
use packet_formats::ipv4::Ipv4Header as _;
use packet_formats::ipv6::ext_hdrs::{HopByHopOptionData, Ipv6ExtensionHeaderData};
use packet_formats::ipv6::Ipv6Header as _;

/// Informs the transport layer of parameters for transparent local delivery.
#[derive(Debug, GenericOverIp, Clone)]
#[generic_over_ip(I, Ip)]
pub struct TransparentLocalDelivery<I: IpExt> {
    /// The local delivery address.
    pub addr: SpecifiedAddr<I::Addr>,
    /// The local delivery port.
    pub port: NonZeroU16,
}

/// Meta information for an incoming packet.
#[derive(Debug, GenericOverIp, Clone)]
#[generic_over_ip(I, Ip)]
pub struct ReceiveIpPacketMeta<I: IpExt> {
    /// Indicates that the packet was sent to a broadcast address.
    pub broadcast: Option<I::BroadcastMarker>,

    /// Destination overrides for the transparent proxy.
    pub transparent_override: Option<TransparentLocalDelivery<I>>,
}

/// Information for an incoming packet.
///
/// This is given to upper layers to provide extra information on locally
/// delivered IP packets.
#[derive(Debug, Clone)]
pub struct LocalDeliveryPacketInfo<I: IpExt, H: IpHeaderInfo<I>> {
    /// Packet metadata calculated by the stack.
    pub meta: ReceiveIpPacketMeta<I>,
    /// Accessor for extra information in IP header.
    pub header_info: H,
}

/// Abstracts extracting information from IP headers for upper layers.
///
/// Implemented for the combination of fixed header and options of IPv4 and IPv6
/// packets, so we can ensure this information is always extracted from packets,
/// including when they're rewritten by filters.
///
/// This is a trait so:
/// - We're gating here and acknowledging all the information necessary by upper
///   layers.
/// - A [fake implementation] can be provided without constructing a full
///   IPv4/IPv6 packet.
///
/// [fake implementation]: testutil::FakeIpHeaderInfo
pub trait IpHeaderInfo<I> {
    /// DSCP and ECN values received in Traffic Class or TOS field.
    fn dscp_and_ecn(&self) -> DscpAndEcn;

    /// The TTL (IPv4) or Hop Limit (IPv6) of the received packet.
    fn hop_limit(&self) -> u8;

    /// Returns true if the router alert option (IPv4) or extension header
    /// (IPv6) is present on the packet.
    fn router_alert(&self) -> bool;
}

pub(crate) struct Ipv4HeaderInfo<'a> {
    pub(crate) prefix: &'a packet_formats::ipv4::HeaderPrefix,
    pub(crate) options: packet_formats::ipv4::Options<&'a [u8]>,
}

impl IpHeaderInfo<Ipv4> for Ipv4HeaderInfo<'_> {
    fn dscp_and_ecn(&self) -> DscpAndEcn {
        self.prefix.dscp_and_ecn()
    }

    fn hop_limit(&self) -> u8 {
        self.prefix.ttl()
    }

    fn router_alert(&self) -> bool {
        self.options.iter().any(|opt| matches!(opt, Ipv4Option::RouterAlert { .. }))
    }
}

pub(crate) struct Ipv6HeaderInfo<'a> {
    pub(crate) fixed: &'a packet_formats::ipv6::FixedHeader,
    pub(crate) extension: packet_formats::ipv6::ExtensionHeaders<'a>,
}

impl IpHeaderInfo<Ipv6> for Ipv6HeaderInfo<'_> {
    fn dscp_and_ecn(&self) -> DscpAndEcn {
        self.fixed.dscp_and_ecn()
    }

    fn hop_limit(&self) -> u8 {
        self.fixed.hop_limit()
    }

    fn router_alert(&self) -> bool {
        self.extension.iter().any(|h| match h.data() {
            Ipv6ExtensionHeaderData::HopByHopOptions { options } => {
                options.iter().any(|h| matches!(h.data, HopByHopOptionData::RouterAlert { .. }))
            }
            _ => false,
        })
    }
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    /// Handroll a default impl for `ReceiveIpPacketMeta` only for tests to
    /// prevent accidental usage.
    impl<I: IpExt> Default for ReceiveIpPacketMeta<I> {
        fn default() -> Self {
            Self { broadcast: None, transparent_override: None }
        }
    }

    impl<I: IpExt> Default for LocalDeliveryPacketInfo<I, FakeIpHeaderInfo> {
        fn default() -> Self {
            Self { meta: Default::default(), header_info: Default::default() }
        }
    }

    /// A fake implementation of [`IpHeaderInfo`].
    #[derive(Debug, Default)]
    pub struct FakeIpHeaderInfo {
        /// The value returned by [`IpHeaderInfo::dscp_and_ecn`].
        pub dscp_and_ecn: DscpAndEcn,
        /// The value returned by [`IpHeaderInfo::hop_limit`].
        pub hop_limit: u8,
        /// The value returned by [`IpHeaderInfo::router_alert`].
        pub router_alert: bool,
    }

    impl<I: IpExt> IpHeaderInfo<I> for FakeIpHeaderInfo {
        fn dscp_and_ecn(&self) -> DscpAndEcn {
            self.dscp_and_ecn
        }

        fn hop_limit(&self) -> u8 {
            self.hop_limit
        }

        fn router_alert(&self) -> bool {
            self.router_alert
        }
    }
}
