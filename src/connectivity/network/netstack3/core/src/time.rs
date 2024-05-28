// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for dealing with time and timers.

use core::convert::Infallible as Never;

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};
use tracing::trace;

use crate::{
    context::{CoreCtx, CoreTimerContext, HandleableTimer, TimerHandler},
    device::{DeviceLayerTimerId, WeakDeviceId},
    ip::{
        device::{IpDeviceIpExt, IpDeviceTimerId},
        device_integration::IpAddrCtxSpec,
        IpLayerTimerId,
    },
    transport::TransportLayerTimerId,
    BindingsTypes,
};

pub use netstack3_base::Instant;

/// The identifier for any timer event.
#[derive(Derivative, GenericOverIp)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
#[generic_over_ip()]
pub struct TimerId<BT: BindingsTypes>(pub(crate) TimerIdInner<BT>);

#[derive(Derivative, GenericOverIp)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
#[generic_over_ip()]
pub enum TimerIdInner<BT: BindingsTypes> {
    /// A timer event in the device layer.
    DeviceLayer(DeviceLayerTimerId<BT>),
    /// A timer event in the transport layer.
    TransportLayer(TransportLayerTimerId<BT>),
    /// A timer event in the IP layer.
    IpLayer(IpLayerTimerId),
    /// A timer event for an IPv4 device.
    Ipv4Device(IpDeviceTimerId<Ipv4, WeakDeviceId<BT>, IpAddrCtxSpec<BT>>),
    /// A timer event for an IPv6 device.
    Ipv6Device(IpDeviceTimerId<Ipv6, WeakDeviceId<BT>, IpAddrCtxSpec<BT>>),
}

impl<BT: BindingsTypes> From<DeviceLayerTimerId<BT>> for TimerId<BT> {
    fn from(id: DeviceLayerTimerId<BT>) -> TimerId<BT> {
        TimerId(TimerIdInner::DeviceLayer(id))
    }
}

impl<BT: BindingsTypes> From<IpLayerTimerId> for TimerId<BT> {
    fn from(id: IpLayerTimerId) -> TimerId<BT> {
        TimerId(TimerIdInner::IpLayer(id))
    }
}

impl<BT: BindingsTypes> From<TransportLayerTimerId<BT>> for TimerId<BT> {
    fn from(id: TransportLayerTimerId<BT>) -> Self {
        TimerId(TimerIdInner::TransportLayer(id))
    }
}

impl<BT: BindingsTypes, I: IpDeviceIpExt>
    From<IpDeviceTimerId<I, WeakDeviceId<BT>, IpAddrCtxSpec<BT>>> for TimerId<BT>
{
    fn from(value: IpDeviceTimerId<I, WeakDeviceId<BT>, IpAddrCtxSpec<BT>>) -> Self {
        I::map_ip(
            value,
            |v4| TimerId(TimerIdInner::Ipv4Device(v4)),
            |v6| TimerId(TimerIdInner::Ipv6Device(v6)),
        )
    }
}

impl<CC, BT> HandleableTimer<CC, BT> for TimerId<BT>
where
    BT: BindingsTypes,
    CC: TimerHandler<BT, DeviceLayerTimerId<BT>>
        + TimerHandler<BT, TransportLayerTimerId<BT>>
        + TimerHandler<BT, IpLayerTimerId>
        + TimerHandler<BT, IpDeviceTimerId<Ipv4, WeakDeviceId<BT>, IpAddrCtxSpec<BT>>>
        + TimerHandler<BT, IpDeviceTimerId<Ipv6, WeakDeviceId<BT>, IpAddrCtxSpec<BT>>>,
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BT) {
        trace!("handle_timer: dispatching timerid: {self:?}");
        match self {
            TimerId(TimerIdInner::DeviceLayer(x)) => core_ctx.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::TransportLayer(x)) => core_ctx.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::IpLayer(x)) => core_ctx.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::Ipv4Device(x)) => core_ctx.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::Ipv6Device(x)) => core_ctx.handle_timer(bindings_ctx, x),
        }
    }
}

impl<'a, BT, L> CoreTimerContext<Never, BT> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
{
    fn convert_timer(dispatch_id: Never) -> <BT as netstack3_base::TimerBindingsTypes>::DispatchId {
        match dispatch_id {}
    }
}
