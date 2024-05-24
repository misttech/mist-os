// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of IP.

use core::sync::atomic::AtomicU16;

use lock_order::{
    lock::{DelegatedOrderedLockAccess, LockLevelFor, UnlockedAccess},
    relation::LockBefore,
    wrap::prelude::*,
};
use net_types::{
    ip::{Ip, IpMarked, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr},
    MulticastAddr, SpecifiedAddr,
};
use packet::BufferMut;
use packet_formats::ip::{IpProto, Ipv4Proto, Ipv6Proto};
use tracing::trace;

use crate::{
    context::CounterContext,
    data_structures::token_bucket::TokenBucket,
    device::{AnyDevice, DeviceId, DeviceIdContext, WeakDeviceId, WeakDeviceIdentifier},
    ip::{
        self,
        device::{self, IpDeviceBindingsContext, IpDeviceIpExt},
        icmp::{
            self, IcmpIpTransportContext, IcmpRxCounters, IcmpSocketId, IcmpSocketSet,
            IcmpSocketState, IcmpSockets, IcmpState, IcmpTxCounters, Icmpv4ErrorCode,
            Icmpv6ErrorCode, InnerIcmpContext, InnerIcmpv4Context, NdpCounters,
        },
        raw::RawIpSocketMap,
        ForwardingTable, FragmentContext, IpCounters, IpDeviceStateContext,
        IpForwardingDeviceContext, IpLayerBindingsContext, IpLayerContext, IpLayerIpExt,
        IpPacketFragmentCache, IpStateContext, IpStateInner, IpTransportContext,
        IpTransportDispatchContext, MulticastMembershipHandler, PmtuCache, PmtuContext,
        ResolveRouteError, TransparentLocalDelivery, TransportReceiveError,
    },
    routes::ResolvedRoute,
    socket::{datagram, SocketIpAddr},
    transport::{tcp::socket::TcpIpTransportContext, udp::UdpIpTransportContext},
    uninstantiable::UninstantiableWrapper,
    BindingsContext, BindingsTypes, CoreCtx, StackState,
};

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
    Self: device::IpDeviceConfigurationContext<I, BC> + IpLayerContext<I, BC>,
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
    ) -> Result<Self::DeviceId, ResolveRouteError> {
        let remote_ip = SocketIpAddr::new_from_multicast(addr);
        let ResolvedRoute { src_addr: _, device, local_delivery_device, next_hop: _ } =
            ip::resolve_route_to_destination(self, None, None, Some(remote_ip))?;
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
    type Guard<'l> = &'l IcmpTxCounters<I> where Self: 'l;

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
    type Guard<'l> = &'l IcmpRxCounters<I> where Self: 'l;

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
    type Guard<'l> = &'l NdpCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ndp_counters()
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
    type Guard<'l> = &'l bool where Self: 'l;

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
    type Guard<'l> = &'l IpCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ip_counters()
    }
}

impl<I, BC, L> IpStateContext<I, BC> for CoreCtx<'_, BC, L>
where
    I: IpLayerIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::IpStateRoutingTable<I>>,

    // These bounds ensure that we can fulfill all the traits for the associated
    // type `IpDeviceIdCtx` below and keep the compiler happy where we don't
    // have implementations that are generic on Ip.
    for<'a> CoreCtx<'a, BC, crate::lock_ordering::IpStateRoutingTable<I>>:
        DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
            + IpForwardingDeviceContext<I>
            + IpDeviceStateContext<I, BC>,
{
    type IpDeviceIdCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IpStateRoutingTable<I>>;

    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (cache, mut locked) =
            self.read_lock_and::<crate::lock_ordering::IpStateRoutingTable<I>>();
        cb(&mut locked, &cache)
    }

    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut cache, mut locked) =
            self.write_lock_and::<crate::lock_ordering::IpStateRoutingTable<I>>();
        cb(&mut locked, &mut cache)
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
        transport_override: Option<TransparentLocalDelivery<Ipv4>>,
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
                    transport_override,
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
                    transport_override,
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
                    transport_override,
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
        transport_override: Option<TransparentLocalDelivery<Ipv6>>,
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
                    transport_override,
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
                    transport_override,
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
                    transport_override,
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
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpBoundMap<Ipv4>>;
    type DualStackContext = UninstantiableWrapper<Self>;

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

    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp::BoundSockets<Ipv4, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<Ipv4>>();
        cb(&mut core_ctx, &mut sockets)
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
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpBoundMap<Ipv6>>;
    type DualStackContext = UninstantiableWrapper<Self>;
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

    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp::BoundSockets<Ipv6, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<Ipv6>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv6>>())
    }
}

impl<L, BC: BindingsContext> icmp::IcmpStateContext for CoreCtx<'_, BC, L> {}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I,
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<I>>
            + LockBefore<crate::lock_ordering::TcpDemux<I>>
            + LockBefore<crate::lock_ordering::UdpBoundMap<I>>,
    > icmp::IcmpSocketStateContext<I, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpSocketState<I>>;

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
        cb(&mut self.cast_locked())
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
    for crate::lock_ordering::IpStatePmtuCache<I>
{
    type Data = PmtuCache<I, BT>;
}

impl<I: IpLayerIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<ForwardingTable<I, DeviceId<BT>>> for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IpStateRoutingTable<I>
{
    type Data = ForwardingTable<I, DeviceId<BT>>;
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
    DelegatedOrderedLockAccess<icmp::BoundSockets<I, WeakDeviceId<BT>, BT>> for StackState<BT>
{
    type Inner = IcmpSockets<I, WeakDeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.inner_icmp_state().sockets
    }
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::IcmpBoundMap<I>
{
    type Data = icmp::BoundSockets<I, WeakDeviceId<BT>, BT>;
}

impl<I: datagram::DualStackIpExt, BT: BindingsTypes>
    DelegatedOrderedLockAccess<IcmpSocketSet<I, WeakDeviceId<BT>, BT>> for StackState<BT>
{
    type Inner = IcmpSockets<I, WeakDeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.inner_icmp_state().sockets
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
    type Inner = IcmpState<I, WeakDeviceId<BT>, BT>;
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
    type Guard<'l> = &'l AtomicU16 where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv4.next_packet_id
    }
}
