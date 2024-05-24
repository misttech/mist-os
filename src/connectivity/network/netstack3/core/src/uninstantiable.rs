// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Groups trait implementations for uninstantiable types.

use explicit::UnreachableExt as _;
use net_types::SpecifiedAddr;
use packet::{BufferMut, Serializer};

use crate::{
    context::CounterContext,
    device::{EitherDeviceId, WeakDeviceIdentifier},
    ip::{
        socket::{DeviceIpSocketHandler, IpSock, IpSocketHandler, Mms, MmsError, SendOptions},
        HopLimits, IpExt, IpLayerIpExt, IpSockCreationError, IpSockSendError, TransportIpContext,
    },
    socket::{
        datagram::{
            self,
            spec_context::{
                DatagramSpecBoundStateContext, DualStackDatagramSpecBoundStateContext,
                NonDualStackDatagramSpecBoundStateContext,
            },
            DatagramBoundStateContext, DatagramSocketMapSpec, DatagramSocketOptions,
            DatagramSocketSpec, DualStackConverter, NonDualStackConverter,
        },
        MaybeDualStack, SocketIpAddr,
    },
    transport::tcp::{
        socket::{self as tcp_socket, TcpBindingsTypes},
        TcpCounters,
    },
};

pub use netstack3_base::{Uninstantiable, UninstantiableWrapper};

impl<I: datagram::IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DatagramSpecBoundStateContext<I, UninstantiableWrapper<P>, C> for S
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;
    type DualStackContext = P::DualStackContext;
    type NonDualStackContext = P::NonDualStackContext;
    fn dual_stack_context(
        core_ctx: &mut UninstantiableWrapper<P>,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        core_ctx.uninstantiable_unreachable()
    }
    fn with_bound_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &datagram::BoundSockets<
                I,
                P::WeakDeviceId,
                <S as DatagramSocketSpec>::AddrSpec,
                <S as DatagramSocketSpec>::SocketMapSpec<I, P::WeakDeviceId>,
            >,
        ) -> O,
    >(
        core_ctx: &mut UninstantiableWrapper<P>,
        _cb: F,
    ) -> O {
        core_ctx.uninstantiable_unreachable()
    }
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut datagram::BoundSockets<
                I,
                P::WeakDeviceId,
                <S as DatagramSocketSpec>::AddrSpec,
                <S as DatagramSocketSpec>::SocketMapSpec<I, P::WeakDeviceId>,
            >,
        ) -> O,
    >(
        core_ctx: &mut UninstantiableWrapper<P>,
        _cb: F,
    ) -> O {
        core_ctx.uninstantiable_unreachable()
    }
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        core_ctx: &mut UninstantiableWrapper<P>,
        _cb: F,
    ) -> O {
        core_ctx.uninstantiable_unreachable()
    }
}

impl<I: datagram::IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    NonDualStackDatagramSpecBoundStateContext<I, UninstantiableWrapper<P>, C> for S
{
    fn nds_converter(
        core_ctx: &UninstantiableWrapper<P>,
    ) -> impl NonDualStackConverter<I, P::WeakDeviceId, Self> {
        core_ctx.uninstantiable_unreachable::<Uninstantiable>()
    }
}

impl<I: datagram::IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DualStackDatagramSpecBoundStateContext<I, UninstantiableWrapper<P>, C> for S
where
    for<'a> P::IpSocketsCtx<'a>: TransportIpContext<I::OtherVersion, C>,
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;

    fn dual_stack_enabled(
        core_ctx: &UninstantiableWrapper<P>,
        _state: &impl AsRef<datagram::IpOptions<I, P::WeakDeviceId, S>>,
    ) -> bool {
        core_ctx.uninstantiable_unreachable()
    }

    fn to_other_socket_options<'a>(
        core_ctx: &UninstantiableWrapper<P>,
        _state: &'a datagram::IpOptions<I, P::WeakDeviceId, S>,
    ) -> &'a DatagramSocketOptions<I::OtherVersion, P::WeakDeviceId> {
        core_ctx.uninstantiable_unreachable()
    }

    fn ds_converter(
        core_ctx: &UninstantiableWrapper<P>,
    ) -> impl DualStackConverter<I, P::WeakDeviceId, S> {
        core_ctx.uninstantiable_unreachable::<Uninstantiable>()
    }

    fn to_other_bound_socket_id(
        core_ctx: &UninstantiableWrapper<P>,
        _id: &S::SocketId<I, P::WeakDeviceId>,
    ) -> <S::SocketMapSpec<I::OtherVersion, P::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        P::WeakDeviceId,
        S::AddrSpec,
    >>::BoundSocketId {
        core_ctx.uninstantiable_unreachable()
    }

    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut datagram::BoundSockets<
                I,
                P::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, P::WeakDeviceId>,
            >,
            &mut datagram::BoundSockets<
                I::OtherVersion,
                P::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, P::WeakDeviceId>,
            >,
        ) -> O,
    >(
        core_ctx: &mut UninstantiableWrapper<P>,
        _cb: F,
    ) -> O {
        core_ctx.uninstantiable_unreachable()
    }

    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut datagram::BoundSockets<
                I::OtherVersion,
                P::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<<I>::OtherVersion, P::WeakDeviceId>,
            >,
        ) -> O,
    >(
        core_ctx: &mut UninstantiableWrapper<P>,
        _cb: F,
    ) -> O {
        core_ctx.uninstantiable_unreachable()
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        core_ctx: &mut UninstantiableWrapper<P>,
        _cb: F,
    ) -> O {
        core_ctx.uninstantiable_unreachable()
    }
}

impl<
        I: tcp_socket::DualStackIpExt,
        D: WeakDeviceIdentifier,
        BT: TcpBindingsTypes,
        P: tcp_socket::TcpDemuxContext<I, D, BT>,
    > tcp_socket::TcpDemuxContext<I, D, BT> for UninstantiableWrapper<P>
{
    type IpTransportCtx<'a> = P::IpTransportCtx<'a>;
    fn with_demux<O, F: FnOnce(&tcp_socket::DemuxState<I, D, BT>) -> O>(&mut self, _cb: F) -> O {
        self.uninstantiable_unreachable()
    }
    fn with_demux_mut<O, F: FnOnce(&mut tcp_socket::DemuxState<I, D, BT>) -> O>(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
    fn with_demux_mut_and_ip_transport_ctx<
        O,
        F: FnOnce(&mut tcp_socket::DemuxState<I, D, BT>, &mut Self::IpTransportCtx<'_>) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpLayerIpExt, C, P: DeviceIpSocketHandler<I, C>> DeviceIpSocketHandler<I, C>
    for UninstantiableWrapper<P>
{
    fn get_mms(
        &mut self,
        _ctx: &mut C,
        _ip_sock: &IpSock<I, Self::WeakDeviceId>,
    ) -> Result<Mms, MmsError> {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, C, P: TransportIpContext<I, C>> TransportIpContext<I, C>
    for UninstantiableWrapper<P>
{
    type DevicesWithAddrIter<'s> = P::DevicesWithAddrIter<'s> where P: 's;
    fn get_devices_with_assigned_addr(
        &mut self,
        _addr: SpecifiedAddr<I::Addr>,
    ) -> Self::DevicesWithAddrIter<'_> {
        self.uninstantiable_unreachable()
    }
    fn get_default_hop_limits(&mut self, _device: Option<&Self::DeviceId>) -> HopLimits {
        self.uninstantiable_unreachable()
    }
    fn confirm_reachable_with_destination(
        &mut self,
        _ctx: &mut C,
        _dst: SpecifiedAddr<I::Addr>,
        _device: Option<&Self::DeviceId>,
    ) {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, C, P: IpSocketHandler<I, C>> IpSocketHandler<I, C> for UninstantiableWrapper<P> {
    fn new_ip_socket(
        &mut self,
        _ctx: &mut C,
        _device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
        _local_ip: Option<SocketIpAddr<I::Addr>>,
        _remote_ip: SocketIpAddr<I::Addr>,
        _proto: I::Proto,
    ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError> {
        self.uninstantiable_unreachable()
    }
    fn send_ip_packet<S, O>(
        &mut self,
        _ctx: &mut C,
        _socket: &IpSock<I, Self::WeakDeviceId>,
        _body: S,
        _mtu: Option<u32>,
        _options: &O,
    ) -> Result<(), (S, IpSockSendError)>
    where
        S: Serializer,
        S::Buffer: BufferMut,
        O: SendOptions<I>,
    {
        self.uninstantiable_unreachable()
    }
}

impl<P> tcp_socket::AsThisStack<P> for UninstantiableWrapper<P> {
    fn as_this_stack(&mut self) -> &mut P {
        self.uninstantiable_unreachable()
    }
}

impl<I: tcp_socket::DualStackIpExt> tcp_socket::DualStackDemuxIdConverter<I> for Uninstantiable {
    fn convert<D: WeakDeviceIdentifier, BT: tcp_socket::TcpBindingsTypes>(
        &self,
        _id: tcp_socket::TcpSocketId<I, D, BT>,
    ) -> <I::OtherVersion as tcp_socket::DualStackBaseIpExt>::DemuxSocketId<D, BT> {
        self.uninstantiable_unreachable()
    }
}

impl<
        I: tcp_socket::DualStackIpExt,
        D: WeakDeviceIdentifier,
        BT: TcpBindingsTypes,
        P: tcp_socket::TcpDualStackContext<I::OtherVersion, D, BT>,
    > tcp_socket::TcpDualStackContext<I, D, BT> for UninstantiableWrapper<P>
where
    for<'a> P::DualStackIpTransportCtx<'a>: CounterContext<TcpCounters<I>>,
{
    type Converter = Uninstantiable;
    type DualStackIpTransportCtx<'a> = P::DualStackIpTransportCtx<'a>;
    fn other_demux_id_converter(&self) -> Self::Converter {
        self.uninstantiable_unreachable()
    }

    fn dual_stack_enabled(&self, _ip_options: &I::DualStackIpOptions) -> bool {
        self.uninstantiable_unreachable()
    }

    fn set_dual_stack_enabled(&self, _ip_options: &mut I::DualStackIpOptions, _value: bool) {
        self.uninstantiable_unreachable()
    }

    fn with_both_demux_mut<
        O,
        F: FnOnce(
            &mut tcp_socket::DemuxState<I, D, BT>,
            &mut tcp_socket::DemuxState<I::OtherVersion, D, BT>,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }

    fn with_both_demux_mut_and_ip_transport_ctx<
        O,
        F: FnOnce(
            &mut tcp_socket::DemuxState<I, D, BT>,
            &mut tcp_socket::DemuxState<I::OtherVersion, D, BT>,
            &mut Self::DualStackIpTransportCtx<'_>,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}
