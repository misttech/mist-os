// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{
    lock::{LockFor, UnlockedAccess},
    relation::LockBefore,
    Locked,
};
use net_types::ip::{Ipv4, Ipv6};
use packet::BufferMut;

use crate::{
    device::WeakDeviceId,
    ip::{device::IpDeviceNonSyncContext, BufferTransportIpContext},
    transport::{
        tcp::{self, socket::isn::IsnGenerator, TcpState},
        udp,
    },
    NonSyncContext, SyncCtx,
};

impl<
        C: NonSyncContext + IpDeviceNonSyncContext<Ipv4, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::TcpSockets<Ipv4>>,
    > tcp::socket::SyncContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
    type IpTransportCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::TcpSockets<Ipv4>>;

    fn with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpTransportCtx<'_>,
            &IsnGenerator<C::Instant>,
            &mut tcp::socket::Sockets<Ipv4, Self::WeakDeviceId, C>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let isn_generator = self.unlocked_access::<crate::lock_ordering::TcpIsnGenerator<Ipv4>>();
        let (mut sockets, mut locked) = self.lock_and::<crate::lock_ordering::TcpSockets<Ipv4>>();
        cb(&mut locked, isn_generator, &mut *sockets)
    }

    fn with_tcp_sockets<O, F: FnOnce(&tcp::socket::Sockets<Ipv4, Self::WeakDeviceId, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let sockets = self.lock::<crate::lock_ordering::TcpSockets<Ipv4>>();
        cb(&*sockets)
    }
}

impl<
        C: NonSyncContext + IpDeviceNonSyncContext<Ipv6, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::TcpSockets<Ipv6>>,
    > tcp::socket::SyncContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    type IpTransportCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::TcpSockets<Ipv6>>;

    fn with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpTransportCtx<'_>,
            &IsnGenerator<C::Instant>,
            &mut tcp::socket::Sockets<Ipv6, Self::WeakDeviceId, C>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let isn_generator = self.unlocked_access::<crate::lock_ordering::TcpIsnGenerator<Ipv6>>();
        let (mut sockets, mut locked) = self.lock_and::<crate::lock_ordering::TcpSockets<Ipv6>>();
        cb(&mut locked, isn_generator, &mut sockets)
    }

    fn with_tcp_sockets<O, F: FnOnce(&tcp::socket::Sockets<Ipv6, Self::WeakDeviceId, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let sockets = self.lock::<crate::lock_ordering::TcpSockets<Ipv6>>();
        cb(&*sockets)
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

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpBoundMap<Ipv6>>>
    udp::BoundStateContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    type IpSocketsCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::UdpBoundMap<Ipv6>>;

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

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }
}

impl<
        I: udp::IpExt,
        B: BufferMut,
        C: udp::BufferNonSyncContext<I, B>
            + udp::BufferNonSyncContext<I::OtherVersion, B>
            + crate::NonSyncContext,
        L,
    > udp::BufferStateContext<I, C, B> for Locked<&SyncCtx<C>, L>
where
    Self: udp::StateContext<I, C>,
    for<'a> Self::SocketStateCtx<'a>: udp::BufferBoundStateContext<I, C, B>,
{
    type BufferSocketStateCtx<'a> = Self::SocketStateCtx<'a>;
}

impl<
        I: udp::IpExt,
        B: BufferMut,
        C: udp::BufferNonSyncContext<I, B>
            + udp::BufferNonSyncContext<I::OtherVersion, B>
            + crate::NonSyncContext,
        L,
    > udp::BufferBoundStateContext<I, C, B> for Locked<&SyncCtx<C>, L>
where
    Self: udp::BoundStateContext<I, C>,
    for<'a> Self::IpSocketsCtx<'a>: BufferTransportIpContext<I, C, B>,
{
    type BufferIpSocketsCtx<'a> = Self::IpSocketsCtx<'a>;
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::TcpSockets<Ipv4>> for SyncCtx<C> {
    type Data = tcp::socket::Sockets<Ipv4, WeakDeviceId<C>, C>;
    type Guard<'l> = crate::sync::LockGuard<'l, tcp::socket::Sockets<Ipv4, WeakDeviceId<C>, C>>
        where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        self.state.transport.tcpv4.sockets.lock()
    }
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::TcpSockets<Ipv6>> for SyncCtx<C> {
    type Data = tcp::socket::Sockets<Ipv6, WeakDeviceId<C>, C>;
    type Guard<'l> = crate::sync::LockGuard<'l, tcp::socket::Sockets<Ipv6, WeakDeviceId<C>, C>>
        where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        self.state.transport.tcpv6.sockets.lock()
    }
}

impl<C: NonSyncContext> UnlockedAccess<crate::lock_ordering::TcpIsnGenerator<Ipv4>> for SyncCtx<C> {
    type Data = IsnGenerator<C::Instant>;
    type Guard<'l> = &'l IsnGenerator<C::Instant> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        let TcpState { isn_generator, sockets: _ } = &self.state.transport.tcpv4;
        isn_generator
    }
}

impl<C: NonSyncContext> UnlockedAccess<crate::lock_ordering::TcpIsnGenerator<Ipv6>> for SyncCtx<C> {
    type Data = IsnGenerator<C::Instant>;
    type Guard<'l> = &'l IsnGenerator<C::Instant> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        let TcpState { isn_generator, sockets: _ } = &self.state.transport.tcpv6;
        isn_generator
    }
}
