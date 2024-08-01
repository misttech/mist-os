// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to multicast routes.

use alloc::fmt::Debug;
use alloc::sync::Arc;
use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6Scope};
use net_types::{MulticastAddress as _, ScopeableAddress as _, SpecifiedAddress as _, UnicastAddr};
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

/// A witness type wrapping [`Ipv4Addr`], proving the following properties:
/// * the inner address is multicast, and
/// * the inner address's scope is greater than link local.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Ipv4DestinationAddr {
    addr: Ipv4Addr,
}

impl Ipv4DestinationAddr {
    /// Construct a new [`Ipv4DestinationAddr`].
    ///
    /// `None` if the provided address does not have the required properties.
    fn new(addr: Ipv4Addr) -> Option<Self> {
        // As per RFC 5771 Section 4:
        //   Addresses in the Local Network Control Block are used for protocol
        //   control traffic that is not forwarded off link.
        if addr.is_multicast() && !Ipv4::LINK_LOCAL_MULTICAST_SUBNET.contains(&addr) {
            Some(Ipv4DestinationAddr { addr })
        } else {
            None
        }
    }
}

/// A witness type wrapping [`Ipv6Addr`], proving the following properties:
/// * the inner address is multicast, and
/// * the inner address's scope is greater than link local.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Ipv6DestinationAddr {
    addr: Ipv6Addr,
}

impl Ipv6DestinationAddr {
    /// Construct a new [`Ipv6DestinationAddr`].
    ///
    /// `None` if the provided address does not have the required properties.
    fn new(addr: Ipv6Addr) -> Option<Self> {
        // As per RFC 4291 Section 2.7:
        //   Routers must not forward any multicast packets beyond of the scope
        //   indicated by the scop field in the destination multicast address.
        if addr.is_multicast()
            && addr.scope().multicast_scope_id() > Ipv6Scope::MULTICAST_SCOPE_ID_LINK_LOCAL
        {
            Some(Ipv6DestinationAddr { addr })
        } else {
            None
        }
    }
}

/// IP extension trait for multicast routes.
pub trait MulticastRouteIpExt: Ip {
    /// The type of source address used in [`MulticastRouteKey`].
    type SourceAddress: Clone + Debug + Eq + Ord + PartialEq + PartialOrd;
    /// The type of destination address used in [`MulticastRouteKey`].
    type DestinationAddress: Clone + Debug + Eq + Ord + PartialEq + PartialOrd;
}

impl MulticastRouteIpExt for Ipv4 {
    type SourceAddress = Ipv4SourceAddr;
    type DestinationAddress = Ipv4DestinationAddr;
}

impl MulticastRouteIpExt for Ipv6 {
    type SourceAddress = UnicastAddr<Ipv6Addr>;
    type DestinationAddress = Ipv6DestinationAddr;
}

/// The attributes of a multicast route that uniquely identify it.
#[derive(Clone, Debug, Eq, GenericOverIp, Ord, PartialEq, PartialOrd)]
#[generic_over_ip(I, Ip)]
pub struct MulticastRouteKey<I: MulticastRouteIpExt> {
    /// The source address packets must have in order to use this route.
    src_addr: I::SourceAddress,
    /// The destination address packets must have in order to use this route.
    dst_addr: I::DestinationAddress,
}

impl<I: MulticastRouteIpExt> MulticastRouteKey<I> {
    /// Construct a new [`MulticastRouteKey`].
    ///
    /// `None` if the provided addresses do not have the required properties.
    pub fn new(src_addr: I::Addr, dst_addr: I::Addr) -> Option<MulticastRouteKey<I>> {
        I::map_ip(
            (src_addr, dst_addr),
            |(src_addr, dst_addr)| {
                Some(MulticastRouteKey {
                    src_addr: Ipv4SourceAddr::new(src_addr)?,
                    dst_addr: Ipv4DestinationAddr::new(dst_addr)?,
                })
            },
            |(src_addr, dst_addr)| {
                Some(MulticastRouteKey {
                    src_addr: UnicastAddr::new(src_addr)?,
                    dst_addr: Ipv6DestinationAddr::new(dst_addr)?,
                })
            },
        )
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
    Forward(MulticastRouteTargets<D>),
}

/// The collection of targets out of which to forward a multicast packet.
///
/// Note, storing the targets behind an `Arc` allows us to return a reference
/// to the targets, to contexts that are not protected by the multicast route
/// table lock, without cloning the underlying data. Here, an `Arc<Mutex<...>>`
/// is unnecessary, because the underlying targets list is never modified. This
/// is not to say that a route's targets are never modified (e.g. device removal
/// prunes the list of targets); in such cases the route's target list is
/// *replaced* with a new allocation. This strategy allows us to avoid
/// additional locking on the hot path, at the cost of extra allocations for
/// certain control operations.
pub(crate) type MulticastRouteTargets<D> = Arc<[MulticastRouteTarget<D>]>;

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
        targets: MulticastRouteTargets<D>,
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
    const MULTICAST_V4: Ipv4Addr = net_ip_v4!("224.0.1.1");
    const LL_MULTICAST_V4: Ipv4Addr = net_ip_v4!("224.0.0.1");
    const UNICAST_V6: Ipv6Addr = net_ip_v6!("2001:0DB8::1");
    const MULTICAST_V6: Ipv6Addr = net_ip_v6!("ff0e::1");
    const LL_MULTICAST_V6: Ipv6Addr = net_ip_v6!("ff02::1");

    #[test_case(UNICAST_V4, MULTICAST_V4 => true; "success")]
    #[test_case(UNICAST_V4, UNICAST_V4 => false; "unicast_dst")]
    #[test_case(UNICAST_V4, Ipv4::UNSPECIFIED_ADDRESS => false; "unspecified_dst")]
    #[test_case(MULTICAST_V4, MULTICAST_V4 => false; "multicast_src")]
    #[test_case(Ipv4::UNSPECIFIED_ADDRESS, MULTICAST_V4 => false; "unspecified_src")]
    #[test_case(UNICAST_V4, LL_MULTICAST_V4 => false; "ll_multicast_dst")]
    fn new_ipv4_route_key(src_addr: Ipv4Addr, dst_addr: Ipv4Addr) -> bool {
        MulticastRouteKey::<Ipv4>::new(src_addr, dst_addr).is_some()
    }

    #[test_case(UNICAST_V6, MULTICAST_V6 => true; "success")]
    #[test_case(UNICAST_V6, UNICAST_V6 => false; "unicast_dst")]
    #[test_case(UNICAST_V6, Ipv6::UNSPECIFIED_ADDRESS => false; "unspecified_dst")]
    #[test_case(MULTICAST_V6, MULTICAST_V6 => false; "multicast_src")]
    #[test_case(Ipv6::UNSPECIFIED_ADDRESS, MULTICAST_V6 => false; "unspecified_src")]
    #[test_case(UNICAST_V6, LL_MULTICAST_V6 => false; "ll_multicast_dst")]
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
