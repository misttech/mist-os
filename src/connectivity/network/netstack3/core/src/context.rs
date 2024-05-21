// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Execution contexts.
//!
//! This module defines "context" traits, which allow code in this crate to be
//! written agnostic to their execution context.
//!
//! All of the code in this crate operates in terms of "events". When an event
//! occurs (for example, a packet is received, an application makes a request,
//! or a timer fires), a function is called to handle that event. In response to
//! that event, the code may wish to emit new events (for example, to send a
//! packet, to respond to an application request, or to install a new timer).
//! The traits in this module provide the ability to emit new events. For
//! example, if, in order to handle some event, we need the ability to install
//! new timers, then the function to handle that event would take a
//! [`TimerContext`] parameter, which it could use to install new timers.
//!
//! Structuring code this way allows us to write code which is agnostic to
//! execution context - a test fake or any number of possible "real-world"
//! implementations of these traits all appear as indistinguishable, opaque
//! trait implementations to our code.
//!
//! The benefits are deeper than this, though. Large units of code can be
//! subdivided into smaller units that view each other as "contexts". For
//! example, the ARP implementation in the [`crate::device::arp`] module defines
//! the [`ArpContext`] trait, which is an execution context for ARP operations.
//! It is implemented both by the test fakes in that module, and also by the
//! Ethernet device implementation in the [`crate::device::ethernet`] module.
//!
//! This subdivision of code into small units in turn enables modularity. If,
//! for example, the IP code sees transport layer protocols as execution
//! contexts, then customizing which transport layer protocols are supported is
//! just a matter of providing a different implementation of the transport layer
//! context traits (this isn't what we do today, but we may in the future).

use lock_order::Unlocked;

use crate::{
    marker::{BindingsContext, BindingsTypes},
    state::StackState,
};

pub use netstack3_base::{
    BuildableCoreContext, ContextPair, ContextProvider, CoreEventContext, CoreTimerContext,
    CounterContext, CtxPair, DeferredResourceRemovalContext, EventContext, HandleableTimer,
    InstantBindingsTypes, InstantContext, NestedIntoCoreTimerCtx, ReceivableFrameMeta,
    RecvFrameContext, ReferenceNotifiers, ReferenceNotifiersExt, ResourceCounterContext,
    RngContext, SendFrameContext, SendableFrameMeta, TimerBindingsTypes, TimerContext,
    TimerHandler, TracingContext,
};

// Enable all blanket implementations on CoreCtx.
//
// Some blanket implementations are enabled individually to sidestep coherence
// issues with the fake context implementations in tests. We treat each of them
// individually so it's easier to split things into separate crates and avoids
// playing whack-a-mole with single markers that work for some traits/crates but
// not others.
impl<BC: BindingsContext, L> crate::ip::base::UseTransportIpContextBlanket for CoreCtx<'_, BC, L> {}
impl<BC: BindingsContext, L> crate::ip::base::UseIpSocketContextBlanket for CoreCtx<'_, BC, L> {}
impl<BC: BindingsContext, L> crate::ip::socket::UseIpSocketHandlerBlanket for CoreCtx<'_, BC, L> {}
impl<BC: BindingsContext, L> crate::ip::socket::UseDeviceIpSocketHandlerBlanket
    for CoreCtx<'_, BC, L>
{
}
impl<BC: BindingsContext, L> crate::transport::udp::UseUdpIpTransportContextBlanket
    for CoreCtx<'_, BC, L>
{
}
impl<BC: BindingsContext, L> crate::device::ethernet::UseArpFrameMetadataBlanket
    for CoreCtx<'_, BC, L>
{
}

/// Provides access to core context implementations.
///
/// `L` is the current lock level of `CoreCtx`. The alias [`UnlockedCoreCtx`] is
/// provided at the [`Unlocked`] level.
pub type CoreCtx<'a, BT, L> = Locked<&'a StackState<BT>, L>;

pub(crate) type CoreCtxAndResource<'a, BT, R, L> =
    Locked<lock_order::OwnedTupleWrapper<&'a StackState<BT>, &'a R>, L>;

/// An alias for an unlocked [`CoreCtx`].
pub type UnlockedCoreCtx<'a, BT> = CoreCtx<'a, BT, Unlocked>;

pub(crate) use locked::Locked;

impl<'a, BT, L> ContextProvider for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
{
    type Context = Self;

    fn context(&mut self) -> &mut Self::Context {
        self
    }
}

/// Provides a crate-local wrapper for `[lock_order::Locked]`.
///
/// This module is intentionally private so usage is limited to the type alias
/// in [`CoreCtx`].
mod locked {
    use super::{BindingsTypes, CoreCtx, StackState};

    use core::ops::Deref;
    use lock_order::{wrap::LockedWrapper, Locked as ExternalLocked, TupleWrapper, Unlocked};

    /// A crate-local wrapper on [`lock_order::Locked`].
    pub struct Locked<T, L>(ExternalLocked<T, L>);

    impl<T, L> LockedWrapper<T, L> for Locked<T, L>
    where
        T: Deref,
        T::Target: Sized,
    {
        type AtLockLevel<'l, M> = Locked<&'l T::Target, M>
    where
        M: 'l,
        T: 'l;

        type CastWrapper<X> = Locked<X, L>
    where
        X: Deref,
        X::Target: Sized;

        fn wrap<'l, M>(locked: ExternalLocked<&'l T::Target, M>) -> Self::AtLockLevel<'l, M>
        where
            M: 'l,
            T: 'l,
        {
            Locked(locked)
        }

        fn wrap_cast<R: Deref>(locked: ExternalLocked<R, L>) -> Self::CastWrapper<R>
        where
            R::Target: Sized,
        {
            Locked(locked)
        }

        fn get_mut(&mut self) -> &mut ExternalLocked<T, L> {
            let Self(locked) = self;
            locked
        }

        fn get(&self) -> &ExternalLocked<T, L> {
            let Self(locked) = self;
            locked
        }
    }

    impl<'a, BT: BindingsTypes> CoreCtx<'a, BT, Unlocked> {
        /// Creates a new `CoreCtx` from a borrowed [`StackState`].
        pub fn new(stack_state: &'a StackState<BT>) -> Self {
            Self(ExternalLocked::new(stack_state))
        }
    }

    impl<'a, BT, R, L, T> Locked<T, L>
    where
        R: 'a,
        T: Deref<Target = TupleWrapper<&'a StackState<BT>, &'a R>>,
        BT: BindingsTypes,
    {
        pub(crate) fn cast_resource(&mut self) -> Locked<&'_ R, L> {
            let Self(locked) = self;
            Locked(locked.cast_with(|c| c.right()))
        }

        pub(crate) fn cast_core_ctx(&mut self) -> CoreCtx<'_, BT, L> {
            let Self(locked) = self;
            crate::CoreCtx::<BT, L>::wrap(locked.cast_with(|c| c.left()))
        }
    }
}

/// Fake implementations of context traits.
#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use alloc::sync::Arc;
    use core::fmt::Debug;

    #[cfg(test)]
    use crate::filter::{FilterBindingsContext, FilterHandlerProvider};
    use crate::{
        device::link::LinkDevice,
        ip::device::nud::{LinkResolutionContext, LinkResolutionNotifier},
        sync::Mutex,
    };

    pub use netstack3_base::testutil::{
        FakeBindingsCtx, FakeCoreCtx, FakeCryptoRng, FakeEventCtx, FakeFrameCtx, FakeInstant,
        FakeInstantCtx, FakeNetwork, FakeNetworkLinks, FakeNetworkSpec, FakeTimerCtx,
        FakeTimerCtxExt, FakeTracingCtx, InstantAndData, PendingFrame, PendingFrameData,
        StepResult, WithFakeFrameContext, WithFakeTimerContext,
    };

    impl<D: LinkDevice, Id, Event: Debug, State, FrameMeta> LinkResolutionContext<D>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        type Notifier = FakeLinkResolutionNotifier<D>;
    }

    /// A fake implementation of [`LinkResolutionNotifier`].
    #[derive(Debug)]
    pub struct FakeLinkResolutionNotifier<D: LinkDevice>(
        Arc<Mutex<Option<Result<D::Address, crate::error::AddressResolutionFailed>>>>,
    );

    impl<D: LinkDevice> LinkResolutionNotifier<D> for FakeLinkResolutionNotifier<D> {
        type Observer =
            Arc<Mutex<Option<Result<D::Address, crate::error::AddressResolutionFailed>>>>;

        fn new() -> (Self, Self::Observer) {
            let inner = Arc::new(Mutex::new(None));
            (Self(inner.clone()), inner)
        }

        fn notify(self, result: Result<D::Address, crate::error::AddressResolutionFailed>) {
            let Self(inner) = self;
            let mut inner = inner.lock();
            assert_eq!(*inner, None, "resolved link address was set more than once");
            *inner = Some(result);
        }
    }

    #[cfg(test)]
    impl<I: packet_formats::ip::IpExt, BC: FilterBindingsContext, S, Meta, DeviceId>
        FilterHandlerProvider<I, BC> for FakeCoreCtx<S, Meta, DeviceId>
    {
        type Handler<'a> = crate::filter::NoopImpl where Self: 'a;

        fn filter_handler(&mut self) -> Self::Handler<'_> {
            crate::filter::NoopImpl
        }
    }
}
