// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of IP.

use alloc::collections::HashMap;
use core::sync::atomic::AtomicU16;

use lock_order::lock::{DelegatedOrderedLockAccess, LockLevelFor, UnlockedAccess};
use lock_order::relation::LockBefore;
use log::trace;
use net_types::ip::{Ip, IpMarked, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr};
use net_types::{MulticastAddr, SpecifiedAddr};
use netstack3_base::socket::SocketIpAddr;
use netstack3_base::{
    CounterContext, Icmpv4ErrorCode, Icmpv6ErrorCode, TokenBucket, WeakDeviceIdentifier,
};
use netstack3_datagram as datagram;
use netstack3_device::{DeviceId, WeakDeviceId};
use netstack3_icmp_echo::{
    self as icmp_echo, IcmpEchoBoundStateContext, IcmpEchoContextMarker,
    IcmpEchoIpTransportContext, IcmpEchoStateContext, IcmpSocketId, IcmpSocketSet, IcmpSocketState,
    IcmpSockets,
};
use netstack3_ip::device::{self, IidSecret, IpDeviceBindingsContext, IpDeviceIpExt};
use netstack3_ip::icmp::{
    self, IcmpIpTransportContext, IcmpRxCounters, IcmpState, IcmpTxCounters, InnerIcmpContext,
    InnerIcmpv4Context, NdpCounters,
};
use netstack3_ip::multicast_forwarding::MulticastForwardingState;
use netstack3_ip::raw::RawIpSocketMap;
use netstack3_ip::{
    self as ip, FragmentContext, IpCounters, IpDeviceContext, IpLayerBindingsContext, IpLayerIpExt,
    IpPacketFragmentCache, IpRouteTableContext, IpRouteTablesContext, IpStateContext, IpStateInner,
    IpTransportContext, IpTransportDispatchContext, Marks, MulticastMembershipHandler, PmtuCache,
    PmtuContext, ReceiveIpPacketMeta, ResolveRouteError, ResolvedRoute, RoutingTable,
    RoutingTableId, RulesTable, TransportReceiveError,
};
use netstack3_sync::rc::Primary;
use netstack3_sync::RwLock;
use netstack3_tcp::TcpIpTransportContext;
use netstack3_udp::UdpIpTransportContext;
use packet::BufferMut;
use packet_formats::ip::{IpProto, Ipv4Proto, Ipv6Proto};

use crate::context::prelude::*;
use crate::context::WrapLockLevel;
use crate::{BindingsContext, BindingsTypes, CoreCtx, StackState};

impl<I, BT, L> FragmentContext<I, BT> for CoreCtx<'_, BT, L>
where
    I: IpLayerIpExt,
    BT: BindingsTypes,
    L: LockBefore<crate::lock_ordering::IpStateFragmentCache<I>>,
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<I, BT>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<I>>();
        cb(&mut cache)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv4>>>
    PmtuContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv4, BC>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv6>>>
    PmtuContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv6, BC>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<
        I: Ip + IpDeviceIpExt + IpLayerIpExt,
        BC: BindingsContext
            + IpDeviceBindingsContext<I, Self::DeviceId>
            + IpLayerBindingsContext<I, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::IpState<I>>,
    > MulticastMembershipHandler<I, BC> for CoreCtx<'_, BC, L>
where
    Self: device::IpDeviceConfigurationContext<I, BC> + IpStateContext<I> + IpDeviceContext<I>,
{
    fn join_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        ip::device::join_ip_multicast::<I, _, _>(self, bindings_ctx, device, addr)
    }

    fn leave_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        ip::device::leave_ip_multicast::<I, _, _>(self, bindings_ctx, device, addr)
    }

    fn select_device_for_multicast_group(
        &mut self,
        addr: MulticastAddr<I::Addr>,
        marks: &Marks,
    ) -> Result<Self::DeviceId, ResolveRouteError> {
        let remote_ip = SocketIpAddr::new_from_multicast(addr);
        let ResolvedRoute {
            src_addr: _,
            device,
            local_delivery_device,
            next_hop: _,
            internal_forwarding: _,
        } = ip::resolve_output_route_to_destination(self, None, None, Some(remote_ip), marks)?;
        // NB: Because the original address is multicast, it cannot be assigned
        // to a local interface. Thus local delivery should never be requested.
        debug_assert!(local_delivery_device.is_none(), "{:?}", local_delivery_device);
        Ok(device)
    }
}

impl<BT: BindingsTypes, I: datagram::DualStackIpExt>
    UnlockedAccess<crate::lock_ordering::IcmpTxCounters<I>> for StackState<BT>
{
    type Data = IcmpTxCounters<I>;
    type Guard<'l>
        = &'l IcmpTxCounters<I>
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.inner_icmp_state().tx_counters
    }
}

impl<BT: BindingsTypes, I: datagram::DualStackIpExt, L> CounterContext<IcmpTxCounters<I>>
    for CoreCtx<'_, BT, L>
{
    fn with_counters<O, F: FnOnce(&IcmpTxCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IcmpTxCounters<I>>())
    }
}

impl<BT: BindingsTypes, I: datagram::DualStackIpExt>
    UnlockedAccess<crate::lock_ordering::IcmpRxCounters<I>> for StackState<BT>
{
    type Data = IcmpRxCounters<I>;
    type Guard<'l>
        = &'l IcmpRxCounters<I>
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.inner_icmp_state().rx_counters
    }
}

impl<BT: BindingsTypes, I: datagram::DualStackIpExt, L> CounterContext<IcmpRxCounters<I>>
    for CoreCtx<'_, BT, L>
{
    fn with_counters<O, F: FnOnce(&IcmpRxCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IcmpRxCounters<I>>())
    }
}

impl<BT: BindingsTypes> UnlockedAccess<crate::lock_ordering::NdpCounters> for StackState<BT> {
    type Data = NdpCounters;
    type Guard<'l>
        = &'l NdpCounters
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv6.icmp.ndp_counters
    }
}

impl<BT: BindingsTypes, L> CounterContext<NdpCounters> for CoreCtx<'_, BT, L> {
    fn with_counters<O, F: FnOnce(&NdpCounters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::NdpCounters>())
    }
}

impl<BT: BindingsTypes> UnlockedAccess<crate::lock_ordering::IcmpSendTimestampReply<Ipv4>>
    for StackState<BT>
{
    type Data = bool;
    type Guard<'l>
        = &'l bool
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv4.icmp.send_timestamp_reply
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv4>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>
            + LockBefore<crate::lock_ordering::UdpAllSocketsSet<Ipv4>>,
    > InnerIcmpv4Context<BC> for CoreCtx<'_, BC, L>
{
    fn should_send_timestamp_reply(&self) -> bool {
        *self.unlocked_access::<crate::lock_ordering::IcmpSendTimestampReply<Ipv4>>()
    }
}

impl<BT: BindingsTypes, I: IpLayerIpExt, L> CounterContext<IpCounters<I>> for CoreCtx<'_, BT, L> {
    fn with_counters<O, F: FnOnce(&IpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IpStateCounters<I>>())
    }
}

impl<BT: BindingsTypes, I: IpLayerIpExt> UnlockedAccess<crate::lock_ordering::IpStateCounters<I>>
    for StackState<BT>
{
    type Data = IpCounters<I>;
    type Guard<'l>
        = &'l IpCounters<I>
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.inner_ip_state().counters()
    }
}

impl<BT: BindingsTypes, I: IpLayerIpExt> UnlockedAccess<crate::lock_ordering::IpMainTableId<I>>
    for StackState<BT>
{
    type Data = RoutingTableId<I, DeviceId<BT>>;
    type Guard<'l> = &'l RoutingTableId<I, DeviceId<BT>>;

    fn access(&self) -> Self::Guard<'_> {
        self.inner_ip_state::<I>().main_table_id()
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, BC, L> IpStateContext<I> for CoreCtx<'_, BC, L>
where
    I: IpLayerIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::IpStateRulesTable<I>>,
{
    type IpRouteTablesCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IpStateRulesTable<I>>>;

    fn with_rules_table<
        O,
        F: FnOnce(&mut Self::IpRouteTablesCtx<'_>, &RulesTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (rules_table, mut restricted) =
            self.read_lock_and::<crate::lock_ordering::IpStateRulesTable<I>>();
        cb(&mut restricted, &rules_table)
    }

    fn with_rules_table_mut<
        O,
        F: FnOnce(&mut Self::IpRouteTablesCtx<'_>, &mut RulesTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut rules_table, mut restricted) =
            self.write_lock_and::<crate::lock_ordering::IpStateRulesTable<I>>();
        cb(&mut restricted, &mut rules_table)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, BC, L> IpRouteTablesContext<I> for CoreCtx<'_, BC, L>
where
    I: IpLayerIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::IpStateRoutingTables<I>>,
{
    type Ctx<'a> = CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IpStateRoutingTables<I>>>;

    fn main_table_id(&self) -> RoutingTableId<I, Self::DeviceId> {
        self.unlocked_access::<crate::lock_ordering::IpMainTableId<I>>().clone()
    }

    fn with_ip_routing_tables<
        O,
        F: FnOnce(
            &mut Self::Ctx<'_>,
            &HashMap<
                RoutingTableId<I, Self::DeviceId>,
                Primary<RwLock<RoutingTable<I, Self::DeviceId>>>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (table, mut ctx) = self.lock_and::<crate::lock_ordering::IpStateRoutingTables<I>>();
        cb(&mut ctx, &table)
    }

    fn with_ip_routing_tables_mut<
        O,
        F: FnOnce(
            &mut HashMap<
                RoutingTableId<I, Self::DeviceId>,
                Primary<RwLock<RoutingTable<I, Self::DeviceId>>>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let mut tables = self.lock::<crate::lock_ordering::IpStateRoutingTables<I>>();
        cb(&mut *tables)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, BC, L> IpRouteTableContext<I> for CoreCtx<'_, BC, L>
where
    I: IpLayerIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::IpStateRoutingTable<I>>,
{
    type IpDeviceIdCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IpStateRoutingTable<I>>>;

    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        table_id: &RoutingTableId<I, Self::DeviceId>,
        cb: F,
    ) -> O {
        let mut table = self.adopt(table_id);
        let (table, mut restricted) = table
            .read_lock_with_and::<crate::lock_ordering::IpStateRoutingTable<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &table)
    }

    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        table_id: &RoutingTableId<I, Self::DeviceId>,
        cb: F,
    ) -> O {
        let mut table = self.adopt(table_id);
        let (mut table, mut restricted) = table
            .write_lock_with_and::<crate::lock_ordering::IpStateRoutingTable<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &mut table)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<Ipv4>>>
    IpTransportDispatchContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        proto: Ipv4Proto,
        body: B,
        meta: ReceiveIpPacketMeta<Ipv4>,
    ) -> Result<(), TransportReceiveError> {
        match proto {
            Ipv4Proto::Icmp => {
                <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    meta,
                )
                .map_err(|(_body, err)| err)
            }
            Ipv4Proto::Igmp => {
                device::receive_igmp_packet(self, bindings_ctx, device, src_ip, dst_ip, body);
                Ok(())
            }
            Ipv4Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    meta,
                )
                .map_err(|(_body, err)| err)
            }
            Ipv4Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    meta,
                )
                .map_err(|(_body, err)| err)
            }
            // TODO(joshlf): Once all IP protocol numbers are covered, remove
            // this default case.
            _ => Err(TransportReceiveError::ProtocolUnsupported),
        }
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<Ipv6>>>
    IpTransportDispatchContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        proto: Ipv6Proto,
        body: B,
        meta: ReceiveIpPacketMeta<Ipv6>,
    ) -> Result<(), TransportReceiveError> {
        match proto {
            Ipv6Proto::Icmpv6 => {
                <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    meta,
                )
                .map_err(|(_body, err)| err)
            }
            // A value of `Ipv6Proto::NoNextHeader` tells us that there is no
            // header whatsoever following the last lower-level header so we stop
            // processing here.
            Ipv6Proto::NoNextHeader => Ok(()),
            Ipv6Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    meta,
                )
                .map_err(|(_body, err)| err)
            }
            Ipv6Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    meta,
                )
                .map_err(|(_body, err)| err)
            }
            // TODO(joshlf): Once all IP Next Header numbers are covered, remove
            // this default case.
            _ => Err(TransportReceiveError::ProtocolUnsupported),
        }
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv4>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>
            + LockBefore<crate::lock_ordering::UdpAllSocketsSet<Ipv4>>,
    > InnerIcmpContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type EchoTransportContext = IcmpEchoIpTransportContext;

    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        original_src_ip: Option<SpecifiedAddr<Ipv4Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv4Addr>,
        original_proto: Ipv4Proto,
        original_body: &[u8],
        err: Icmpv4ErrorCode,
    ) {
        self.increment(|counters: &IpCounters<Ipv4>| &counters.receive_icmp_error);
        trace!("InnerIcmpContext<Ipv4>::receive_icmp_error({:?})", err);

        match original_proto {
            Ipv4Proto::Icmp => {
                <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv4Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv4Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered,
            // remove this default case.
            _ => <() as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                self,
                bindings_ctx,
                device,
                original_src_ip,
                original_dst_ip,
                original_body,
                err,
            ),
        }
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv4>>())
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv6>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv6>>
            + LockBefore<crate::lock_ordering::UdpAllSocketsSet<Ipv6>>,
    > InnerIcmpContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type EchoTransportContext = IcmpEchoIpTransportContext;

    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        original_src_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv6Addr>,
        original_next_header: Ipv6Proto,
        original_body: &[u8],
        err: Icmpv6ErrorCode,
    ) {
        self.increment(|counters: &IpCounters<Ipv6>| &counters.receive_icmp_error);
        trace!("InnerIcmpContext<Ipv6>::receive_icmp_error({:?})", err);

        match original_next_header {
            Ipv6Proto::Icmpv6 => {
                <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv6Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv6Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered,
            // remove this default case.
            _ => <() as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                self,
                bindings_ctx,
                device,
                original_src_ip,
                original_dst_ip,
                original_body,
                err,
            ),
        }
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv6>>())
    }
}

impl<L, BC: BindingsContext> icmp::IcmpStateContext for CoreCtx<'_, BC, L> {}

impl<BT: BindingsTypes, L> IcmpEchoContextMarker for CoreCtx<'_, BT, L> {}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<I>>>
    IcmpEchoStateContext<I, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IcmpSocketState<I>>>;

    fn with_all_sockets_mut<O, F: FnOnce(&mut IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.write_lock::<crate::lock_ordering::IcmpAllSocketsSet<I>>())
    }

    fn with_all_sockets<O, F: FnOnce(&IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::IcmpAllSocketsSet<I>>())
    }

    fn with_socket_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id);
        let (socket_state, mut restricted) =
            locked.read_lock_with_and::<crate::lock_ordering::IcmpSocketState<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &socket_state)
    }

    fn with_socket_state_mut<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id);
        let (mut socket_state, mut restricted) = locked
            .write_lock_with_and::<crate::lock_ordering::IcmpSocketState<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &mut socket_state)
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked::<crate::lock_ordering::IcmpSocketState<I>>())
    }

    fn for_each_socket<
        F: FnMut(
            &mut Self::SocketStateCtx<'_>,
            &IcmpSocketId<I, Self::WeakDeviceId, BC>,
            &IcmpSocketState<I, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        mut cb: F,
    ) {
        let (all_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::IcmpAllSocketsSet<I>>();
        all_sockets.keys().for_each(|id| {
            let id = IcmpSocketId::from(id.clone());
            let mut locked = locked.adopt(&id);
            let (socket_state, mut restricted) = locked
                .read_lock_with_and::<crate::lock_ordering::IcmpSocketState<I>, _>(|c| c.right());
            let mut restricted = restricted.cast_core_ctx();
            cb(&mut restricted, &id, &socket_state);
        });
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpBoundMap<I>>>
    IcmpEchoBoundStateContext<I, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IcmpBoundMap<I>>>;
    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp_echo::BoundSockets<I, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<I>>();
        cb(&mut core_ctx, &mut sockets)
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> DelegatedOrderedLockAccess<IpPacketFragmentCache<I, BT>>
    for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IpStateFragmentCache<I>
{
    type Data = IpPacketFragmentCache<I, BT>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes> DelegatedOrderedLockAccess<PmtuCache<I, BT>>
    for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IpStateRulesTable<I>
{
    type Data = RulesTable<I, DeviceId<BT>>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes> DelegatedOrderedLockAccess<RulesTable<I, DeviceId<BT>>>
    for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IpStatePmtuCache<I>
{
    type Data = PmtuCache<I, BT>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IpStateRoutingTables<I>
{
    type Data =
        HashMap<RoutingTableId<I, DeviceId<BT>>, Primary<RwLock<RoutingTable<I, DeviceId<BT>>>>>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<
        HashMap<RoutingTableId<I, DeviceId<BT>>, Primary<RwLock<RoutingTable<I, DeviceId<BT>>>>>,
    > for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<RoutingTableId<I, DeviceId<BT>>>
    for crate::lock_ordering::IpStateRoutingTable<I>
{
    type Data = RoutingTable<I, DeviceId<BT>>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<MulticastForwardingState<I, DeviceId<BT>, BT>> for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IpMulticastForwardingState<I>
{
    type Data = MulticastForwardingState<I, DeviceId<BT>, BT>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<RawIpSocketMap<I, WeakDeviceId<BT>, BT>> for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::AllRawIpSockets<I>
{
    type Data = RawIpSocketMap<I, WeakDeviceId<BT>, BT>;
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<icmp_echo::BoundSockets<I, WeakDeviceId<BT>, BT>>
    for StackState<BT>
{
    type Inner = IcmpSockets<I, WeakDeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.transport.icmp_echo_state()
    }
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IcmpBoundMap<I>
{
    type Data = icmp_echo::BoundSockets<I, WeakDeviceId<BT>, BT>;
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<IcmpSocketSet<I, WeakDeviceId<BT>, BT>> for StackState<BT>
{
    type Inner = IcmpSockets<I, WeakDeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.transport.icmp_echo_state()
    }
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IcmpAllSocketsSet<I>
{
    type Data = IcmpSocketSet<I, WeakDeviceId<BT>, BT>;
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<IpMarked<I, TokenBucket<BT::Instant>>> for StackState<BT>
{
    type Inner = IcmpState<I, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_icmp_state()
    }
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IcmpTokenBucket<I>
{
    type Data = IpMarked<I, TokenBucket<BT::Instant>>;
}

impl<I: datagram::DualStackIpExt, D: WeakDeviceIdentifier, BT: BindingsTypes>
    LockLevelFor<IcmpSocketId<I, D, BT>> for crate::lock_ordering::IcmpSocketState<I>
{
    type Data = IcmpSocketState<I, D, BT>;
}

impl<BT: BindingsTypes> UnlockedAccess<crate::lock_ordering::Ipv4StateNextPacketId>
    for StackState<BT>
{
    type Data = AtomicU16;
    type Guard<'l>
        = &'l AtomicU16
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv4.next_packet_id
    }
}

impl<BT: BindingsTypes> UnlockedAccess<crate::lock_ordering::SlaacTempSecretKey>
    for StackState<BT>
{
    type Data = IidSecret;
    type Guard<'l>
        = &'l IidSecret
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv6.slaac_temp_secret_key
    }
}
