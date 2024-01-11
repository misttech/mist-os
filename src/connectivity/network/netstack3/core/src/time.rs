// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for dealing with time and timers.

use core::fmt::Debug;
use derivative::Derivative;

use tracing::trace;

use crate::{
    device::{self, DeviceId, DeviceLayerTimerId},
    ip::{
        self,
        device::{Ipv4DeviceTimerId, Ipv6DeviceTimerId},
        IpLayerTimerId,
    },
    transport::{self, TransportLayerTimerId},
    BindingsContext, BindingsTypes, CoreCtx, SyncCtx,
};

/// The identifier for any timer event.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub struct TimerId<BT: BindingsTypes>(pub(crate) TimerIdInner<BT>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) enum TimerIdInner<BT: BindingsTypes> {
    /// A timer event in the device layer.
    DeviceLayer(DeviceLayerTimerId<BT>),
    /// A timer event in the transport layer.
    TransportLayer(TransportLayerTimerId<BT>),
    /// A timer event in the IP layer.
    IpLayer(IpLayerTimerId),
    /// A timer event for an IPv4 device.
    Ipv4Device(Ipv4DeviceTimerId<DeviceId<BT>>),
    /// A timer event for an IPv6 device.
    Ipv6Device(Ipv6DeviceTimerId<DeviceId<BT>>),
    /// A no-op timer event (used for tests)
    #[cfg(test)]
    Nop(usize),
}

impl<BT: BindingsTypes> From<DeviceLayerTimerId<BT>> for TimerId<BT> {
    fn from(id: DeviceLayerTimerId<BT>) -> TimerId<BT> {
        TimerId(TimerIdInner::DeviceLayer(id))
    }
}

impl<BT: BindingsTypes> From<Ipv4DeviceTimerId<DeviceId<BT>>> for TimerId<BT> {
    fn from(id: Ipv4DeviceTimerId<DeviceId<BT>>) -> TimerId<BT> {
        TimerId(TimerIdInner::Ipv4Device(id))
    }
}

impl<BT: BindingsTypes> From<Ipv6DeviceTimerId<DeviceId<BT>>> for TimerId<BT> {
    fn from(id: Ipv6DeviceTimerId<DeviceId<BT>>) -> TimerId<BT> {
        TimerId(TimerIdInner::Ipv6Device(id))
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
impl_timer_context!(
    BT: BindingsTypes,
    TimerId<BT>,
    Ipv4DeviceTimerId<DeviceId<BT>>,
    TimerId(TimerIdInner::Ipv4Device(id)),
    id
);
impl_timer_context!(
    BT: BindingsTypes,
    TimerId<BT>,
    Ipv6DeviceTimerId<DeviceId<BT>>,
    TimerId(TimerIdInner::Ipv6Device(id)),
    id
);
impl_timer_context!(
    BT: BindingsTypes,
    TimerId<BT>,
    TransportLayerTimerId<BT>,
    TimerId(TimerIdInner::TransportLayer(id)),
    id
);

/// Handles a generic timer event.
pub fn handle_timer<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: TimerId<BC>,
) {
    trace!("handle_timer: dispatching timerid: {:?}", id);
    let mut core_ctx = CoreCtx::new_deprecated(core_ctx);

    match id {
        TimerId(TimerIdInner::DeviceLayer(x)) => {
            device::handle_timer(&mut core_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::TransportLayer(x)) => {
            transport::handle_timer(&mut core_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::IpLayer(x)) => {
            ip::handle_timer(&mut core_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::Ipv4Device(x)) => {
            ip::device::handle_ipv4_timer(&mut core_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::Ipv6Device(x)) => {
            ip::device::handle_ipv6_timer(&mut core_ctx, bindings_ctx, x);
        }
        #[cfg(test)]
        TimerId(TimerIdInner::Nop(_)) => {
            crate::context::CounterContext::with_counters(&core_ctx, |counters: &TimerCounters| {
                counters.nop.increment()
            })
        }
    }
}

#[cfg(test)]
/// Timer-related counters.
#[derive(Default)]
pub struct TimerCounters {
    /// Count of no-op timers handled.
    pub(crate) nop: crate::counters::Counter,
}

#[cfg(test)]
impl<BT: BindingsTypes> lock_order::lock::UnlockedAccess<crate::lock_ordering::TimerCounters>
    for crate::StackState<BT>
{
    type Data = TimerCounters;
    type Guard<'l> = &'l TimerCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.timer_counters
    }
}

#[cfg(test)]
impl<BT: BindingsTypes, L> crate::context::CounterContext<TimerCounters> for CoreCtx<'_, BT, L> {
    fn with_counters<O, F: FnOnce(&TimerCounters) -> O>(&self, cb: F) -> O {
        use lock_order::wrap::prelude::*;
        cb(self.unlocked_access::<crate::lock_ordering::TimerCounters>())
    }
}

/// A type representing an instant in time.
///
/// `Instant` can be implemented by any type which represents an instant in
/// time. This can include any sort of real-world clock time (e.g.,
/// [`std::time::Instant`]) or fake time such as in testing.
pub trait Instant: Sized + Ord + Copy + Clone + Debug + Send + Sync {
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
