// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Marker traits with blanket implementations.
//!
//! Traits in this module exist to be exported as markers to bindings without
//! exposing the internal traits directly.

use crate::{
    context::{BindingsContext, CounterContext},
    device::{AnyDevice, DeviceId, DeviceIdContext, WeakDeviceId},
    ip::icmp::IcmpBindingsContext,
    transport::udp::{UdpCounters, UdpStateBindingsContext},
};

/// A marker for extensions to IP types.
pub trait IpExt:
    crate::ip::IpExt
    + crate::ip::icmp::IcmpIpExt
    + crate::transport::tcp::socket::DualStackIpExt
    + crate::socket::datagram::DualStackIpExt
{
}

impl<O> IpExt for O where
    O: crate::ip::IpExt
        + crate::ip::icmp::IcmpIpExt
        + crate::transport::tcp::socket::DualStackIpExt
        + crate::socket::datagram::DualStackIpExt
{
}

/// A marker trait for core context implementations.
///
/// This trait allows bindings to express trait bounds on routines that have IP
/// type parameters. It is an umbrella of all the core contexts that must be
/// implemented by [`crate::context::UnlockedCoreCtx`] to satisfy all the API
/// objects vended by [`crate::api::CoreApi`].
pub trait CoreContext<I, BC>:
    crate::transport::udp::StateContext<I, BC>
    + CounterContext<UdpCounters<I>>
    + crate::ip::icmp::socket::StateContext<I, BC>
    + crate::ip::icmp::IcmpStateContext
    + DeviceIdContext<AnyDevice, DeviceId = DeviceId<BC>, WeakDeviceId = WeakDeviceId<BC>>
where
    I: IpExt,
    BC: BindingsContext
        + UdpStateBindingsContext<I, Self::DeviceId>
        + IcmpBindingsContext<I, Self::DeviceId>,
{
}

impl<I, BC, O> CoreContext<I, BC> for O
where
    I: IpExt,
    BC: BindingsContext
        + UdpStateBindingsContext<I, O::DeviceId>
        + IcmpBindingsContext<I, O::DeviceId>,
    O: crate::transport::udp::StateContext<I, BC>
        + CounterContext<UdpCounters<I>>
        + crate::ip::icmp::socket::StateContext<I, BC>
        + crate::ip::icmp::IcmpStateContext
        + DeviceIdContext<AnyDevice, DeviceId = DeviceId<BC>, WeakDeviceId = WeakDeviceId<BC>>,
{
}
