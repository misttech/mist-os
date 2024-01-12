// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    wrap::prelude::*,
};
use net_types::ip::{Ipv4, Ipv6};

use crate::{
    device::{self, WeakDeviceId},
    socket::{EitherStack, MaybeDualStack},
    transport::{
        tcp::{
            self,
            socket::{
                isn::IsnGenerator, TcpSocketId, TcpSocketSet, TcpSocketState, WeakTcpSocketId,
            },
        },
        udp::{self},
    },
    uninstantiable::{Uninstantiable, UninstantiableWrapper},
    BindingsContext, CoreCtx, StackState,
};

impl<I, L, BC> tcp::socket::TcpDemuxContext<I, WeakDeviceId<BC>, BC> for CoreCtx<'_, BC, L>
where
    I: crate::ip::IpExt
        + crate::ip::device::IpDeviceIpExt
        + crate::transport::tcp::socket::DualStackIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::TcpDemux<I>>,
{
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

    fn socket_destruction_deferred(
        &mut self,
        _socket: WeakTcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
    ) {
        // Do nothing, we use this function to assert on deferred destruction.
    }

    fn for_each_socket<
        F: FnMut(
            &TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
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
            cb(&*guard, MaybeDualStack::NotDualStack(()));
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
    // TODO(https://fxbug.dev/42085913): Use `UninstantiableWrapper<Self>` as
    // the single stack ctx once the `AsSingleStack` bound has been dropped
    // from [`TcpSyncCtx::DualStackIpTransportAndDemuxCtx`] (It's not
    // possible for `Self` to implement
    // `AsSingleStack<UninstantiableWrapper<Self>>`).
    type SingleStackIpTransportAndDemuxCtx<'a> =
        CoreCtx<'a, BC, crate::lock_ordering::TcpSocketState<Ipv6>>;

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

    fn socket_destruction_deferred(
        &mut self,
        _socket: WeakTcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
    ) {
        // Do nothing, we use this function to assert on deferred destruction.
    }

    fn for_each_socket<
        F: FnMut(
            &TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
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
            cb(&*guard, MaybeDualStack::DualStack(()));
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

impl<L, BC: BindingsContext> tcp::socket::TcpDualStackContext<Ipv6> for CoreCtx<'_, BC, L> {
    fn into_other_demux_socket_id<D: device::WeakId, BT: tcp::socket::TcpBindingsTypes>(
        &self,
        id: tcp::socket::TcpSocketId<Ipv6, D, BT>,
    ) -> <Ipv4 as tcp::socket::DualStackIpExt>::DemuxSocketId<D, BT> {
        EitherStack::OtherStack(id)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv4>>>
    udp::StateContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpSocketsTable<Ipv4>>;

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
        self.cast_with(|s| &s.transport.udpv4.send_port_unreachable).copied()
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

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv6>>>
    udp::StateContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpSocketsTable<Ipv6>>;

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
        self.cast_with(|s| &s.transport.udpv6.send_port_unreachable).copied()
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

impl<L, BC: BindingsContext> udp::UdpStateContext for CoreCtx<'_, BC, L> {}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv4>>>
    udp::DualStackBoundStateContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::UdpBoundMap<Ipv6>>;

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
