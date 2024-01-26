// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for dealing with time and timers.

use core::fmt::Debug;
use derivative::Derivative;

use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};
use tracing::trace;

use crate::{
    context::{TimerContext, TimerHandler},
    device::{DeviceId, DeviceLayerTimerId},
    inspect::InspectableValue,
    ip::{
        device::{IpDeviceIpExt, IpDeviceTimerId},
        IpLayerTimerId,
    },
    transport::TransportLayerTimerId,
    BindingsTypes,
};

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

/// A type representing an instant in time.
///
/// `Instant` can be implemented by any type which represents an instant in
/// time. This can include any sort of real-world clock time (e.g.,
/// [`std::time::Instant`]) or fake time such as in testing.
pub trait Instant:
    Sized + Ord + Copy + Clone + Debug + Send + Sync + InspectableValue + 'static
{
    /// Returns the amount of time elapsed from another instant to this one.
    ///
    /// # Panics
    ///
    /// This function will panic if `earlier` is later than `self`.
    fn duration_since(&self, earlier: Self) -> core::time::Duration;

    /// Returns the amount of time elapsed from another instant to this one,
    /// saturating at zero.
    fn saturating_duration_since(&self, earlier: Self) -> core::time::Duration;

    /// Returns `Some(t)` where `t` is the time `self + duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    fn checked_add(&self, duration: core::time::Duration) -> Option<Self>;

    /// Unwraps the result from `checked_add`.
    ///
    /// # Panics
    ///
    /// This function will panic if the addition makes the clock wrap around.
    fn add(&self, duration: core::time::Duration) -> Self {
        self.checked_add(duration).unwrap_or_else(|| {
            panic!("clock wraps around when adding {:?} to {:?}", duration, *self);
        })
    }

    /// Returns `Some(t)` where `t` is the time `self - duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    fn checked_sub(&self, duration: core::time::Duration) -> Option<Self>;
}
