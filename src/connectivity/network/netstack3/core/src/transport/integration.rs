// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use net_types::ip::{Ipv4, Ipv6};

use crate::{
    ip::device::IpDeviceNonSyncContext,
    transport::{
        tcp::{self, socket::isn::IsnGenerator, TcpState},
        udp,
    },
    NonSyncContext, SyncCtx,
};

impl<C: NonSyncContext + IpDeviceNonSyncContext<Ipv4, Self::DeviceId>>
    tcp::socket::SyncContext<Ipv4, C> for &'_ SyncCtx<C>
{
    type IpTransportCtx = Self;

    fn with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut<
        O,
        F: FnOnce(
            &mut Self,
            &IsnGenerator<C::Instant>,
            &mut tcp::socket::Sockets<Ipv4, Self::WeakDeviceId, C>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let mut s = *self;
        let TcpState { isn_generator, sockets } = &s.state.transport.tcpv4;
        cb(&mut s, isn_generator, &mut sockets.lock())
    }

    fn with_tcp_sockets<O, F: FnOnce(&tcp::socket::Sockets<Ipv4, Self::WeakDeviceId, C>) -> O>(
        &self,
        cb: F,
    ) -> O {
        let TcpState { sockets, isn_generator: _ } = &self.state.transport.tcpv4;
        cb(&sockets.lock())
    }
}

impl<C: NonSyncContext + IpDeviceNonSyncContext<Ipv6, Self::DeviceId>>
    tcp::socket::SyncContext<Ipv6, C> for &'_ SyncCtx<C>
{
    type IpTransportCtx = Self;

    fn with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut<
        O,
        F: FnOnce(
            &mut Self,
            &IsnGenerator<C::Instant>,
            &mut tcp::socket::Sockets<Ipv6, Self::WeakDeviceId, C>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let mut s = *self;
        let TcpState { isn_generator, sockets } = &s.state.transport.tcpv6;
        cb(&mut s, isn_generator, &mut sockets.lock())
    }

    fn with_tcp_sockets<O, F: FnOnce(&tcp::socket::Sockets<Ipv6, Self::WeakDeviceId, C>) -> O>(
        &self,
        cb: F,
    ) -> O {
        let TcpState { sockets, isn_generator: _ } = &self.state.transport.tcpv6;
        cb(&sockets.lock())
    }
}

impl<C: NonSyncContext> udp::StateContext<Ipv4, C> for &'_ SyncCtx<C> {
    type IpSocketsCtx = Self;

    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx, &udp::Sockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(self, &self.state.transport.udpv4.sockets.read())
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx, &mut udp::Sockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(self, &mut self.state.transport.udpv4.sockets.write())
    }

    fn should_send_port_unreachable(&self) -> bool {
        self.state.transport.udpv4.send_port_unreachable
    }
}

impl<C: NonSyncContext> udp::StateContext<Ipv6, C> for &'_ SyncCtx<C> {
    type IpSocketsCtx = Self;

    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx, &udp::Sockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(self, &self.state.transport.udpv6.sockets.read())
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx, &mut udp::Sockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(self, &mut self.state.transport.udpv6.sockets.write())
    }

    fn should_send_port_unreachable(&self) -> bool {
        self.state.transport.udpv6.send_port_unreachable
    }
}
