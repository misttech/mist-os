// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    Locked,
};
use net_types::ip::{Ipv4, Ipv6};
use packet::BufferMut;

use crate::{
    device::WeakDeviceId,
    socket::datagram::{MaybeDualStack, UninstantiableContext},
    transport::{
        tcp::{
            self,
            socket::{
                isn::IsnGenerator, TcpSocketId, TcpSocketSet, TcpSocketState, WeakTcpSocketId,
            },
        },
        udp::{self},
    },
    NonSyncContext, SyncCtx,
};

impl<I, L, C> tcp::socket::DemuxSyncContext<I, WeakDeviceId<C>, C> for Locked<&SyncCtx<C>, L>
where
    I: crate::ip::IpExt + crate::ip::device::IpDeviceIpExt,
    C: NonSyncContext,
    L: LockBefore<crate::lock_ordering::TcpDemux<I>>,
{
    fn with_demux<O, F: FnOnce(&tcp::socket::DemuxState<I, WeakDeviceId<C>, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::TcpDemux<I>>())
    }

    fn with_demux_mut<O, F: FnOnce(&mut tcp::socket::DemuxState<I, WeakDeviceId<C>, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.write_lock::<crate::lock_ordering::TcpDemux<I>>())
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I, L, C> tcp::socket::SyncContext<I, C> for Locked<&SyncCtx<C>, L>
where
    I: crate::ip::IpExt + crate::ip::device::IpDeviceIpExt + crate::ip::IpLayerIpExt,
    C: NonSyncContext,
    L: LockBefore<crate::lock_ordering::TcpAllSocketsSet<I>>,
{
    type IpTransportAndDemuxCtx<'a> =
        Locked<&'a SyncCtx<C>, crate::lock_ordering::TcpSocketState<I>>;

    fn with_all_sockets_mut<O, F: FnOnce(&mut TcpSocketSet<I, Self::WeakDeviceId, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut all_sockets = self.write_lock::<crate::lock_ordering::TcpAllSocketsSet<I>>();
        cb(&mut *all_sockets)
    }

    fn socket_destruction_deferred(&mut self, _socket: WeakTcpSocketId<I, Self::WeakDeviceId, C>) {
        // Do nothing, we use this function to assert on deferred destruction.
    }

    fn for_each_socket<F: FnMut(&TcpSocketState<I, Self::WeakDeviceId, C>)>(&mut self, mut cb: F) {
        let (all_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::TcpAllSocketsSet<I>>();
        all_sockets.keys().for_each(|id| {
            let mut locked = locked.adopt(id);
            let guard = locked
                .read_lock_with::<crate::lock_ordering::TcpSocketState<I>, TcpSocketId<_, _, _>>(
                    |(_ctx, sock)| sock,
                );
            cb(&*guard);
        });
    }

    fn with_socket_mut_isn_transport_demux<
        O,
        F: FnOnce(
            &mut Self::IpTransportAndDemuxCtx<'_>,
            &mut TcpSocketState<I, Self::WeakDeviceId, C>,
            &IsnGenerator<C::Instant>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, C>,
        cb: F,
    ) -> O {
        let isn = self.unlocked_access::<crate::lock_ordering::TcpIsnGenerator<I>>();
        let mut locked = self.adopt(id);
        let (mut socket_state, mut restricted) = locked
            .write_lock_with_and::<crate::lock_ordering::TcpSocketState<I>, TcpSocketId<_, _, _>>(
                |(_ctx, id)| id,
            );
        let mut restricted = restricted.cast_with::<SyncCtx<_>>(|(ctx, _id)| ctx);
        cb(&mut restricted, &mut socket_state, isn)
    }

    fn with_socket<O, F: FnOnce(&TcpSocketState<I, Self::WeakDeviceId, C>) -> O>(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, C>,
        cb: F,
    ) -> O {
        // Acquire socket lock at the current level.
        let mut locked = self.adopt(id);
        let socket_state = locked
            .read_lock_with::<crate::lock_ordering::TcpSocketState<I>, TcpSocketId<_, _, _>>(
                |(_ctx, id)| id,
            );
        cb(&socket_state)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv4>>>
    udp::StateContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
    type SocketStateCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpSocketsTable<Ipv4>>;

    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &udp::SocketsState<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (socket_state, mut locked) =
            self.read_lock_and::<crate::lock_ordering::UdpSocketsTable<Ipv4>>();
        cb(&mut locked, &socket_state)
    }

    fn with_sockets_state_mut<
        O,
        F: FnOnce(
            &mut Self::SocketStateCtx<'_>,
            &mut udp::SocketsState<Ipv4, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut socket_state, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpSocketsTable<Ipv4>>();
        cb(&mut locked, &mut socket_state)
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        self.cast_with(|s| &s.state.transport.udpv4.send_port_unreachable).copied()
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::BoundStateContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
    type IpSocketsCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpBoundMap<Ipv4>>;
    type DualStackContext = UninstantiableContext<Ipv4, udp::Udp, Self>;
    type NonDualStackContext = Self;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::BoundSockets<Ipv4, Self::WeakDeviceId>) -> O,
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
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::BoundSockets<Ipv4, Self::WeakDeviceId>) -> O,
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

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv6>>>
    udp::StateContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    type SocketStateCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpSocketsTable<Ipv6>>;

    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &udp::SocketsState<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (socket_state, mut locked) =
            self.read_lock_and::<crate::lock_ordering::UdpSocketsTable<Ipv6>>();
        cb(&mut locked, &socket_state)
    }

    fn with_sockets_state_mut<
        O,
        F: FnOnce(
            &mut Self::SocketStateCtx<'_>,
            &mut udp::SocketsState<Ipv6, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut socket_state, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpSocketsTable<Ipv6>>();
        cb(&mut locked, &mut socket_state)
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        self.cast_with(|s| &s.state.transport.udpv6.send_port_unreachable).copied()
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::BoundStateContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    type IpSocketsCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpBoundMap<Ipv6>>;
    type DualStackContext = Self;
    type NonDualStackContext = UninstantiableContext<Ipv6, udp::Udp, Self>;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::BoundSockets<Ipv6, Self::WeakDeviceId>) -> O,
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
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::BoundSockets<Ipv6, Self::WeakDeviceId>) -> O,
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

impl<L, C: NonSyncContext> udp::UdpStateContext for Locked<&SyncCtx<C>, L> {}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::DualStackBoundStateContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    type IpSocketsCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpBoundMap<Ipv6>>;
    type OtherBufferIpSocketsCtx<'a, B: BufferMut> =
        Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpBoundMap<Ipv4>>;

    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut udp::BoundSockets<Ipv6, Self::WeakDeviceId>,
            &mut udp::BoundSockets<Ipv4, Self::WeakDeviceId>,
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
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::BoundSockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut bound_v4, mut locked) =
            self.write_lock_and::<crate::lock_ordering::UdpBoundMap<Ipv4>>();
        cb(&mut locked.cast_locked(), &mut bound_v4)
    }

    fn with_other_transport_context_buf<
        O,
        B: BufferMut,
        F: FnOnce(&mut Self::OtherBufferIpSocketsCtx<'_, B>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked::<crate::lock_ordering::UdpBoundMap<Ipv4>>())
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::NonDualStackBoundStateContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
}

impl<I: crate::ip::IpExt, C: NonSyncContext> RwLockFor<crate::lock_ordering::TcpAllSocketsSet<I>>
    for SyncCtx<C>
{
    type Data = tcp::socket::TcpSocketSet<I, WeakDeviceId<C>, C>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
        where
            Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.transport.tcp_state::<I>().sockets.all_sockets.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.transport.tcp_state::<I>().sockets.all_sockets.write()
    }
}

impl<I: crate::ip::IpExt, C: NonSyncContext> RwLockFor<crate::lock_ordering::TcpDemux<I>>
    for SyncCtx<C>
{
    type Data = tcp::socket::DemuxState<I, WeakDeviceId<C>, C>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
        where
            Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.transport.tcp_state::<I>().sockets.demux.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.transport.tcp_state::<I>().sockets.demux.write()
    }
}

impl<I: crate::ip::IpExt, C: NonSyncContext>
    UnlockedAccess<crate::lock_ordering::TcpIsnGenerator<I>> for SyncCtx<C>
{
    type Data = IsnGenerator<C::Instant>;
    type Guard<'l> = &'l IsnGenerator<C::Instant> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.state.transport.tcp_state::<I>().isn_generator
    }
}
