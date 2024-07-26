// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to multicast routes.

use alloc::fmt::Debug;
use alloc::vec::Vec;
use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use net_types::{MulticastAddr, MulticastAddress as _, SpecifiedAddress as _, UnicastAddr};
use netstack3_base::StrongDeviceIdentifier;

/// A witness type wrapping [`Ipv4Addr`], proving the following properties:
/// * the inner address is specified, and
/// * the inner address is not a multicast address.
///
/// Note, unlike for [`Ipv6SourceAddr`], the `UnicastAddr` witness type cannot
/// be used. This is because "unicastness" is not an absolute property of an
/// IPv4 address: it requires knowing the subnet in which the address is being
/// used.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Ipv4SourceAddr {
    addr: Ipv4Addr,
}

impl Ipv4SourceAddr {
    /// Construct a new [`Ipv4SourceAddr`].
    ///
    /// `None` if the provided address does not have the required properties.
    fn new(addr: Ipv4Addr) -> Option<Self> {
        if addr.is_specified() && !addr.is_multicast() {
            Some(Ipv4SourceAddr { addr })
        } else {
            None
        }
    }
}

impl<I: MulticastRouteIpExt> GenericOverIp<I> for Ipv4SourceAddr {
    type Type = I::SourceAddress;
}

/// A witness type wrapping [`Ipv6Addr`], proving the following properties:
/// * the inner address is unicast.
// NB: Use a new-type around `UnicastAddr` so that we can re-implement
// [`GenericOverIp`] in a way that's compatible with [`MulticastRouteIpExt`].
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Ipv6SourceAddr(UnicastAddr<Ipv6Addr>);

impl Ipv6SourceAddr {
    /// Construct a new [`Ipv6SourceAddr`].
    ///
    /// `None` if the provided address does not have the required properties.
    fn new(addr: Ipv6Addr) -> Option<Self> {
        UnicastAddr::new(addr).map(Ipv6SourceAddr)
    }
}

impl<I: MulticastRouteIpExt> GenericOverIp<I> for Ipv6SourceAddr {
    type Type = I::SourceAddress;
}

/// IP extension trait for multicast routes.
pub trait MulticastRouteIpExt: Ip {
    /// The type of source address used in [`MulticastRouteKey`].
    type SourceAddress: Clone
        + Debug
        + Eq
        + Ord
        + PartialEq
        + PartialOrd
        + GenericOverIp<Self, Type = Self::SourceAddress>
        + GenericOverIp<Ipv4, Type = Ipv4SourceAddr>
        + GenericOverIp<Ipv6, Type = Ipv6SourceAddr>;
}

impl MulticastRouteIpExt for Ipv4 {
    type SourceAddress = Ipv4SourceAddr;
}

impl MulticastRouteIpExt for Ipv6 {
    type SourceAddress = Ipv6SourceAddr;
}

/// The attributes of a multicast route that uniquely identify it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MulticastRouteKey<I: MulticastRouteIpExt> {
    /// The source address packets must have in order to use this route.
    src_addr: I::SourceAddress,
    /// The destination address packets must have in order to use this route.
    dst_addr: MulticastAddr<I::Addr>,
}

impl<I: MulticastRouteIpExt> MulticastRouteKey<I> {
    /// Construct a new [`MulticastRouteKey`].
    ///
    /// `None` if the provided addresses do not have the required properties.
    pub fn new(src_addr: I::Addr, dst_addr: I::Addr) -> Option<MulticastRouteKey<I>> {
        let dst_addr = MulticastAddr::new(dst_addr)?;
        let src_addr = I::map_ip::<_, Option<I::SourceAddress>>(
            src_addr,
            Ipv4SourceAddr::new,
            Ipv6SourceAddr::new,
        )?;
        Some(MulticastRouteKey { src_addr, dst_addr })
    }
}

/// All attributes of a multicast route, excluding the [`MulticastRouteKey`].
///
/// This type acts as a witness that the route is valid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MulticastRoute<D: StrongDeviceIdentifier> {
    /// The interface on which packets must arrive in order to use this route.
    pub(crate) input_interface: D,
    /// The route's action.
    pub(crate) action: Action<D>,
}

/// The action to be taken for a packet that matches a route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Action<D: StrongDeviceIdentifier> {
    /// Forward the packet out of each provided [`Target`].
    Forward(Targets<D>),
}

/// The collection of targets out of which to forward a multicast packet.
type Targets<D> = Vec<MulticastRouteTarget<D>>;

/// The target out of which to forward a multicast packet.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MulticastRouteTarget<D: StrongDeviceIdentifier> {
    /// An interface the packet should be forwarded out of.
    pub output_interface: D,
    /// The minimum TTL of the packet in order for it to be forwarded.
    ///
    /// A value of 0 allows packets to be forwarded, regardless of their TTL.
    pub min_ttl: u8,
}

/// Errors returned by [`MulticastRoute::new_forward`].
#[derive(Debug, Eq, PartialEq)]
pub enum ForwardMulticastRouteError {
    /// The route's list of targets is empty.
    EmptyTargetList,
    /// The route's `input_interface` is also listed as a target. This would
    /// create a routing loop.
    InputInterfaceIsTarget,
    /// The route lists the same [`Target`] output_interface multiple times.
    DuplicateTarget,
}

impl<D: StrongDeviceIdentifier> MulticastRoute<D> {
    /// Construct a new [`MulticastRoute`] with [`Action::Forward`].
    pub fn new_forward(
        input_interface: D,
        targets: Targets<D>,
    ) -> Result<Self, ForwardMulticastRouteError> {
        if targets.is_empty() {
            return Err(ForwardMulticastRouteError::EmptyTargetList);
        }
        if targets.iter().any(|MulticastRouteTarget { output_interface, min_ttl: _ }| {
            output_interface == &input_interface
        }) {
            return Err(ForwardMulticastRouteError::InputInterfaceIsTarget);
        }

        // NB: Search for duplicates by doing a naive n^2 comparison. This is
        // expected to be more performant than other approaches (e.g.
        // sort + dedup, or collecting into a hash map) given how small the vec
        // is expected to be.
        for (index, target_a) in targets.iter().enumerate() {
            // NB: Only check the targets that occur in the vec *after* this
            // one. The targets before this one were checked in previous
            // iterations.
            if targets[index + 1..]
                .iter()
                .any(|target_b| target_a.output_interface == target_b.output_interface)
            {
                return Err(ForwardMulticastRouteError::DuplicateTarget);
            }
        }

        Ok(MulticastRoute { input_interface, action: Action::Forward(targets) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec;
    use alloc::vec::Vec;
    use net_declare::{net_ip_v4, net_ip_v6};
    use netstack3_base::testutil::MultipleDevicesId;
    use test_case::test_case;

    const UNICAST_V4: Ipv4Addr = net_ip_v4!("192.0.2.1");
    const MULTICAST_V4: Ipv4Addr = net_ip_v4!("224.0.0.1");
    const UNICAST_V6: Ipv6Addr = net_ip_v6!("2001:0DB8::1");
    const MULTICAST_V6: Ipv6Addr = net_ip_v6!("ff0e::2");

    #[test_case(UNICAST_V4, MULTICAST_V4 => true; "success")]
    #[test_case(UNICAST_V4, UNICAST_V4 => false; "unicast_dst")]
    #[test_case(UNICAST_V4, Ipv4::UNSPECIFIED_ADDRESS => false; "unspecified_dst")]
    #[test_case(MULTICAST_V4, MULTICAST_V4 => false; "multicast_src")]
    #[test_case(Ipv4::UNSPECIFIED_ADDRESS, MULTICAST_V4 => false; "unspecified_src")]
    fn new_ipv4_route_key(src_addr: Ipv4Addr, dst_addr: Ipv4Addr) -> bool {
        MulticastRouteKey::<Ipv4>::new(src_addr, dst_addr).is_some()
    }

    #[test_case(UNICAST_V6, MULTICAST_V6 => true; "success")]
    #[test_case(UNICAST_V6, UNICAST_V6 => false; "unicast_dst")]
    #[test_case(UNICAST_V6, Ipv6::UNSPECIFIED_ADDRESS => false; "unspecified_dst")]
    #[test_case(MULTICAST_V6, MULTICAST_V6 => false; "multicast_src")]
    #[test_case(Ipv6::UNSPECIFIED_ADDRESS, MULTICAST_V6 => false; "unspecified_src")]
    fn new_ipv6_route_key(src_addr: Ipv6Addr, dst_addr: Ipv6Addr) -> bool {
        MulticastRouteKey::<Ipv6>::new(src_addr, dst_addr).is_some()
    }

    #[test_case(MultipleDevicesId::A, vec![] =>
        Some(ForwardMulticastRouteError::EmptyTargetList); "empty_target_list")]
    #[test_case(MultipleDevicesId::A, vec![MultipleDevicesId::A] =>
        Some(ForwardMulticastRouteError::InputInterfaceIsTarget); "input_interface_is_target")]
    #[test_case(MultipleDevicesId::A, vec![MultipleDevicesId::B, MultipleDevicesId::B] =>
        Some(ForwardMulticastRouteError::DuplicateTarget); "duplicate_target")]
    #[test_case(MultipleDevicesId::A, vec![MultipleDevicesId::B, MultipleDevicesId::C] =>
        None; "valid_route")]
    fn new_forward(
        input_interface: MultipleDevicesId,
        output_interfaces: Vec<MultipleDevicesId>,
    ) -> Option<ForwardMulticastRouteError> {
        let targets = output_interfaces
            .into_iter()
            .map(|output_interface| MulticastRouteTarget { output_interface, min_ttl: 0 })
            .collect();
        MulticastRoute::new_forward(input_interface, targets).err()
    }
}
