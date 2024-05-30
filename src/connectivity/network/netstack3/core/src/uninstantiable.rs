// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Groups trait implementations for uninstantiable types.

use explicit::UnreachableExt as _;

use netstack3_base::{CounterContext, Uninstantiable, UninstantiableWrapper, WeakDeviceIdentifier};

use crate::transport::tcp::{
    socket::{self as tcp_socket, TcpBindingsTypes},
    TcpCounters,
};

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
