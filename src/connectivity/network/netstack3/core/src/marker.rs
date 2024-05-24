// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Marker traits with blanket implementations.
//!
//! Traits in this module exist to be exported as markers to bindings without
//! exposing the internal traits directly.

use net_types::ip::{Ipv4, Ipv6};

use crate::{
    context::{
        CounterContext, InstantBindingsTypes, ReferenceNotifiers, RngContext, TimerBindingsTypes,
        TracingContext,
    },
    device::{
        self, AnyDevice, DeviceId, DeviceIdContext, DeviceLayerTypes, EthernetDeviceId,
        EthernetLinkDevice, EthernetWeakDeviceId, WeakDeviceId,
    },
    filter::{FilterBindingsContext, FilterBindingsTypes},
    ip::{
        self,
        device::{
            IpDeviceBindingsContext, IpDeviceConfigurationContext, IpDeviceConfigurationHandler,
            IpDeviceIpExt,
        },
        icmp::{IcmpBindingsContext, IcmpBindingsTypes},
        nud::{NudBindingsContext, NudContext},
        raw::{RawIpSocketMapContext, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes},
        socket::IpSocketContext,
        IpLayerBindingsContext, IpLayerContext, IpLayerIpExt,
    },
    socket,
    transport::{
        self,
        tcp::socket::{TcpBindingsContext, TcpBindingsTypes, TcpContext},
        udp::{UdpBindingsContext, UdpBindingsTypes, UdpCounters},
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
    + ip::raw::RawIpSocketsIpExt
{
}

impl<O> IpExt for O where
    O: ip::IpLayerIpExt
        + IpDeviceIpExt
        + ip::icmp::IcmpIpExt
        + ip::device::IpDeviceIpExt
        + transport::tcp::socket::DualStackIpExt
        + socket::datagram::DualStackIpExt
        + ip::raw::RawIpSocketsIpExt
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
    + ip::icmp::IcmpSocketStateContext<I, BC>
    + ip::icmp::IcmpStateContext
    + IpLayerContext<I, BC>
    + NudContext<I, EthernetLinkDevice, BC>
    + IpDeviceConfigurationContext<I, BC>
    + IpDeviceConfigurationHandler<I, BC>
    + IpSocketContext<I, BC>
    + DeviceIdContext<AnyDevice, DeviceId = DeviceId<BC>, WeakDeviceId = WeakDeviceId<BC>>
    + DeviceIdContext<
        EthernetLinkDevice,
        DeviceId = EthernetDeviceId<BC>,
        WeakDeviceId = EthernetWeakDeviceId<BC>,
    > + RawIpSocketMapContext<I, BC>
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
        + ip::icmp::IcmpSocketStateContext<I, BC>
        + ip::icmp::IcmpStateContext
        + IpLayerContext<I, BC>
        + NudContext<I, EthernetLinkDevice, BC>
        + IpDeviceConfigurationContext<I, BC>
        + IpDeviceConfigurationHandler<I, BC>
        + IpSocketContext<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = DeviceId<BC>, WeakDeviceId = WeakDeviceId<BC>>
        + DeviceIdContext<
            EthernetLinkDevice,
            DeviceId = EthernetDeviceId<BC>,
            WeakDeviceId = EthernetWeakDeviceId<BC>,
        > + RawIpSocketMapContext<I, BC>,
{
}

/// A marker trait for all the types stored in core objects that are specified
/// by bindings.
pub trait BindingsTypes:
    InstantBindingsTypes
    + DeviceLayerTypes
    + TcpBindingsTypes
    + FilterBindingsTypes
    + IcmpBindingsTypes
    + RawIpSocketsBindingsTypes
    + UdpBindingsTypes
    + TimerBindingsTypes<DispatchId = TimerId<Self>>
{
}

impl<O> BindingsTypes for O where
    O: InstantBindingsTypes
        + DeviceLayerTypes
        + TcpBindingsTypes
        + FilterBindingsTypes
        + IcmpBindingsTypes
        + RawIpSocketsBindingsTypes
        + UdpBindingsTypes
        + TimerBindingsTypes<DispatchId = TimerId<Self>>
{
}

/// The execution context provided by bindings for a given IP version.
pub trait IpBindingsContext<I: IpExt>:
    BindingsTypes
    + RngContext
    + UdpBindingsContext<I, DeviceId<Self>>
    + TcpBindingsContext
    + FilterBindingsContext
    + IcmpBindingsContext<I, DeviceId<Self>>
    + RawIpSocketsBindingsContext<I, DeviceId<Self>>
    + IpDeviceBindingsContext<I, DeviceId<Self>>
    + IpLayerBindingsContext<I, DeviceId<Self>>
    + NudBindingsContext<I, EthernetLinkDevice, EthernetDeviceId<Self>>
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
        + UdpBindingsContext<I, DeviceId<Self>>
        + TcpBindingsContext
        + FilterBindingsContext
        + IcmpBindingsContext<I, DeviceId<Self>>
        + RawIpSocketsBindingsContext<I, DeviceId<Self>>
        + IpDeviceBindingsContext<I, DeviceId<Self>>
        + IpLayerBindingsContext<I, DeviceId<Self>>
        + NudBindingsContext<I, EthernetLinkDevice, EthernetDeviceId<Self>>
        + device::DeviceLayerEventDispatcher
        + device::socket::DeviceSocketBindingsContext<DeviceId<Self>>
        + ReferenceNotifiers
        + TracingContext
        + 'static,
{
}

/// The execution context provided by bindings.
pub trait BindingsContext: IpBindingsContext<Ipv4> + IpBindingsContext<Ipv6> {}
impl<BC> BindingsContext for BC where BC: IpBindingsContext<Ipv4> + IpBindingsContext<Ipv6> {}
