// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{
    lock::{LockFor, UnlockedAccess},
    relation::LockBefore,
    Locked, Unlocked,
};
use net_types::ip::{Ipv4, Ipv6};
use packet::BufferMut;

use crate::{
    device::WeakDeviceId,
    ip::{device::IpDeviceNonSyncContext, BufferTransportIpContext, IpExt},
    transport::{
        tcp::{self, socket::isn::IsnGenerator, TcpState},
        udp,
    },
    NonSyncContext, SyncCtx,
};

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<C: NonSyncContext + IpDeviceNonSyncContext<Ipv4, Self::DeviceId>>
    tcp::socket::SyncContext<Ipv4, C> for &'_ SyncCtx<C>
{
    type IpTransportCtx<'a> =
        <Locked<'a, SyncCtx<C>, Unlocked> as tcp::socket::SyncContext<Ipv4, C>>::IpTransportCtx<'a>;

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
        tcp::socket::SyncContext::<Ipv4, _>::with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut(
            &mut Locked::new(*self),
            cb,
        )
    }

    fn with_tcp_sockets<O, F: FnOnce(&tcp::socket::Sockets<Ipv4, Self::WeakDeviceId, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        tcp::socket::SyncContext::<Ipv4, _>::with_tcp_sockets(&mut Locked::new(*self), cb)
    }
}

impl<
        C: NonSyncContext + IpDeviceNonSyncContext<Ipv4, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::TcpSockets<Ipv4>>,
    > tcp::socket::SyncContext<Ipv4, C> for Locked<'_, SyncCtx<C>, L>
{
    type IpTransportCtx<'a> = Locked<'a, SyncCtx<C>, crate::lock_ordering::TcpSockets<Ipv4>>;

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

// TODO(https://fxbug.dev/121450): Remove this when it is unused.
impl<C: NonSyncContext + IpDeviceNonSyncContext<Ipv6, Self::DeviceId>>
    tcp::socket::SyncContext<Ipv6, C> for &'_ SyncCtx<C>
{
    type IpTransportCtx<'a> =
        <Locked<'a, SyncCtx<C>, Unlocked> as tcp::socket::SyncContext<Ipv6, C>>::IpTransportCtx<'a>;

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
        tcp::socket::SyncContext::<Ipv6, _>::with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut(
            &mut Locked::new(*self),
            cb,
        )
    }

    fn with_tcp_sockets<O, F: FnOnce(&tcp::socket::Sockets<Ipv6, Self::WeakDeviceId, C>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        tcp::socket::SyncContext::<Ipv6, _>::with_tcp_sockets(&mut Locked::new(*self), cb)
    }
}

impl<
        C: NonSyncContext + IpDeviceNonSyncContext<Ipv6, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::TcpSockets<Ipv6>>,
    > tcp::socket::SyncContext<Ipv6, C> for Locked<'_, SyncCtx<C>, L>
{
    type IpTransportCtx<'a> = Locked<'a, SyncCtx<C>, crate::lock_ordering::TcpSockets<Ipv6>>;

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

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<C: NonSyncContext> udp::StateContext<Ipv4, C> for &'_ SyncCtx<C> {
    type IpSocketsCtx<'a> =
        <Locked<'a, SyncCtx<C>, Unlocked> as udp::StateContext<Ipv4, C>>::IpSocketsCtx<'a>;

    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::Sockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        udp::StateContext::<Ipv4, _>::with_sockets(&mut Locked::new(*self), cb)
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::Sockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        udp::StateContext::<Ipv4, _>::with_sockets_mut(&mut Locked::new(*self), cb)
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        udp::StateContext::<Ipv4, _>::should_send_port_unreachable(&mut Locked::new(*self))
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpSockets<Ipv4>>>
    udp::StateContext<Ipv4, C> for Locked<'_, SyncCtx<C>, L>
{
    type IpSocketsCtx<'a> = Locked<'a, SyncCtx<C>, crate::lock_ordering::UdpSockets<Ipv4>>;

    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::Sockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (udp, mut locked) = self.read_lock_and::<crate::lock_ordering::UdpSockets<Ipv4>>();
        cb(&mut locked, &udp)
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::Sockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut udp, mut locked) = self.write_lock_and::<crate::lock_ordering::UdpSockets<Ipv4>>();
        cb(&mut locked, &mut udp)
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        self.cast_with(|s| &s.state.transport.udpv4.send_port_unreachable).copied()
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<C: NonSyncContext> udp::StateContext<Ipv6, C> for &'_ SyncCtx<C> {
    type IpSocketsCtx<'a> =
        <Locked<'a, SyncCtx<C>, Unlocked> as udp::StateContext<Ipv6, C>>::IpSocketsCtx<'a>;

    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::Sockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        udp::StateContext::<Ipv6, _>::with_sockets(&mut Locked::new(*self), cb)
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::Sockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        udp::StateContext::<Ipv6, _>::with_sockets_mut(&mut Locked::new(*self), cb)
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        udp::StateContext::<Ipv6, _>::should_send_port_unreachable(&mut Locked::new(*self))
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::UdpSockets<Ipv6>>>
    udp::StateContext<Ipv6, C> for Locked<'_, SyncCtx<C>, L>
{
    type IpSocketsCtx<'a> = Locked<'a, SyncCtx<C>, crate::lock_ordering::UdpSockets<Ipv6>>;

    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &udp::Sockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (udp, mut locked) = self.read_lock_and::<crate::lock_ordering::UdpSockets<Ipv6>>();
        cb(&mut locked, &udp)
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut udp::Sockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut udp, mut locked) = self.write_lock_and::<crate::lock_ordering::UdpSockets<Ipv6>>();
        cb(&mut locked, &mut udp)
    }

    fn should_send_port_unreachable(&mut self) -> bool {
        self.cast_with(|s| &s.state.transport.udpv4.send_port_unreachable).copied()
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<I: IpExt, B: BufferMut, C: udp::BufferNonSyncContext<I, B> + crate::NonSyncContext>
    udp::BufferStateContext<I, C, B> for &'_ SyncCtx<C>
where
    Self: udp::StateContext<I, C>,
    for<'a> Self::IpSocketsCtx<'a>: BufferTransportIpContext<I, C, B>,
{
    type BufferIpSocketsCtx<'a> = <Self as udp::StateContext<I, C>>::IpSocketsCtx<'a>;
}

impl<
        I: IpExt,
        B: BufferMut,
        C: udp::BufferNonSyncContext<I, B> + crate::NonSyncContext,
        L: LockBefore<crate::lock_ordering::UdpSockets<I>>,
    > udp::BufferStateContext<I, C, B> for Locked<'_, SyncCtx<C>, L>
where
    Self: udp::StateContext<I, C>,
    for<'a> Self::IpSocketsCtx<'a>: BufferTransportIpContext<I, C, B>,
{
    type BufferIpSocketsCtx<'a> = <Self as udp::StateContext<I, C>>::IpSocketsCtx<'a>;
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::TcpSockets<Ipv4>> for SyncCtx<C> {
    type Data<'l> = crate::sync::LockGuard<'l, tcp::socket::Sockets<Ipv4, WeakDeviceId<C::Instant>, C>>
        where Self: 'l;

    fn lock(&self) -> Self::Data<'_> {
        self.state.transport.tcpv4.sockets.lock()
    }
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::TcpSockets<Ipv6>> for SyncCtx<C> {
    type Data<'l> = crate::sync::LockGuard<'l, tcp::socket::Sockets<Ipv6, WeakDeviceId<C::Instant>, C>>
        where Self: 'l;

    fn lock(&self) -> Self::Data<'_> {
        self.state.transport.tcpv6.sockets.lock()
    }
}

impl<C: NonSyncContext> UnlockedAccess<crate::lock_ordering::TcpIsnGenerator<Ipv4>> for SyncCtx<C> {
    type Data<'l> = &'l IsnGenerator<C::Instant> where Self: 'l;

    fn access(&self) -> Self::Data<'_> {
        let TcpState { isn_generator, sockets: _ } = &self.state.transport.tcpv4;
        isn_generator
    }
}

impl<C: NonSyncContext> UnlockedAccess<crate::lock_ordering::TcpIsnGenerator<Ipv6>> for SyncCtx<C> {
    type Data<'l> = &'l IsnGenerator<C::Instant> where Self: 'l;

    fn access(&self) -> Self::Data<'_> {
        let TcpState { isn_generator, sockets: _ } = &self.state.transport.tcpv6;
        isn_generator
    }
}
