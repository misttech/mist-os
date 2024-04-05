// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for dealing with time and timers.

use core::convert::Infallible as Never;

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};
use tracing::trace;

use crate::{
    context::{CoreCtx, CoreTimerContext, TimerContext, TimerHandler},
    device::{DeviceId, DeviceLayerTimerId},
    ip::{
        device::{IpDeviceIpExt, IpDeviceTimerId},
        IpLayerTimerId,
    },
    transport::TransportLayerTimerId,
    BindingsTypes,
};

pub use netstack3_base::{Instant, LocalTimerHeap};

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
pub(crate) enum TimerIdInner<BT: BindingsTypes> {
    /// A timer event in the device layer.
    DeviceLayer(DeviceLayerTimerId<BT>),
    /// A timer event in the transport layer.
    TransportLayer(TransportLayerTimerId<BT>),
    /// A timer event in the IP layer.
    IpLayer(IpLayerTimerId),
    /// A timer event for an IPv4 device.
    Ipv4Device(IpDeviceTimerId<Ipv4, DeviceId<BT>>),
    /// A timer event for an IPv6 device.
    Ipv6Device(IpDeviceTimerId<Ipv6, DeviceId<BT>>),
}

impl<BT: BindingsTypes> TimerIdInner<BT> {
    fn as_ip_device<I: IpDeviceIpExt>(&self) -> Option<&IpDeviceTimerId<I, DeviceId<BT>>> {
        I::map_ip(
            self,
            |t| match t {
                TimerIdInner::Ipv4Device(d) => Some(d),
                _ => None,
            },
            |t| match t {
                TimerIdInner::Ipv6Device(d) => Some(d),
                _ => None,
            },
        )
    }
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

impl_timer_context!(
    BT: BindingsTypes,
    TimerId<BT>,
    DeviceLayerTimerId<BT>,
    TimerId(TimerIdInner::DeviceLayer(id)),
    id
);
impl_timer_context!(
    BT: BindingsTypes,
    TimerId<BT>,
    IpLayerTimerId,
    TimerId(TimerIdInner::IpLayer(id)),
    id
);

impl<BT: BindingsTypes, I: IpDeviceIpExt> From<IpDeviceTimerId<I, DeviceId<BT>>> for TimerId<BT> {
    fn from(value: IpDeviceTimerId<I, DeviceId<BT>>) -> Self {
        I::map_ip(
            value,
            |v4| TimerId(TimerIdInner::Ipv4Device(v4)),
            |v6| TimerId(TimerIdInner::Ipv6Device(v6)),
        )
    }
}

impl<BT, I, O> TimerContext<IpDeviceTimerId<I, DeviceId<BT>>> for O
where
    BT: BindingsTypes,
    I: IpDeviceIpExt,
    O: TimerContext<TimerId<BT>>,
{
    fn schedule_timer_instant(
        &mut self,
        time: Self::Instant,
        id: IpDeviceTimerId<I, DeviceId<BT>>,
    ) -> Option<Self::Instant> {
        TimerContext::<TimerId<BT>>::schedule_timer_instant(self, time, id.into())
    }

    fn cancel_timer(&mut self, id: IpDeviceTimerId<I, DeviceId<BT>>) -> Option<Self::Instant> {
        TimerContext::<TimerId<BT>>::cancel_timer(self, id.into())
    }

    fn cancel_timers_with<F: FnMut(&IpDeviceTimerId<I, DeviceId<BT>>) -> bool>(
        &mut self,
        mut f: F,
    ) {
        TimerContext::<TimerId<BT>>::cancel_timers_with(self, move |TimerId(timer_id)| {
            timer_id.as_ip_device::<I>().is_some_and(|d| f(d))
        })
    }

    fn scheduled_instant(&self, id: IpDeviceTimerId<I, DeviceId<BT>>) -> Option<Self::Instant> {
        TimerContext::<TimerId<BT>>::scheduled_instant(self, id.into())
    }
}

impl_timer_context!(
    BT: BindingsTypes,
    TimerId<BT>,
    TransportLayerTimerId<BT>,
    TimerId(TimerIdInner::TransportLayer(id)),
    id
);

impl<BT, CC> TimerHandler<BT, TimerId<BT>> for CC
where
    BT: BindingsTypes,
    CC: TimerHandler<BT, DeviceLayerTimerId<BT>>
        + TimerHandler<BT, TransportLayerTimerId<BT>>
        + TimerHandler<BT, IpLayerTimerId>
        + TimerHandler<BT, IpDeviceTimerId<Ipv4, DeviceId<BT>>>
        + TimerHandler<BT, IpDeviceTimerId<Ipv6, DeviceId<BT>>>,
{
    fn handle_timer(&mut self, bindings_ctx: &mut BT, id: TimerId<BT>) {
        trace!("handle_timer: dispatching timerid: {id:?}");
        match id {
            TimerId(TimerIdInner::DeviceLayer(x)) => self.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::TransportLayer(x)) => self.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::IpLayer(x)) => self.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::Ipv4Device(x)) => self.handle_timer(bindings_ctx, x),
            TimerId(TimerIdInner::Ipv6Device(x)) => self.handle_timer(bindings_ctx, x),
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
