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
    ContextPair, ContextProvider, CoreEventContext, CoreTimerContext, CounterContext, CtxPair,
    DeferredResourceRemovalContext, EventContext, HandleableTimer, InstantBindingsTypes,
    InstantContext, NestedIntoCoreTimerCtx, ReceivableFrameMeta, RecvFrameContext,
    ReferenceNotifiers, ResourceCounterContext, RngContext, SendFrameContext, SendableFrameMeta,
    TimerBindingsTypes, TimerContext, TimerHandler, TracingContext,
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
///
/// Each trait `Xxx` has a fake called `FakeXxx`. `FakeXxx` implements `Xxx`,
/// and `impl<T> FakeXxx for T` where either `T: AsRef<FakeXxx>` or `T:
/// AsMut<FakeXxx>` or both (depending on the trait). This allows fake
/// implementations to be composed easily - any container type need only provide
/// the appropriate `AsRef` and/or `AsMut` implementations, and the blanket impl
/// will take care of the rest.
#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use alloc::sync::Arc;
    #[cfg(test)]
    use alloc::vec::Vec;
    use core::fmt::Debug;
    #[cfg(test)]
    use core::marker::PhantomData;

    #[cfg(test)]
    use derivative::Derivative;

    #[cfg(test)]
    use crate::{
        context::{ContextProvider, CounterContext},
        filter::{FilterBindingsTypes, FilterHandlerProvider},
    };
    use crate::{
        device::link::LinkDevice,
        ip::device::nud::{LinkResolutionContext, LinkResolutionNotifier},
        sync::Mutex,
    };

    pub use netstack3_base::testutil::{
        FakeBindingsCtx, FakeCryptoRng, FakeEventCtx, FakeFrameCtx, FakeInstant, FakeInstantCtx,
        FakeNetwork, FakeNetworkContext, FakeNetworkLinks, FakeTimerCtx, FakeTimerCtxExt,
        FakeTracingCtx, InstantAndData, PendingFrame, PendingFrameData, StepResult,
        WithFakeFrameContext, WithFakeTimerContext,
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
    impl<CC, TimerId, Event: Debug, State> WithFakeTimerContext<TimerId>
        for FakeCtxWithCoreCtx<CC, TimerId, Event, State>
    {
        fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId>) -> O>(&self, f: F) -> O {
            let Self { core_ctx: _, bindings_ctx } = self;
            f(&bindings_ctx.timers)
        }

        fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            let Self { core_ctx: _, bindings_ctx } = self;
            f(&mut bindings_ctx.timers)
        }
    }

    #[cfg(test)]
    pub(crate) type FakeCtxWithCoreCtx<CC, TimerId, Event, BindingsCtxState> =
        crate::testutil::ContextPair<CC, FakeBindingsCtx<TimerId, Event, BindingsCtxState, ()>>;

    #[cfg(test)]
    pub(crate) type FakeCtx<S, TimerId, Meta, Event, DeviceId, BindingsCtxState> =
        FakeCtxWithCoreCtx<FakeCoreCtx<S, Meta, DeviceId>, TimerId, Event, BindingsCtxState>;

    #[cfg(test)]
    impl<CC, Id, Event: Debug, BindingsCtxState> AsRef<FakeInstantCtx>
        for FakeCtxWithCoreCtx<CC, Id, Event, BindingsCtxState>
    {
        fn as_ref(&self) -> &FakeInstantCtx {
            self.bindings_ctx.timers.as_ref()
        }
    }

    #[cfg(test)]
    impl<CC, Id, Event: Debug, BindingsCtxState> AsRef<FakeTimerCtx<Id>>
        for FakeCtxWithCoreCtx<CC, Id, Event, BindingsCtxState>
    {
        fn as_ref(&self) -> &FakeTimerCtx<Id> {
            &self.bindings_ctx.timers
        }
    }

    #[cfg(test)]
    impl<CC, Id, Event: Debug, BindingsCtxState> AsMut<FakeTimerCtx<Id>>
        for FakeCtxWithCoreCtx<CC, Id, Event, BindingsCtxState>
    {
        fn as_mut(&mut self) -> &mut FakeTimerCtx<Id> {
            &mut self.bindings_ctx.timers
        }
    }

    #[cfg(test)]
    impl<S, Id, Meta, Event: Debug, DeviceId, BindingsCtxState> AsMut<FakeFrameCtx<Meta>>
        for FakeCtx<S, Id, Meta, Event, DeviceId, BindingsCtxState>
    {
        fn as_mut(&mut self) -> &mut FakeFrameCtx<Meta> {
            &mut self.core_ctx.frames
        }
    }

    #[cfg(test)]
    impl<S, Id, Meta, Event: Debug, DeviceId, BindingsCtxState> WithFakeFrameContext<Meta>
        for FakeCtx<S, Id, Meta, Event, DeviceId, BindingsCtxState>
    {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<Meta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            f(&mut self.core_ctx.frames)
        }
    }

    #[cfg(test)]
    #[derive(Default)]
    pub(crate) struct Wrapped<Outer, Inner> {
        pub(crate) inner: Inner,
        pub(crate) outer: Outer,
    }

    #[cfg(test)]
    impl<Outer, Inner> ContextProvider for Wrapped<Outer, Inner> {
        type Context = Self;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    #[cfg(test)]
    pub(crate) type WrappedFakeCoreCtx<Outer, S, Meta, DeviceId> =
        Wrapped<Outer, FakeCoreCtx<S, Meta, DeviceId>>;

    #[cfg(test)]
    impl<Outer, S, Meta, DeviceId> WrappedFakeCoreCtx<Outer, S, Meta, DeviceId> {
        pub(crate) fn with_inner_and_outer_state(inner: S, outer: Outer) -> Self {
            Self { inner: FakeCoreCtx::with_state(inner), outer }
        }
    }

    #[cfg(test)]
    impl<Outer, T, Inner: AsRef<T>> AsRef<T> for Wrapped<Outer, Inner> {
        fn as_ref(&self) -> &T {
            self.inner.as_ref()
        }
    }

    #[cfg(test)]
    impl<Outer, T, Inner: AsMut<T>> AsMut<T> for Wrapped<Outer, Inner> {
        fn as_mut(&mut self) -> &mut T {
            self.inner.as_mut()
        }
    }

    /// A test helper used to provide an implementation of a core context.
    #[cfg(test)]
    #[derive(Derivative)]
    #[derivative(Default(bound = "S: Default"))]
    pub(crate) struct FakeCoreCtx<S, Meta, DeviceId> {
        pub(crate) state: S,
        pub(crate) frames: FakeFrameCtx<Meta>,
        _devices_marker: PhantomData<DeviceId>,
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> ContextProvider for FakeCoreCtx<S, Meta, DeviceId> {
        type Context = Self;

        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> AsRef<FakeCoreCtx<S, Meta, DeviceId>> for FakeCoreCtx<S, Meta, DeviceId> {
        fn as_ref(&self) -> &FakeCoreCtx<S, Meta, DeviceId> {
            self
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> AsMut<FakeCoreCtx<S, Meta, DeviceId>> for FakeCoreCtx<S, Meta, DeviceId> {
        fn as_mut(&mut self) -> &mut FakeCoreCtx<S, Meta, DeviceId> {
            self
        }
    }

    #[cfg(test)]
    impl<I: packet_formats::ip::IpExt, BC: FilterBindingsTypes, S, Meta, DeviceId>
        FilterHandlerProvider<I, BC> for FakeCoreCtx<S, Meta, DeviceId>
    {
        type Handler<'a> = crate::filter::NoopImpl where Self: 'a;

        fn filter_handler(&mut self) -> Self::Handler<'_> {
            crate::filter::NoopImpl
        }
    }

    #[cfg(test)]
    impl<Outer, I: packet_formats::ip::IpExt, BC: FilterBindingsTypes, S, Meta, DeviceId>
        FilterHandlerProvider<I, BC> for Wrapped<Outer, FakeCoreCtx<S, Meta, DeviceId>>
    {
        type Handler<'a> = crate::filter::NoopImpl where Self: 'a;

        fn filter_handler(&mut self) -> Self::Handler<'_> {
            crate::filter::NoopImpl
        }
    }

    #[cfg(test)]
    impl<BC, S, Meta, DeviceId> CounterContext<BC> for FakeCoreCtx<S, Meta, DeviceId>
    where
        S: CounterContext<BC>,
    {
        fn with_counters<O, F: FnOnce(&BC) -> O>(&self, cb: F) -> O {
            CounterContext::<BC>::with_counters(&self.state, cb)
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> FakeCoreCtx<S, Meta, DeviceId> {
        /// Constructs a `FakeCoreCtx` with the given state and default
        /// `FakeTimerCtx`, and `FakeFrameCtx`.
        pub(crate) fn with_state(state: S) -> Self {
            FakeCoreCtx { state, frames: FakeFrameCtx::default(), _devices_marker: PhantomData }
        }

        /// Get an immutable reference to the inner state.
        ///
        /// This method is provided instead of an [`AsRef`] impl to avoid
        /// conflicting with user-provided implementations of `AsRef<T> for
        /// FakeCtx<S, Id, Meta, Event>` for other types, `T`. It is named
        /// `get_ref` instead of `as_ref` so that programmer doesn't need to
        /// specify which `as_ref` method is intended.
        pub(crate) fn get_ref(&self) -> &S {
            &self.state
        }

        /// Get a mutable reference to the inner state.
        ///
        /// `get_mut` is like `get_ref`, but it returns a mutable reference.
        pub(crate) fn get_mut(&mut self) -> &mut S {
            &mut self.state
        }

        /// Get the list of frames sent so far.
        pub(crate) fn frames(&self) -> &[(Meta, Vec<u8>)] {
            self.frames.frames()
        }

        /// Take the list of frames sent so far.
        pub(crate) fn take_frames(&mut self) -> Vec<(Meta, Vec<u8>)> {
            self.frames.take_frames()
        }

        /// Consumes the `FakeCoreCtx` and returns the inner state.
        pub(crate) fn into_state(self) -> S {
            self.state
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> AsMut<FakeFrameCtx<Meta>> for FakeCoreCtx<S, Meta, DeviceId> {
        fn as_mut(&mut self) -> &mut FakeFrameCtx<Meta> {
            &mut self.frames
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> WithFakeFrameContext<Meta> for FakeCoreCtx<S, Meta, DeviceId> {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<Meta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            f(&mut self.frames)
        }
    }

    #[cfg(test)]
    impl<Outer, Inner: WithFakeFrameContext<Meta>, Meta> WithFakeFrameContext<Meta>
        for Wrapped<Outer, Inner>
    {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<Meta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            self.inner.with_fake_frame_ctx_mut(f)
        }
    }
}
