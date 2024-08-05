// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of multicast forwarding.
//!
//! Multicast forwarding is the ability for netstack to forward multicast
//! packets that arrive on an interface out multiple interfaces (while also
//! optionally delivering the packet to the host itself if the arrival host has
//! an interest in the packet).
//!
//! Note that multicast forwarding decisions are made by consulting the
//! multicast routing table, a routing table entirely separate from the unicast
//! routing table(s).

pub(crate) mod api;
pub(crate) mod route;
pub(crate) mod state;

use packet_formats::ip::IpPacket;
use zerocopy::ByteSlice;

use route::{Action, MulticastRouteTargets};

use crate::multicast_forwarding::{
    MulticastForwardingStateContext, MulticastRoute, MulticastRouteKey,
    MulticastRouteTableContext as _,
};
use crate::{IpDeviceContext, IpLayerIpExt};

/// Query the multicast route table and return the forwarding targets.
///
/// `None` may be returned in several situations:
///   * if multicast forwarding is disabled (either stack-wide or for the
///     provided `dev`),
///   * if the packets src/dst addrs are not viable for multicast forwarding
///     (see the requirements on [`MulticastRouteKey`]), or
///   * if the route table does not have an entry suitable for this packet.
///
/// In the latter case, the packet is stashed in the
/// [`MulticastForwardingPendingPackets`] table, and a relevant event is
/// dispatched to bindings.
///
/// Note that the returned targets are not synchronized with the multicast route
/// table and may grow stale if the table is updated.
pub(crate) fn lookup_multicast_route_for_packet<I, B, CC, BC>(
    core_ctx: &mut CC,
    packet: &I::Packet<B>,
    dev: &CC::DeviceId,
) -> Option<MulticastRouteTargets<CC::DeviceId>>
where
    I: IpLayerIpExt,
    B: ByteSlice,
    CC: MulticastForwardingStateContext<I> + IpDeviceContext<I, BC>,
{
    // Short circuit if the packet's addresses don't constitute a valid
    // multicast route key (e.g. src is not unicast, or dst is not multicast).
    let key = MulticastRouteKey::new(packet.src_ip(), packet.dst_ip())?;

    // Short circuit if the device has forwarding disabled.
    if !core_ctx.is_device_multicast_forwarding_enabled(dev) {
        // TODO(https://fxbug.dev/352570820): Increment a counter.
        return None;
    }

    core_ctx.with_state(|state, ctx| {
        // Short circuit if forwarding is disabled stack-wide.
        let Some(state) = state.enabled() else {
            // TODO(https://fxbug.dev/352570820): Increment a counter.
            return None;
        };
        ctx.with_route_table(state, |route_table, _ctx| {
            if let Some(MulticastRoute { input_interface, action }) = route_table.get(&key) {
                if dev != input_interface {
                    // TODO(https://fxbug.dev/352570820): Increment a counter.
                    // TODO(https://fxbug.dev/353328975): Send a
                    // "Wrong Interface" multicast forwarding event.
                    return None;
                }
                match action {
                    // TODO(https://fxbug.dev/352570820): Increment a counter.
                    Action::Forward(targets) => return Some(targets.clone()),
                }
            }
            // TODO(https://fxbug.dev/352570820): Increment a counter.
            // TODO(https://fxbug.dev/353328975): Queue the packet and send a
            // "Missing Route" multicast forwarding event.
            return None;
        })
    })
}

#[cfg(test)]
mod testutil {
    use super::*;

    use alloc::collections::HashSet;
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use core::num::NonZeroU8;

    use derivative::Derivative;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu};
    use net_types::SpecifiedAddr;
    use netstack3_base::testutil::{FakeStrongDeviceId, MultipleDevicesId};
    use netstack3_base::CtxPair;

    use crate::device::IpDeviceAddr;
    use crate::multicast_forwarding::{
        MulticastForwardingApi, MulticastForwardingEnabledState, MulticastForwardingPendingPackets,
        MulticastForwardingPendingPacketsContext, MulticastForwardingState, MulticastRouteTable,
        MulticastRouteTableContext,
    };
    use crate::{AddressStatus, IpDeviceStateContext};

    /// An IP extension trait providing constants for various IP addresses.
    pub(crate) trait TestIpExt: IpLayerIpExt {
        const SRC1: Self::Addr;
        const SRC2: Self::Addr;
        const DST1: Self::Addr;
        const DST2: Self::Addr;
    }

    impl TestIpExt for Ipv4 {
        const SRC1: Ipv4Addr = net_ip_v4!("192.0.2.1");
        const SRC2: Ipv4Addr = net_ip_v4!("192.0.2.2");
        const DST1: Ipv4Addr = net_ip_v4!("224.0.1.1");
        const DST2: Ipv4Addr = net_ip_v4!("224.0.1.2");
    }

    impl TestIpExt for Ipv6 {
        const SRC1: Ipv6Addr = net_ip_v6!("2001:0DB8::1");
        const SRC2: Ipv6Addr = net_ip_v6!("2001:0DB8::2");
        const DST1: Ipv6Addr = net_ip_v6!("ff0e::1");
        const DST2: Ipv6Addr = net_ip_v6!("ff0e::2");
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    pub(crate) struct FakeCoreCtxState<I: IpLayerIpExt, D: FakeStrongDeviceId> {
        // NB: Hold in an `Rc<RefCell<...>>` to switch to runtime borrow
        // checking. This allows us to borrow the multicast forwarding state at
        // the same time as the outer `FakeCoreCtx` is mutably borrowed.
        pub(crate) multicast_forwarding: Rc<RefCell<MulticastForwardingState<I, D>>>,
        // The list of devices that have multicast forwarding enabled.
        pub(crate) forwarding_enabled_devices: HashSet<D>,
    }

    impl<I: IpLayerIpExt, D: FakeStrongDeviceId> FakeCoreCtxState<I, D> {
        pub(crate) fn set_multicast_forwarding_enabled_for_dev(&mut self, dev: D, enabled: bool) {
            if enabled {
                let _: bool = self.forwarding_enabled_devices.insert(dev);
            } else {
                let _: bool = self.forwarding_enabled_devices.remove(&dev);
            }
        }
    }

    pub(crate) type FakeBindingsCtx = netstack3_base::testutil::FakeBindingsCtx<(), (), (), ()>;
    pub(crate) type FakeCoreCtx<I, D> =
        netstack3_base::testutil::FakeCoreCtx<FakeCoreCtxState<I, D>, (), D>;

    impl<I: IpLayerIpExt, D: FakeStrongDeviceId> MulticastForwardingStateContext<I>
        for FakeCoreCtx<I, D>
    {
        type Ctx<'a> = FakeCoreCtx<I, D>;
        fn with_state<
            O,
            F: FnOnce(&MulticastForwardingState<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let state = self.state.multicast_forwarding.clone();
            let borrow = state.borrow();
            cb(&borrow, self)
        }
        fn with_state_mut<
            O,
            F: FnOnce(&mut MulticastForwardingState<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let state = self.state.multicast_forwarding.clone();
            let mut borrow = state.borrow_mut();
            cb(&mut borrow, self)
        }
    }

    impl<I: IpLayerIpExt, D: FakeStrongDeviceId> MulticastRouteTableContext<I> for FakeCoreCtx<I, D> {
        type Ctx<'a> = FakeCoreCtx<I, D>;
        fn with_route_table<
            O,
            F: FnOnce(&MulticastRouteTable<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
            cb: F,
        ) -> O {
            let route_table = state.route_table().read();
            cb(&route_table, self)
        }
        fn with_route_table_mut<
            O,
            F: FnOnce(&mut MulticastRouteTable<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
            cb: F,
        ) -> O {
            let mut route_table = state.route_table().write();
            cb(&mut route_table, self)
        }
    }

    impl<I: IpLayerIpExt, D: FakeStrongDeviceId> MulticastForwardingPendingPacketsContext<I>
        for FakeCoreCtx<I, D>
    {
        fn with_pending_table_mut<
            O,
            F: FnOnce(&mut MulticastForwardingPendingPackets<I, Self::DeviceId>) -> O,
        >(
            &mut self,
            state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
            cb: F,
        ) -> O {
            let mut pending_table = state.pending_table().lock();
            cb(&mut pending_table)
        }
    }

    impl<I: IpLayerIpExt, D: FakeStrongDeviceId> IpDeviceStateContext<I, FakeBindingsCtx>
        for FakeCoreCtx<I, D>
    {
        fn with_next_packet_id<O, F: FnOnce(&I::PacketIdState) -> O>(&self, _cb: F) -> O {
            unimplemented!("");
        }
        fn get_local_addr_for_remote(
            &mut self,
            _device_id: &Self::DeviceId,
            _remote: Option<SpecifiedAddr<I::Addr>>,
        ) -> Option<IpDeviceAddr<I::Addr>> {
            unimplemented!();
        }
        fn get_hop_limit(&mut self, _device_id: &Self::DeviceId) -> NonZeroU8 {
            unimplemented!();
        }
        fn address_status_for_device(
            &mut self,
            _addr: SpecifiedAddr<I::Addr>,
            _device_id: &Self::DeviceId,
        ) -> AddressStatus<I::AddressStatus> {
            unimplemented!();
        }
    }

    impl<I: IpLayerIpExt, D: FakeStrongDeviceId> IpDeviceContext<I, FakeBindingsCtx>
        for FakeCoreCtx<I, D>
    {
        fn is_ip_device_enabled(&mut self, _device_id: &Self::DeviceId) -> bool {
            todo!()
        }
        type DeviceAndAddressStatusIter<'a> = core::iter::Empty<(Self::DeviceId, I::AddressStatus)>;
        fn with_address_statuses<F: FnOnce(Self::DeviceAndAddressStatusIter<'_>) -> R, R>(
            &mut self,
            _addr: SpecifiedAddr<I::Addr>,
            _cb: F,
        ) -> R {
            unimplemented!()
        }
        fn is_device_unicast_forwarding_enabled(&mut self, _device_id: &Self::DeviceId) -> bool {
            unimplemented!()
        }
        fn is_device_multicast_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
            self.state.forwarding_enabled_devices.contains(device_id)
        }
        fn get_mtu(&mut self, _device_id: &Self::DeviceId) -> Mtu {
            unimplemented!()
        }
        fn confirm_reachable(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            _device: &Self::DeviceId,
            _neighbor: SpecifiedAddr<I::Addr>,
        ) {
            unimplemented!()
        }
    }

    pub(crate) fn new_api<I: IpLayerIpExt>(
    ) -> MulticastForwardingApi<I, CtxPair<FakeCoreCtx<I, MultipleDevicesId>, FakeBindingsCtx>>
    {
        MulticastForwardingApi::new(CtxPair::with_core_ctx(FakeCoreCtx::with_state(
            Default::default(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ip_test_macro::ip_test;
    use netstack3_base::testutil::MultipleDevicesId;
    use packet::{InnerPacketBuilder, ParseBuffer, Serializer};
    use packet_formats::ip::{IpPacketBuilder, IpProto};

    use crate::multicast_forwarding::MulticastRouteTarget;
    use test_case::test_case;
    use testutil::TestIpExt;

    /// Constructs a buffer containing an IP packet with sensible defaults.
    fn new_ip_packet_buf<I: IpLayerIpExt>(
        src_addr: I::Addr,
        dst_addr: I::Addr,
    ) -> impl AsRef<[u8]> {
        const TTL: u8 = 255;
        /// Arbitrary data to put inside of an IP packet.
        const IP_BODY: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        IP_BODY
            .into_serializer()
            .encapsulate(I::PacketBuilder::new(src_addr, dst_addr, TTL, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
    }

    struct LookupTestCase {
        // Whether multicast forwarding is enabled for the netstack.
        enabled: bool,
        // Whether multicast forwarding is enabled for the device.
        dev_enabled: bool,
        // Whether the packet has the correct src/dst addrs.
        right_key: bool,
        // Whether the packet arrived on the correct device.
        right_dev: bool,
    }
    const LOOKUP_SUCCESS_CASE: LookupTestCase =
        LookupTestCase { enabled: true, dev_enabled: true, right_key: true, right_dev: true };

    #[ip_test(I)]
    #[test_case(LOOKUP_SUCCESS_CASE => true; "success")]
    #[test_case(LookupTestCase{enabled: false, ..LOOKUP_SUCCESS_CASE} => false; "disabled")]
    #[test_case(LookupTestCase{dev_enabled: false, ..LOOKUP_SUCCESS_CASE} => false; "dev_disabled")]
    #[test_case(LookupTestCase{right_key: false, ..LOOKUP_SUCCESS_CASE} => false; "wrong_key")]
    #[test_case(LookupTestCase{right_dev: false, ..LOOKUP_SUCCESS_CASE} => false; "wrong_dev")]
    fn lookup_route<I: TestIpExt>(test_case: LookupTestCase) -> bool {
        let LookupTestCase { enabled, dev_enabled, right_key, right_dev } = test_case;
        let mut api = testutil::new_api::<I>();

        let buf = new_ip_packet_buf::<I>(I::SRC1, I::DST1);
        let mut buf_ref = buf.as_ref();
        let packet = buf_ref.parse::<I::Packet<_>>().expect("parse should succeed");

        let key = if right_key {
            MulticastRouteKey::new(I::SRC1, I::DST1).unwrap()
        } else {
            MulticastRouteKey::new(I::SRC2, I::DST2).unwrap()
        };

        let dev = if right_dev { MultipleDevicesId::A } else { MultipleDevicesId::B };

        if enabled {
            assert!(api.enable());
            // NB: Only attempt to install the route when enabled; Otherwise
            // installation fails.
            assert_eq!(
                api.add_multicast_route(
                    key,
                    MulticastRoute::new_forward(
                        dev,
                        [MulticastRouteTarget {
                            output_interface: MultipleDevicesId::C,
                            min_ttl: 0
                        }]
                        .into()
                    )
                    .unwrap()
                ),
                Ok(None)
            );
        }

        api.core_ctx()
            .state
            .set_multicast_forwarding_enabled_for_dev(MultipleDevicesId::A, dev_enabled);

        lookup_multicast_route_for_packet(api.core_ctx(), &packet, &MultipleDevicesId::A).is_some()
    }
}
