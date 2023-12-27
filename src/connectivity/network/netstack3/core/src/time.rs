// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for dealing with time and timers.

use core::fmt::Debug;
use derivative::Derivative;
use lock_order::Locked;
use tracing::trace;

use crate::{
    device::{self, DeviceId, DeviceLayerTimerId},
    ip::{
        self,
        device::{Ipv4DeviceTimerId, Ipv6DeviceTimerId},
        IpLayerTimerId,
    },
    transport::{self, TransportLayerTimerId},
    NonSyncContext, SyncCtx,
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
pub struct TimerId<C: NonSyncContext>(pub(crate) TimerIdInner<C>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) enum TimerIdInner<C: NonSyncContext> {
    /// A timer event in the device layer.
    DeviceLayer(DeviceLayerTimerId<C>),
    /// A timer event in the transport layer.
    TransportLayer(TransportLayerTimerId<C>),
    /// A timer event in the IP layer.
    IpLayer(IpLayerTimerId),
    /// A timer event for an IPv4 device.
    Ipv4Device(Ipv4DeviceTimerId<DeviceId<C>>),
    /// A timer event for an IPv6 device.
    Ipv6Device(Ipv6DeviceTimerId<DeviceId<C>>),
    /// A no-op timer event (used for tests)
    #[cfg(test)]
    Nop(usize),
}

impl<C: NonSyncContext> From<DeviceLayerTimerId<C>> for TimerId<C> {
    fn from(id: DeviceLayerTimerId<C>) -> TimerId<C> {
        TimerId(TimerIdInner::DeviceLayer(id))
    }
}

impl<C: NonSyncContext> From<Ipv4DeviceTimerId<DeviceId<C>>> for TimerId<C> {
    fn from(id: Ipv4DeviceTimerId<DeviceId<C>>) -> TimerId<C> {
        TimerId(TimerIdInner::Ipv4Device(id))
    }
}

impl<C: NonSyncContext> From<Ipv6DeviceTimerId<DeviceId<C>>> for TimerId<C> {
    fn from(id: Ipv6DeviceTimerId<DeviceId<C>>) -> TimerId<C> {
        TimerId(TimerIdInner::Ipv6Device(id))
    }
}

impl<C: NonSyncContext> From<IpLayerTimerId> for TimerId<C> {
    fn from(id: IpLayerTimerId) -> TimerId<C> {
        TimerId(TimerIdInner::IpLayer(id))
    }
}

impl<C: NonSyncContext> From<TransportLayerTimerId<C>> for TimerId<C> {
    fn from(id: TransportLayerTimerId<C>) -> Self {
        TimerId(TimerIdInner::TransportLayer(id))
    }
}

impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    DeviceLayerTimerId<C>,
    TimerId(TimerIdInner::DeviceLayer(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    IpLayerTimerId,
    TimerId(TimerIdInner::IpLayer(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    Ipv4DeviceTimerId<DeviceId<C>>,
    TimerId(TimerIdInner::Ipv4Device(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    Ipv6DeviceTimerId<DeviceId<C>>,
    TimerId(TimerIdInner::Ipv6Device(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    TransportLayerTimerId<C>,
    TimerId(TimerIdInner::TransportLayer(id)),
    id
);

/// Handles a generic timer event.
pub fn handle_timer<NonSyncCtx: NonSyncContext>(
    core_ctx: &SyncCtx<NonSyncCtx>,
    bindings_ctx: &mut NonSyncCtx,
    id: TimerId<NonSyncCtx>,
) {
    trace!("handle_timer: dispatching timerid: {:?}", id);
    let mut sync_ctx = Locked::new(core_ctx);

    match id {
        TimerId(TimerIdInner::DeviceLayer(x)) => {
            device::handle_timer(&mut sync_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::TransportLayer(x)) => {
            transport::handle_timer(&mut sync_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::IpLayer(x)) => {
            ip::handle_timer(&mut sync_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::Ipv4Device(x)) => {
            ip::device::handle_ipv4_timer(&mut sync_ctx, bindings_ctx, x);
        }
        TimerId(TimerIdInner::Ipv6Device(x)) => {
            ip::device::handle_ipv6_timer(&mut sync_ctx, bindings_ctx, x);
        }
        #[cfg(test)]
        TimerId(TimerIdInner::Nop(_)) => {
            crate::context::CounterContext::with_counters(&sync_ctx, |counters: &TimerCounters| {
                counters.nop.increment()
            })
        }
    }
}

#[cfg(test)]
/// Timer-related counters.
#[derive(Default)]
pub(crate) struct TimerCounters {
    /// Count of no-op timers handled.
    pub(crate) nop: crate::counters::Counter,
}

#[cfg(test)]
impl<C: NonSyncContext> lock_order::lock::UnlockedAccess<crate::lock_ordering::TimerCounters>
    for SyncCtx<C>
{
    type Data = TimerCounters;
    type Guard<'l> = &'l TimerCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.state.timer_counters
    }
}

#[cfg(test)]
impl<NonSyncCtx: NonSyncContext, L> crate::context::CounterContext<TimerCounters>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_counters<O, F: FnOnce(&TimerCounters) -> O>(&self, cb: F) -> O {
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
