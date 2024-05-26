// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use explicit::UnreachableExt as _;
use netstack3_base::{socket::MaybeDualStack, Uninstantiable, UninstantiableWrapper};

use crate::internal::{
    base::TransportIpContext,
    datagram::{
        self,
        spec_context::{
            DatagramSpecBoundStateContext, DualStackDatagramSpecBoundStateContext,
            NonDualStackDatagramSpecBoundStateContext,
        },
        DatagramBoundStateContext, DatagramSocketMapSpec, DatagramSocketOptions,
        DatagramSocketSpec, DualStackConverter, NonDualStackConverter,
    },
};

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
