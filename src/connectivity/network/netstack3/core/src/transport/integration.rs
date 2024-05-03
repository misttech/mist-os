// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    wrap::prelude::*,
};
use net_types::ip::{Ip, IpInvariant, Ipv4, Ipv6};

use crate::{
    context::{CoreTimerContext, CounterContext},
    device::{self, WeakDeviceId},
    socket::{datagram, MaybeDualStack},
    transport::{
        tcp::{
            self,
            socket::{
                isn::IsnGenerator, TcpSocketId, TcpSocketSet, TcpSocketState, WeakTcpSocketId,
            },
            TcpCounters,
        },
        udp::{self, UdpSocketId, UdpSocketSet, UdpSocketState},
        TransportLayerTimerId,
    },
    uninstantiable::{Uninstantiable, UninstantiableWrapper},
    BindingsContext, BindingsTypes, CoreCtx, StackState,
};

impl<I, BC, L> CoreTimerContext<WeakTcpSocketId<I, WeakDeviceId<BC>, BC>, BC> for CoreCtx<'_, BC, L>
where
    I: tcp::socket::DualStackIpExt,
    BC: BindingsContext,
{
    fn convert_timer(dispatch_id: WeakTcpSocketId<I, WeakDeviceId<BC>, BC>) -> BC::DispatchId {
        TransportLayerTimerId::Tcp(dispatch_id.into()).into()
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, L, BC> tcp::socket::TcpDemuxContext<I, WeakDeviceId<BC>, BC> for CoreCtx<'_, BC, L>
where
    I: Ip,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::TcpDemux<I>>,
{
    type IpTransportCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::TcpDemux<I>>;
    fn with_demux<O, F: FnOnce(&tcp::socket::DemuxState<I, WeakDeviceId<BC>, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::TcpDemux<I>>())
    }

    fn with_demux_mut<O, F: FnOnce(&mut tcp::socket::DemuxState<I, WeakDeviceId<BC>, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.write_lock::<crate::lock_ordering::TcpDemux<I>>())
    }

    fn with_demux_mut_and_ip_transport_ctx<
        O,
        F: FnOnce(
            &mut tcp::socket::DemuxState<I, WeakDeviceId<BC>, BC>,
            &mut Self::IpTransportCtx<'_>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut demux_state, mut restricted) =
            self.write_lock_and::<crate::lock_ordering::TcpDemux<I>>();
        cb(&mut demux_state, &mut restricted)
    }
}

impl<L, BC> tcp::socket::TcpContext<Ipv4, BC> for CoreCtx<'_, BC, L>
where
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>,
{
    type ThisStackIpTransportAndDemuxCtx<'a> =
        CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv4>>;
    type SingleStackIpTransportAndDemuxCtx<'a> =
        CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv4>>;

    type DualStackIpTransportAndDemuxCtx<'a> =
        UninstantiableWrapper<CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv4>>>;

    type SingleStackConverter = ();
    type DualStackConverter = Uninstantiable;

    fn with_all_sockets_mut<O, F: FnOnce(&mut TcpSocketSet<Ipv4, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut all_sockets = self.write_lock::<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>();
        cb(&mut *all_sockets)
    }

    fn for_each_socket<
        F: FnMut(
            &TcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
            &TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        mut cb: F,
    ) {
        let (all_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>();
        all_sockets.keys().for_each(|id| {
            let mut locked = locked.adopt(id);
            let guard = locked
                .read_lock_with::<crate::lock_ordering::TcpSocketState<Ipv4>, _>(|c| c.right());
            cb(id, &*guard);
        });
    }

    fn with_socket_mut_isn_transport_demux<
        O,
        F: for<'a> FnOnce(
            MaybeDualStack<
                (&'a mut Self::DualStackIpTransportAndDemuxCtx<'a>, Self::DualStackConverter),
                (&'a mut Self::SingleStackIpTransportAndDemuxCtx<'a>, Self::SingleStackConverter),
            >,
            &mut TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
            &IsnGenerator<BC::Instant>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let isn = self.unlocked_access::<crate::lock_ordering::TcpIsnGenerator<Ipv4>>();
        let mut locked = self.adopt(id);
        let (mut socket_state, mut restricted) = locked
            .write_lock_with_and::<crate::lock_ordering::TcpSocketState<Ipv4>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        let maybe_dual_stack = MaybeDualStack::NotDualStack((&mut restricted, ()));
        cb(maybe_dual_stack, &mut socket_state, isn)
    }

    fn with_socket_and_converter<
        O,
        F: FnOnce(
            &TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        // Acquire socket lock at the current level.
        let mut locked = self.adopt(id);
        let socket_state =
            locked.read_lock_with::<crate::lock_ordering::TcpSocketState<Ipv4>, _>(|c| c.right());
        cb(&socket_state, MaybeDualStack::NotDualStack(()))
    }
}

impl<L, BC> tcp::socket::TcpContext<Ipv6, BC> for CoreCtx<'_, BC, L>
where
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv6>>,
{
    type ThisStackIpTransportAndDemuxCtx<'a> =
        CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv6>>;
    type SingleStackIpTransportAndDemuxCtx<'a> =
        UninstantiableWrapper<CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv6>>>;

    type DualStackIpTransportAndDemuxCtx<'a> =
        CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv6>>;

    type SingleStackConverter = Uninstantiable;
    type DualStackConverter = ();

    fn with_all_sockets_mut<O, F: FnOnce(&mut TcpSocketSet<Ipv6, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut all_sockets = self.write_lock::<crate::lock_ordering::TcpAllSocketsSet<Ipv6>>();
        cb(&mut *all_sockets)
    }

    fn for_each_socket<
        F: FnMut(
            &TcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
            &TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        mut cb: F,
    ) {
        let (all_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::TcpAllSocketsSet<Ipv6>>();
        all_sockets.keys().for_each(|id| {
            let mut locked = locked.adopt(id);
            let guard = locked
                .read_lock_with::<crate::lock_ordering::TcpSocketState<Ipv6>, _>(|c| c.right());
            cb(id, &*guard);
        });
    }

    fn with_socket_mut_isn_transport_demux<
        O,
        F: for<'a> FnOnce(
            MaybeDualStack<
                (&'a mut Self::DualStackIpTransportAndDemuxCtx<'a>, Self::DualStackConverter),
                (&'a mut Self::SingleStackIpTransportAndDemuxCtx<'a>, Self::SingleStackConverter),
            >,
            &mut TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
            &IsnGenerator<BC::Instant>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let isn = self.unlocked_access::<crate::lock_ordering::TcpIsnGenerator<Ipv6>>();
        let mut locked = self.adopt(id);
        let (mut socket_state, mut restricted) = locked
            .write_lock_with_and::<crate::lock_ordering::TcpSocketState<Ipv6>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        let maybe_dual_stack = MaybeDualStack::DualStack((&mut restricted, ()));
        cb(maybe_dual_stack, &mut socket_state, isn)
    }

    fn with_socket_and_converter<
        O,
        F: FnOnce(
            &TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        // Acquire socket lock at the current level.
        let mut locked = self.adopt(id);
        let socket_state =
            locked.read_lock_with::<crate::lock_ordering::TcpSocketState<Ipv6>, _>(|c| c.right());
        cb(&socket_state, MaybeDualStack::DualStack(()))
    }
}

impl<L: LockBefore<crate::lock_ordering::TcpDemux<Ipv4>>, BC: BindingsContext>
    tcp::socket::TcpDualStackContext<Ipv6, WeakDeviceId<BC>, BC> for CoreCtx<'_, BC, L>
{
    type Converter = tcp::socket::Ipv6SocketIdToIpv4DemuxIdConverter;
    type DualStackIpTransportCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::TcpDemux<Ipv6>>;
    fn other_demux_id_converter(&self) -> Self::Converter {
        tcp::socket::Ipv6SocketIdToIpv4DemuxIdConverter
    }

    fn dual_stack_enabled(&self, ip_options: &tcp::socket::Ipv6Options) -> bool {
        ip_options.dual_stack_enabled
    }

    fn set_dual_stack_enabled(&self, ip_options: &mut tcp::socket::Ipv6Options, value: bool) {
        ip_options.dual_stack_enabled = value;
    }

    fn with_both_demux_mut<
        O,
        F: FnOnce(
            &mut tcp::socket::DemuxState<Ipv6, WeakDeviceId<BC>, BC>,
            &mut tcp::socket::DemuxState<Ipv4, WeakDeviceId<BC>, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut demux_v4, mut locked) =
            self.write_lock_and::<crate::lock_ordering::TcpDemux<Ipv4>>();
        let mut demux_v6 = locked.write_lock::<crate::lock_ordering::TcpDemux<Ipv6>>();
        cb(&mut demux_v6, &mut demux_v4)
    }

    fn with_both_demux_mut_and_ip_transport_ctx<
        O,
        F: FnOnce(
            &mut tcp::socket::DemuxState<Ipv6, WeakDeviceId<BC>, BC>,
            &mut tcp::socket::DemuxState<Ipv4, WeakDeviceId<BC>, BC>,
            &mut Self::DualStackIpTransportCtx<'_>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut demux_v4, mut locked) =
            self.write_lock_and::<crate::lock_ordering::TcpDemux<Ipv4>>();
        let (mut demux_v6, mut locked) =
            locked.write_lock_and::<crate::lock_ordering::TcpDemux<Ipv6>>();
        cb(&mut demux_v6, &mut demux_v4, &mut locked)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: Ip, BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpAllSocketsSet<I>>>
    udp::StateContext<I, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpSocketState<I>>;

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked::<crate::lock_ordering::UdpSocketState<I>>())
    }

    fn with_all_sockets_mut<O, F: FnOnce(&mut UdpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.write_lock::<crate::lock_ordering::UdpAllSocketsSet<I>>())
    }

    fn with_all_sockets<O, F: FnOnce(&UdpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::UdpAllSocketsSet<I>>())
    }

    fn with_socket_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &UdpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &UdpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id);
        let (socket_state, mut restricted) =
            locked.read_lock_with_and::<crate::lock_ordering::UdpSocketState<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &socket_state)
    }

    fn with_socket_state_mut<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut UdpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &UdpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id);
        let (mut socket_state, mut restricted) =
            locked.write_lock_with_and::<crate::lock_ordering::UdpSocketState<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &mut socket_state)
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        self.cast_with(|s| {
            let IpInvariant(send_port_unreachable) = I::map_ip(
                (),
                |()| IpInvariant(&s.transport.udpv4.send_port_unreachable),
                |()| IpInvariant(&s.transport.udpv6.send_port_unreachable),
            );
            send_port_unreachable
        })
        .copied()
    }

    fn for_each_socket<
        F: FnMut(
            &mut Self::SocketStateCtx<'_>,
            &UdpSocketId<I, Self::WeakDeviceId, BC>,
            &UdpSocketState<I, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        mut cb: F,
    ) {
        let (all_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::UdpAllSocketsSet<I>>();
        all_sockets.keys().for_each(|id| {
            let id = UdpSocketId::from(id.clone());
            let mut locked = locked.adopt(&id);
            let (socket_state, mut restricted) = locked
                .read_lock_with_and::<crate::lock_ordering::UdpSocketState<I>, _>(|c| c.right());
            let mut restricted = restricted.cast_core_ctx();
            cb(&mut restricted, &id, &socket_state);
        });
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::BoundStateContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpBoundMap<Ipv4>>;
    type DualStackContext = UninstantiableWrapper<Self>;
    type NonDualStackContext = Self;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::BoundSockets<Ipv4, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (bound_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv4>>();
        cb(&mut locked, &bound_sockets)
    }

    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut udp::BoundSockets<Ipv4, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut bound_sockets, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv4>>();
        cb(&mut locked, &mut bound_sockets)
    }

    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        MaybeDualStack::NotDualStack(self)
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::BoundStateContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpBoundMap<Ipv6>>;
    type DualStackContext = Self;
    type NonDualStackContext = UninstantiableWrapper<Self>;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::BoundSockets<Ipv6, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (bound_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv6>>();
        cb(&mut locked, &bound_sockets)
    }

    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut udp::BoundSockets<Ipv6, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut bound_sockets, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv6>>();
        cb(&mut locked, &mut bound_sockets)
    }

    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        MaybeDualStack::DualStack(self)
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked::<crate::lock_ordering::UdpBoundMap<Ipv6>>())
    }
}

impl<L, BC: BindingsContext> udp::UdpStateContext for CoreCtx<'_, BC, L> {}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::DualStackBoundStateContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpBoundMap<Ipv6>>;

    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut udp::BoundSockets<Ipv6, Self::WeakDeviceId, BC>,
            &mut udp::BoundSockets<Ipv4, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut bound_v4, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv4>>();
        let (mut bound_v6, mut locked) =
            locked.write_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv6>>();
        cb(&mut locked, &mut bound_v6, &mut bound_v4)
    }

    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut udp::BoundSockets<Ipv4, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut bound_v4, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv4>>();
        cb(&mut locked.cast_locked(), &mut bound_v4)
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked::<crate::lock_ordering::UdpBoundMap<Ipv6>>())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::NonDualStackBoundStateContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
}

impl<I: crate::transport::tcp::socket::DualStackIpExt, BC: BindingsContext>
    RwLockFor<crate::lock_ordering::TcpAllSocketsSet<I>> for StackState<BC>
{
    type Data = tcp::socket::TcpSocketSet<I, WeakDeviceId<BC>, BC>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
        where
            Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.transport.tcp_state::<I>().sockets.all_sockets.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.transport.tcp_state::<I>().sockets.all_sockets.write()
    }
}

impl<I: crate::transport::tcp::socket::DualStackIpExt, BC: BindingsContext>
    RwLockFor<crate::lock_ordering::TcpDemux<I>> for StackState<BC>
{
    type Data = tcp::socket::DemuxState<I, WeakDeviceId<BC>, BC>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
        where
            Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.transport.tcp_state::<I>().sockets.demux.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.transport.tcp_state::<I>().sockets.demux.write()
    }
}

impl<I: crate::transport::tcp::socket::DualStackIpExt, BC: BindingsContext>
    UnlockedAccess<crate::lock_ordering::TcpIsnGenerator<I>> for StackState<BC>
{
    type Data = IsnGenerator<BC::Instant>;
    type Guard<'l> = &'l IsnGenerator<BC::Instant> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.transport.tcp_state::<I>().isn_generator
    }
}

impl<I: datagram::IpExt, D: device::WeakId, BT: BindingsTypes>
    RwLockFor<crate::lock_ordering::UdpSocketState<I>> for UdpSocketId<I, D, BT>
{
    type Data = UdpSocketState<I, D, BT>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
    where
        Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
    where
        Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state_for_locking().read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state_for_locking().write()
    }
}

impl<I: datagram::IpExt, BT: BindingsTypes> RwLockFor<crate::lock_ordering::UdpBoundMap<I>>
    for StackState<BT>
{
    type Data = udp::BoundSockets<I, WeakDeviceId<BT>, BT>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.transport.udp_state::<I>().sockets.bound.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.transport.udp_state::<I>().sockets.bound.write()
    }
}

impl<I: datagram::IpExt, BT: BindingsTypes> RwLockFor<crate::lock_ordering::UdpAllSocketsSet<I>>
    for StackState<BT>
{
    type Data = UdpSocketSet<I, WeakDeviceId<BT>, BT>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.transport.udp_state::<I>().sockets.all_sockets.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.transport.udp_state::<I>().sockets.all_sockets.write()
    }
}

impl<BC: crate::BindingsContext, I: Ip> UnlockedAccess<crate::lock_ordering::TcpCounters<I>>
    for StackState<BC>
{
    type Data = TcpCounters<I>;
    type Guard<'l> = &'l TcpCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.tcp_counters()
    }
}

impl<BC: crate::BindingsContext, I: Ip, L> CounterContext<TcpCounters<I>> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&TcpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::TcpCounters<I>>())
    }
}
