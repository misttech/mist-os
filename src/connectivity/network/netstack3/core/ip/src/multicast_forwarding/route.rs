// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to multicast routes.

use alloc::vec::Vec;
use net_types::ip::Ip;
use net_types::{MulticastAddr, UnicastAddr};
use netstack3_base::StrongDeviceIdentifier;

/// The attributes of a multicast route that uniquely identify it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MulticastRouteKey<I: Ip> {
    /// The source address packets must have in order to use this route.
    pub src_addr: UnicastAddr<I::Addr>,
    /// The destination address packets must have in order to use this route.
    pub dst_addr: MulticastAddr<I::Addr>,
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
    use netstack3_base::testutil::MultipleDevicesId;
    use test_case::test_case;

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
