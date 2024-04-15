// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A collection of uninstantiable types.
//!
//! These uninstantiable types can be used to satisfy trait bounds in
//! uninstantiable situations. For example,
//! [`crate::socket::datagram::DatagramBoundStateContext::DualStackContext`]
//! is set to the [`UninstantiableWrapper`] when implemented for Ipv4, because
//! IPv4 sockets do not support dualstack operations.

use core::{convert::Infallible as Never, marker::PhantomData};

use explicit::UnreachableExt as _;
use net_types::{ip::Ip, SpecifiedAddr};
use packet::{BufferMut, Serializer};

use crate::{
    context::{CoreTimerContext, CounterContext, TimerBindingsTypes},
    convert::BidirectionalConverter,
    device::{self, Device, DeviceIdContext},
    ip::{
        socket::{DeviceIpSocketHandler, IpSock, IpSocketHandler, Mms, MmsError, SendOptions},
        EitherDeviceId, HopLimits, IpExt, IpLayerIpExt, IpSockCreationError, IpSockSendError,
        TransportIpContext,
    },
    socket::{
        address::SocketIpAddr,
        datagram::{
            self, DatagramBoundStateContext, DatagramSocketMapSpec, DatagramSocketSpec,
            DualStackDatagramBoundStateContext, NonDualStackDatagramBoundStateContext,
        },
        MaybeDualStack,
    },
    transport::tcp::{
        socket::{self as tcp_socket, TcpBindingsTypes},
        TcpCounters,
    },
};

/// An uninstantiable type.
#[derive(Clone, Copy)]
pub struct Uninstantiable(Never);

impl AsRef<Never> for Uninstantiable {
    fn as_ref(&self) -> &Never {
        &self.0
    }
}

impl<I, O> BidirectionalConverter<I, O> for Uninstantiable {
    fn convert_back(&self, _: O) -> I {
        self.uninstantiable_unreachable()
    }
    fn convert(&self, _: I) -> O {
        self.uninstantiable_unreachable()
    }
}

/// An uninstantiable type that wraps an instantiable type, `A`.
///
/// This type can be used to more easily implement traits where `A` already
/// implements the trait.
// TODO(https://github.com/rust-lang/rust/issues/118212): Simplify the trait
// implementations once Rust supports function delegation.
pub struct UninstantiableWrapper<A>(Never, PhantomData<A>);

impl<A> AsRef<Never> for UninstantiableWrapper<A> {
    fn as_ref(&self) -> &Never {
        let Self(never, _marker) = self;
        &never
    }
}

impl<D: Device, C: DeviceIdContext<D>> DeviceIdContext<D> for UninstantiableWrapper<C> {
    type DeviceId = C::DeviceId;
    type WeakDeviceId = C::WeakDeviceId;
}

impl<T, BT, C> CoreTimerContext<T, BT> for UninstantiableWrapper<C>
where
    BT: TimerBindingsTypes,
    C: CoreTimerContext<T, BT>,
{
    fn convert_timer(dispatch_id: T) -> BT::DispatchId {
        C::convert_timer(dispatch_id)
    }
}

impl<I: datagram::IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DatagramBoundStateContext<I, C, S> for UninstantiableWrapper<P>
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;
    type DualStackContext = P::DualStackContext;
    type NonDualStackContext = P::NonDualStackContext;
    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        self.uninstantiable_unreachable()
    }
    fn with_bound_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &datagram::BoundSockets<
                I,
                Self::WeakDeviceId,
                <S as DatagramSocketSpec>::AddrSpec,
                <S as DatagramSocketSpec>::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut datagram::BoundSockets<
                I,
                Self::WeakDeviceId,
                <S as DatagramSocketSpec>::AddrSpec,
                <S as DatagramSocketSpec>::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}

impl<I: datagram::IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    NonDualStackDatagramBoundStateContext<I, C, S> for UninstantiableWrapper<P>
{
    type Converter = Uninstantiable;
    fn converter(&self) -> Self::Converter {
        self.uninstantiable_unreachable()
    }
}

impl<I: datagram::IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DualStackDatagramBoundStateContext<I, C, S> for UninstantiableWrapper<P>
where
    for<'a> P::IpSocketsCtx<'a>: TransportIpContext<I::OtherVersion, C>,
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;

    fn dual_stack_enabled(
        &self,
        _state: &impl AsRef<datagram::IpOptions<I, Self::WeakDeviceId, S>>,
    ) -> bool {
        self.uninstantiable_unreachable()
    }

    fn to_other_send_options<'a>(
        &self,
        _state: &'a datagram::IpOptions<I, Self::WeakDeviceId, S>,
    ) -> &'a datagram::SocketHopLimits<I::OtherVersion> {
        self.uninstantiable_unreachable()
    }

    type Converter = Uninstantiable;
    fn converter(&self) -> Self::Converter {
        self.uninstantiable_unreachable()
    }

    fn to_other_bound_socket_id(
        &self,
        _id: &S::SocketId<I, Self::WeakDeviceId>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::BoundSocketId {
        self.uninstantiable_unreachable()
    }

    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut datagram::BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut datagram::BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }

    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut datagram::BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<<I>::OtherVersion, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}

impl<
        I: tcp_socket::DualStackIpExt,
        D: device::WeakId,
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
    fn convert<D: device::WeakId, BT: tcp_socket::TcpBindingsTypes>(
        &self,
        _id: tcp_socket::TcpSocketId<I, D, BT>,
    ) -> <I::OtherVersion as tcp_socket::DualStackIpExt>::DemuxSocketId<D, BT> {
        self.uninstantiable_unreachable()
    }
}

impl<
        I: tcp_socket::DualStackIpExt,
        D: device::WeakId,
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

impl<I: Ip, P> CounterContext<TcpCounters<I>> for UninstantiableWrapper<P> {
    fn with_counters<O, F: FnOnce(&TcpCounters<I>) -> O>(&self, _cb: F) -> O {
        self.uninstantiable_unreachable()
    }
}
