// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Marker traits with blanket implementations.
//!
//! Traits in this module exist to be exported as markers to bindings without
//! exposing the internal traits directly.

use net_types::{
    ethernet::Mac,
    ip::{Ipv4, Ipv6},
};

use crate::{
    context::{
        CounterContext, EventContext, InstantBindingsTypes, ReferenceNotifiers, RngContext,
        TimerContext, TracingContext,
    },
    device::{
        self, AnyDevice, DeviceId, DeviceIdContext, DeviceLayerTypes, EthernetLinkDevice,
        WeakDeviceId,
    },
    ip::{
        self,
        device::{IpDeviceBindingsContext, IpDeviceConfigurationContext, IpDeviceIpExt},
        icmp::IcmpBindingsContext,
        IpLayerBindingsContext, IpLayerContext, IpLayerIpExt,
    },
    socket,
    transport::{
        self,
        tcp::socket::{TcpBindingsContext, TcpBindingsTypes, TcpContext},
        udp::{UdpCounters, UdpStateBindingsContext},
    },
    TimerId,
};

/// A marker for extensions to IP types.
pub trait IpExt:
    IpLayerIpExt
    + IpDeviceIpExt
    + ip::icmp::IcmpIpExt
    + ip::device::IpDeviceIpExt
    + transport::tcp::socket::DualStackIpExt
    + socket::datagram::DualStackIpExt
{
}

impl<O> IpExt for O where
    O: ip::IpLayerIpExt
        + IpDeviceIpExt
        + ip::icmp::IcmpIpExt
        + ip::device::IpDeviceIpExt
        + transport::tcp::socket::DualStackIpExt
        + socket::datagram::DualStackIpExt
{
}

/// A marker trait for core context implementations.
///
/// This trait allows bindings to express trait bounds on routines that have IP
/// type parameters. It is an umbrella of all the core contexts that must be
/// implemented by [`crate::context::UnlockedCoreCtx`] to satisfy all the API
/// objects vended by [`crate::api::CoreApi`].
pub trait CoreContext<I, BC>:
    transport::udp::StateContext<I, BC>
    + CounterContext<UdpCounters<I>>
    + TcpContext<I, BC>
    + ip::icmp::socket::StateContext<I, BC>
    + ip::icmp::IcmpStateContext
    + IpLayerContext<I, BC>
    + IpDeviceConfigurationContext<I, BC>
    + DeviceIdContext<AnyDevice, DeviceId = DeviceId<BC>, WeakDeviceId = WeakDeviceId<BC>>
where
    I: IpExt,
    BC: IpBindingsContext<I>,
{
}

impl<I, BC, O> CoreContext<I, BC> for O
where
    I: IpExt,
    BC: IpBindingsContext<I>,
    O: transport::udp::StateContext<I, BC>
        + CounterContext<UdpCounters<I>>
        + TcpContext<I, BC>
        + ip::icmp::socket::StateContext<I, BC>
        + ip::icmp::IcmpStateContext
        + IpLayerContext<I, BC>
        + IpDeviceConfigurationContext<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = DeviceId<BC>, WeakDeviceId = WeakDeviceId<BC>>,
{
}

/// A marker trait for all the types stored in core objects that are specified
/// by bindings.
pub trait BindingsTypes: InstantBindingsTypes + DeviceLayerTypes + TcpBindingsTypes {}

impl<O> BindingsTypes for O where O: InstantBindingsTypes + DeviceLayerTypes + TcpBindingsTypes {}

/// The execution context provided by bindings for a given IP version.
pub trait IpBindingsContext<I: IpExt>:
    BindingsTypes
    + RngContext
    + EventContext<
        ip::device::nud::Event<
            Mac,
            device::EthernetDeviceId<Self>,
            I,
            <Self as InstantBindingsTypes>::Instant,
        >,
    > + UdpStateBindingsContext<I, DeviceId<Self>>
    + TcpBindingsContext<I, WeakDeviceId<Self>>
    + IcmpBindingsContext<I, DeviceId<Self>>
    + IpDeviceBindingsContext<I, DeviceId<Self>>
    + IpLayerBindingsContext<I, DeviceId<Self>>
    + ip::device::nud::LinkResolutionContext<EthernetLinkDevice>
    + device::DeviceLayerEventDispatcher
    + device::socket::DeviceSocketBindingsContext<DeviceId<Self>>
    + ReferenceNotifiers
    + TracingContext
    + 'static
{
}

impl<I, BC> IpBindingsContext<I> for BC
where
    I: IpExt,
    BC: BindingsTypes
        + RngContext
        + EventContext<
            ip::device::nud::Event<
                Mac,
                device::EthernetDeviceId<Self>,
                I,
                <Self as InstantBindingsTypes>::Instant,
            >,
        > + UdpStateBindingsContext<I, DeviceId<Self>>
        + TcpBindingsContext<I, WeakDeviceId<Self>>
        + IcmpBindingsContext<I, DeviceId<Self>>
        + IpDeviceBindingsContext<I, DeviceId<Self>>
        + IpLayerBindingsContext<I, DeviceId<Self>>
        + ip::device::nud::LinkResolutionContext<EthernetLinkDevice>
        + device::DeviceLayerEventDispatcher
        + device::socket::DeviceSocketBindingsContext<DeviceId<Self>>
        + ReferenceNotifiers
        + TracingContext
        + 'static,
{
}

/// The execution context provided by bindings.
pub trait BindingsContext:
    IpBindingsContext<Ipv4> + IpBindingsContext<Ipv6> + TimerContext<TimerId<Self>>
{
}

impl<BC> BindingsContext for BC where
    BC: IpBindingsContext<Ipv4> + IpBindingsContext<Ipv6> + TimerContext<TimerId<Self>>
{
}
