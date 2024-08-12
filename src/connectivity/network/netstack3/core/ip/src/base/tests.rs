// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::rc::Rc;
use core::cell::RefCell;

use assert_matches::assert_matches;
use ip_test_macro::ip_test;
use netstack3_base::socket::SocketIpAddr;
use netstack3_base::testutil::{FakeDeviceId, TestIpExt};
use netstack3_base::CtxPair;
use packet::InnerPacketBuilder as _;
use packet_formats::ip::IpProto;

use crate::internal::routing::testutil;
use crate::{Destination, Entry, Metric, NextHop, RawMetric};

use super::*;

struct IpFakeCoreCtx<I: IpLayerIpExt> {
    counters: IpCounters<I>,
    rules_table: Rc<RefCell<RulesTable<I, FakeDeviceId>>>,
    main_table_id: RoutingTableId<I, FakeDeviceId>,
    routing_tables: Rc<
        RefCell<
            HashMap<
                RoutingTableId<I, FakeDeviceId>,
                PrimaryRc<RwLock<RoutingTable<I, FakeDeviceId>>>,
            >,
        >,
    >,
}

impl<I: IpLayerIpExt> Default for IpFakeCoreCtx<I> {
    fn default() -> Self {
        let main_table = PrimaryRc::new(RwLock::new(Default::default()));
        let main_table_id = RoutingTableId::new(PrimaryRc::clone_strong(&main_table));
        let route_tables =
            HashMap::from_iter(core::iter::once((main_table_id.clone(), main_table)));
        let rules_table = Rc::new(RefCell::new(RulesTable::new(main_table_id.clone())));
        Self {
            rules_table,
            routing_tables: Rc::new(RefCell::new(route_tables)),
            main_table_id,
            counters: Default::default(),
        }
    }
}

impl<I: IpLayerIpExt> CounterContext<IpCounters<I>> for IpFakeCoreCtx<I> {
    fn with_counters<O, F: FnOnce(&IpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(&self.counters)
    }
}

type FakeCoreCtx<I> = netstack3_base::testutil::FakeCoreCtx<
    IpFakeCoreCtx<I>,
    SendIpPacketMeta<I, FakeDeviceId, SpecifiedAddr<<I as Ip>::Addr>>,
    FakeDeviceId,
>;
type FakeBindingsCtx = netstack3_base::testutil::FakeBindingsCtx<(), (), (), ()>;
type FakeCtx<I> = CtxPair<FakeCoreCtx<I>, FakeBindingsCtx>;

impl<I: IpLayerIpExt, BC> IpDeviceSendContext<I, BC> for FakeCoreCtx<I> {
    fn send_ip_frame<S>(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _destination: IpPacketDestination<I, &Self::DeviceId>,
        _body: S,
        _egress_proof: filter::ProofOfEgressCheck,
    ) -> Result<(), netstack3_base::SendFrameError<S>>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut,
    {
        unimplemented!()
    }
}

#[ip_test(I)]
fn no_loopback_addrs_on_the_wire<I: IpLayerIpExt + TestIpExt>() {
    let mut ctx = FakeCtx::default();
    const TTL: u8 = 1;
    let frame = [].into_serializer().encapsulate(I::PacketBuilder::new(
        I::TEST_ADDRS.local_ip.get(),
        I::LOOPBACK_ADDRESS.get(),
        TTL,
        IpProto::Udp.into(),
    ));
    let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
    let result = send_ip_frame(
        core_ctx,
        bindings_ctx,
        &FakeDeviceId,
        IpPacketDestination::Neighbor(I::TEST_ADDRS.remote_ip),
        frame,
        IpLayerPacketMetadata::default(),
    )
    .map_err(|e| e.into_err());
    assert_eq!(result, Err(IpSendFrameErrorReason::IllegalLoopbackAddress));
}

impl<I: IpLayerIpExt> IpRoutingDeviceContext<I> for FakeCoreCtx<I> {
    fn get_routing_metric(&mut self, _device_id: &Self::DeviceId) -> RawMetric {
        unimplemented!()
    }

    fn is_ip_device_enabled(&mut self, _device_id: &Self::DeviceId) -> bool {
        true
    }
}

impl<I: IpLayerIpExt, BC> IpDeviceStateContext<I, BC> for FakeCoreCtx<I> {
    fn with_next_packet_id<O, F: FnOnce(&I::PacketIdState) -> O>(&self, _cb: F) -> O {
        unimplemented!()
    }

    fn get_local_addr_for_remote(
        &mut self,
        _device_id: &Self::DeviceId,
        _remote: Option<SpecifiedAddr<I::Addr>>,
    ) -> Option<SocketIpAddr<I::Addr>> {
        unimplemented!()
    }

    fn get_hop_limit(&mut self, _device_id: &Self::DeviceId) -> NonZeroU8 {
        unimplemented!()
    }

    fn address_status_for_device(
        &mut self,
        _addr: SpecifiedAddr<I::Addr>,
        _device_id: &Self::DeviceId,
    ) -> AddressStatus<I::AddressStatus> {
        unimplemented!()
    }
}

impl<I: IpLayerIpExt, BC> IpStateContext<I, BC> for FakeCoreCtx<I> {
    type IpRouteTablesCtx<'a> = Self;

    fn with_rules_table<
        O,
        F: FnOnce(&mut Self::IpRouteTablesCtx<'_>, &RulesTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let rules_table = self.state.rules_table.clone();
        let rules_table = rules_table.borrow();
        cb(self, &rules_table)
    }

    fn with_rules_table_mut<
        O,
        F: FnOnce(&mut Self::IpRouteTablesCtx<'_>, &mut RulesTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let rules_table = self.state.rules_table.clone();
        let mut rules_table = rules_table.borrow_mut();
        cb(self, &mut rules_table)
    }
}

impl<I: IpLayerIpExt, BC> IpRouteTablesContext<I, BC> for FakeCoreCtx<I> {
    type IpDeviceIdCtx<'a> = Self;

    fn main_table_id(&self) -> RoutingTableId<I, Self::DeviceId> {
        self.state.main_table_id.clone()
    }

    fn with_ip_routing_tables_mut<
        O,
        F: FnOnce(
            &mut HashMap<
                RoutingTableId<I, Self::DeviceId>,
                PrimaryRc<RwLock<RoutingTable<I, Self::DeviceId>>>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let route_tables = self.state.routing_tables.clone();
        let mut route_tables = route_tables.borrow_mut();
        cb(&mut route_tables)
    }

    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        table_id: &RoutingTableId<I, Self::DeviceId>,
        cb: F,
    ) -> O {
        let table = table_id.0.read();
        cb(self, &table)
    }

    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        table_id: &RoutingTableId<I, Self::DeviceId>,
        cb: F,
    ) -> O {
        let mut table = table_id.0.write();
        cb(self, &mut table)
    }
}

#[ip_test(I)]
fn test_walk_rules<I: IpLayerIpExt + TestIpExt>() {
    let mut ctx = FakeCoreCtx::<I>::default();

    // An unreachable rule should short-circuit the lookup.
    ctx.state
        .rules_table
        .borrow_mut()
        .rules_mut()
        .insert(0, Rule { action: RuleAction::Unreachable });

    assert_eq!(
        walk_rules::<_, (), _, _, _, _>(&mut ctx, (), |(), _core_ctx, _table| panic!(
            "should not be able to look up tables"
        )),
        ControlFlow::Break(RuleAction::<core::convert::Infallible>::Unreachable)
    );

    // On the other hand, an empty routing table should not stop the lookup.
    let route_table_2 = PrimaryRc::new(RwLock::new(RoutingTable::default()));
    let table_id = RoutingTableId::new(PrimaryRc::clone_strong(&route_table_2));
    ctx.state.rules_table.borrow_mut().rules_mut()[0] =
        Rule { action: RuleAction::Lookup(table_id.clone()) };
    assert_matches!(ctx.state.routing_tables.borrow_mut().insert(table_id, route_table_2), None);
    let _entry = testutil::add_entry(
        &mut IpRouteTablesContext::<I, ()>::main_table_id(&ctx).get_mut(),
        Entry {
            subnet: I::TEST_ADDRS.subnet,
            device: FakeDeviceId,
            gateway: None,
            metric: Metric::ExplicitMetric(RawMetric(0)),
        },
    )
    .expect("failed to install route entry");

    assert_eq!(
        walk_rules::<_, (), _, _, _, _>(&mut ctx, (), |(), core_ctx, table| {
            match table.lookup(core_ctx, None, I::TEST_ADDRS.remote_ip.get()) {
                None => ControlFlow::Continue(()),
                Some(dest) => ControlFlow::Break(dest),
            }
        }),
        ControlFlow::Break(RuleAction::Lookup(Destination {
            device: FakeDeviceId,
            next_hop: NextHop::RemoteAsNeighbor
        }))
    );
}
