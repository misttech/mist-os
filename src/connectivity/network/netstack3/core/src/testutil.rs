// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Testing-related utilities.

#[cfg(test)]
use alloc::vec;
use alloc::{borrow::ToOwned, collections::HashMap, sync::Arc, vec::Vec};
#[cfg(test)]
use core::time::Duration;
use core::{
    convert::Infallible as Never,
    ffi::CStr,
    fmt::{self, Debug, Display},
    ops::{Deref, DerefMut},
    sync::atomic::AtomicUsize,
};

use net_types::{
    ethernet::Mac,
    ip::{
        AddrSubnetEither, GenericOverIp, Ip, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6,
        Ipv6Addr, Subnet, SubnetEither,
    },
    SpecifiedAddr, UnicastAddr,
};
#[cfg(test)]
use net_types::{ip::IpAddr, MulticastAddr, Witness as _};
use packet::{Buf, BufferMut};
#[cfg(test)]
use packet_formats::ip::IpProto;
#[cfg(test)]
use rand::Rng as _;
use rand::{self, CryptoRng, RngCore, SeedableRng};
use rand_xorshift::XorShiftRng;
#[cfg(test)]
use tracing::Subscriber;
#[cfg(test)]
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields},
        FmtContext,
    },
    registry::LookupSpan,
};

#[cfg(test)]
use crate::{context::testutil::InstantAndData, ip::SendIpPacketMeta};
use crate::{
    context::{
        testutil::{
            FakeFrameCtx, FakeInstant, FakeNetworkContext, FakeTimerCtx, WithFakeFrameContext,
            WithFakeTimerContext,
        },
        EventContext, InstantBindingsTypes, InstantContext, RngContext, TimerContext,
        TracingContext,
    },
    device::{
        ethernet::MaxEthernetFrameSize, link::LinkDevice, loopback::LoopbackDeviceId, DeviceId,
        DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceSendFrameError, EthernetDeviceId,
        EthernetWeakDeviceId, WeakDeviceId,
    },
    ip::{
        device::{
            nud::{self, LinkResolutionContext, LinkResolutionNotifier},
            IpDeviceEvent, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
        },
        icmp::{IcmpEchoBindingsContext, IcmpIpExt},
        types::{AddableEntry, AddableMetric, RawMetric},
        IpLayerEvent,
    },
    state::StackStateBuilder,
    sync::Mutex,
    time::TimerId,
    transport::{
        tcp::{
            buffer::{
                testutil::{ClientBuffers, ProvidedBuffers, TestSendBuffer},
                RingBuffer,
            },
            socket::TcpBindingsTypes,
            BufferSizes,
        },
        udp::{self, UdpBindingsContext},
    },
    SyncCtx,
};

/// NDP test utilities.
pub mod ndp {
    pub use crate::device::ndp::testutil::*;
}
/// Context test utilities.
pub mod context {
    pub use crate::context::testutil::*;
}

/// The default interface routing metric for test interfaces.
pub(crate) const DEFAULT_INTERFACE_METRIC: RawMetric = RawMetric(100);

/// A structure holding a core and a bindings context.
#[derive(Default)]
pub struct ContextPair<CC, BT> {
    /// The core context.
    pub core_ctx: CC,
    /// The bindings context.
    // We put `bindings_ctx` after `core_ctx` to make sure that `core_ctx` is
    // dropped before `bindings_ctx` so that the existence of strongly-referenced
    // device IDs in `bindings_ctx` causes test failures, forcing proper cleanup
    // of device IDs in our unit tests.
    //
    // Note that if strongly-referenced (device) IDs exist when dropping the
    // primary reference, the primary reference's drop impl will panic. See
    // `crate::sync::PrimaryRc::drop` for details.
    pub bindings_ctx: BT,
}

impl<CC, BC> ContextPair<CC, BC> {
    #[cfg(test)]
    pub(crate) fn with_core_ctx(core_ctx: CC) -> Self
    where
        BC: Default,
    {
        Self { core_ctx, bindings_ctx: BC::default() }
    }
}

/// Context available during the execution of the netstack.
pub type Ctx<BT> = ContextPair<SyncCtx<BT>, BT>;

impl<BC: crate::BindingsContext + Default> Default for Ctx<BC> {
    fn default() -> Self {
        Self::new_with_builder(StackStateBuilder::default())
    }
}

impl<BC: crate::BindingsContext + Default> Ctx<BC> {
    pub(crate) fn new_with_builder(builder: StackStateBuilder) -> Self {
        let mut bindings_ctx = Default::default();
        let state = builder.build_with_ctx(&mut bindings_ctx);
        Self { core_ctx: SyncCtx { state }, bindings_ctx }
    }
}

/// Asserts that an iterable object produces zero items.
///
/// `assert_empty` drains `into_iter.into_iter()` and asserts that zero
/// items are produced. It panics with a message which includes the produced
/// items if this assertion fails.
#[cfg(test)]
#[track_caller]
pub(crate) fn assert_empty<I: IntoIterator>(into_iter: I)
where
    I::Item: Debug,
{
    // NOTE: Collecting into a `Vec` is cheap in the happy path because
    // zero-capacity vectors are guaranteed not to allocate.
    let vec = into_iter.into_iter().collect::<Vec<_>>();
    assert!(vec.is_empty(), "vec={vec:?}");
}

/// Utilities to allow running benchmarks as tests.
///
/// Our benchmarks rely on the unstable `test` feature, which is disallowed in
/// Fuchsia's build system. In order to ensure that our benchmarks are always
/// compiled and tested, this module provides fakes that allow us to run our
/// benchmarks as normal tests when the `benchmark` feature is disabled.
///
/// See the `bench!` macro for details on how this module is used.
#[cfg(test)]
pub(crate) mod benchmarks {
    /// A trait to allow faking of the `test::Bencher` type.
    pub(crate) trait Bencher {
        fn iter<T, F: FnMut() -> T>(&mut self, inner: F);
    }

    #[cfg(benchmark)]
    impl Bencher for criterion::Bencher {
        fn iter<T, F: FnMut() -> T>(&mut self, inner: F) {
            criterion::Bencher::iter(self, inner)
        }
    }

    /// A `Bencher` whose `iter` method runs the provided argument a small,
    /// fixed number of times.
    #[cfg(not(benchmark))]
    pub(crate) struct TestBencher;

    #[cfg(not(benchmark))]
    impl Bencher for TestBencher {
        fn iter<T, F: FnMut() -> T>(&mut self, mut inner: F) {
            const NUM_TEST_ITERS: u32 = 256;
            super::set_logger_for_test();
            for _ in 0..NUM_TEST_ITERS {
                let _: T = inner();
            }
        }
    }

    #[inline(always)]
    pub(crate) fn black_box<T>(placeholder: T) -> T {
        #[cfg(benchmark)]
        return criterion::black_box(placeholder);
        #[cfg(not(benchmark))]
        return placeholder;
    }
}

#[derive(Default)]
/// Bindings context state held by [`FakeBindingsCtx`].
pub struct FakeBindingsCtxState {
    icmpv4_replies: HashMap<crate::ip::icmp::SocketId<Ipv4>, Vec<Vec<u8>>>,
    icmpv6_replies: HashMap<crate::ip::icmp::SocketId<Ipv6>, Vec<Vec<u8>>>,
    udpv4_received: HashMap<crate::transport::udp::SocketId<Ipv4>, Vec<Vec<u8>>>,
    udpv6_received: HashMap<crate::transport::udp::SocketId<Ipv6>, Vec<Vec<u8>>>,
    pub(crate) rx_available: Vec<LoopbackDeviceId<FakeBindingsCtx>>,
    pub(crate) tx_available: Vec<DeviceId<FakeBindingsCtx>>,
}

impl FakeBindingsCtxState {
    pub(crate) fn udp_state_mut<I: Ip>(
        &mut self,
    ) -> &mut HashMap<crate::transport::udp::SocketId<I>, Vec<Vec<u8>>> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<'a, I: Ip>(
            &'a mut HashMap<crate::transport::udp::SocketId<I>, Vec<Vec<u8>>>,
        );
        let Wrapper(map) = I::map_ip::<_, Wrapper<'_, I>>(
            IpInvariant(self),
            |IpInvariant(this)| Wrapper(&mut this.udpv4_received),
            |IpInvariant(this)| Wrapper(&mut this.udpv6_received),
        );
        map
    }
}

/// Shorthand for [`Ctx`] with a [`FakeBindingsCtx`].
pub type FakeCtx = Ctx<FakeBindingsCtx>;
/// Shorthand for [`SyncCtx`] that uses a [`FakeBindingsCtx`].
pub type FakeCoreCtx = SyncCtx<FakeBindingsCtx>;

/// Test-only implementation of [`crate::BindingsContext`].
#[derive(Default, Clone)]
pub struct FakeBindingsCtx(
    Arc<
        Mutex<
            crate::context::testutil::FakeBindingsCtx<
                TimerId<Self>,
                DispatchedEvent,
                FakeBindingsCtxState,
            >,
        >,
    >,
);

/// A wrapper type that makes it easier to implement `Deref` (and optionally
/// `DerefMut`) for a value that is protected by a lock.
///
/// The first field is the type that provides access to the inner value,
/// probably a lock guard. The second and third fields are functions that, given
/// the first field, provide shared and mutable access (respectively) to the
/// inner value.
struct Wrapper<S, Callback, CallbackMut>(S, Callback, CallbackMut);

impl<T: ?Sized, S: Deref, Callback: for<'a> Fn(&'a <S as Deref>::Target) -> &'a T, CallbackMut>
    Deref for Wrapper<S, Callback, CallbackMut>
{
    type Target = T;

    fn deref(&self) -> &T {
        let Self(guard, f, _) = self;
        let target = guard.deref();
        f(target)
    }
}

impl<
        T: ?Sized,
        S: DerefMut,
        Callback: for<'a> Fn(&'a <S as Deref>::Target) -> &'a T,
        CallbackMut: for<'a> Fn(&'a mut <S as Deref>::Target) -> &'a mut T,
    > DerefMut for Wrapper<S, Callback, CallbackMut>
{
    fn deref_mut(&mut self) -> &mut T {
        let Self(guard, _, f) = self;
        let target = guard.deref_mut();
        f(target)
    }
}

impl FakeBindingsCtx {
    fn with_inner<
        F: FnOnce(
            &crate::context::testutil::FakeBindingsCtx<
                TimerId<Self>,
                DispatchedEvent,
                FakeBindingsCtxState,
            >,
        ) -> O,
        O,
    >(
        &self,
        f: F,
    ) -> O {
        let Self(this) = self;
        let locked = this.lock();
        f(&*locked)
    }

    fn with_inner_mut<
        F: FnOnce(
            &mut crate::context::testutil::FakeBindingsCtx<
                TimerId<Self>,
                DispatchedEvent,
                FakeBindingsCtxState,
            >,
        ) -> O,
        O,
    >(
        &self,
        f: F,
    ) -> O {
        let Self(this) = self;
        let mut locked = this.lock();
        f(&mut *locked)
    }

    #[cfg(test)]
    pub(crate) fn timer_ctx(&self) -> impl Deref<Target = FakeTimerCtx<TimerId<Self>>> + '_ {
        Wrapper(self.0.lock(), crate::context::testutil::FakeBindingsCtx::timer_ctx, ())
    }

    #[cfg(test)]
    pub(crate) fn frames_sent(
        &self,
    ) -> impl Deref<Target = [(EthernetWeakDeviceId<FakeBindingsCtx>, Vec<u8>)]> + '_ {
        Wrapper(self.0.lock(), crate::context::testutil::FakeBindingsCtx::frames, ())
    }

    pub(crate) fn state_mut(&mut self) -> impl DerefMut<Target = FakeBindingsCtxState> + '_ {
        Wrapper(
            self.0.lock(),
            crate::context::testutil::FakeBindingsCtx::state,
            crate::context::testutil::FakeBindingsCtx::state_mut,
        )
    }

    /// Take all frames sent so far.
    pub fn take_frames(&mut self) -> Vec<(EthernetWeakDeviceId<FakeBindingsCtx>, Vec<u8>)> {
        self.with_inner_mut(|ctx| ctx.frame_ctx_mut().take_frames())
    }

    #[cfg(test)]
    pub(crate) fn take_events(&mut self) -> Vec<DispatchedEvent> {
        self.with_inner_mut(|ctx| ctx.events.take())
    }

    /// Takes all the received ICMP replies for a given `conn`.
    #[cfg(test)]
    pub(crate) fn take_icmp_replies<I: Ip>(
        &mut self,
        conn: crate::ip::icmp::SocketId<I>,
    ) -> Vec<Vec<u8>> {
        I::map_ip::<_, IpInvariant<Option<Vec<_>>>>(
            (IpInvariant(self), conn),
            |(IpInvariant(this), conn)| IpInvariant(this.state_mut().icmpv4_replies.remove(&conn)),
            |(IpInvariant(this), conn)| IpInvariant(this.state_mut().icmpv6_replies.remove(&conn)),
        )
        .into_inner()
        .unwrap_or_else(Vec::default)
    }

    #[cfg(test)]
    pub(crate) fn take_udp_received<I: Ip>(
        &mut self,
        conn: crate::transport::udp::SocketId<I>,
    ) -> Vec<Vec<u8>> {
        self.state_mut().udp_state_mut::<I>().remove(&conn).unwrap_or_else(Vec::default)
    }
}

impl WithFakeTimerContext<TimerId<FakeBindingsCtx>> for FakeCtx {
    fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId<FakeBindingsCtx>>) -> O>(
        &self,
        f: F,
    ) -> O {
        let Self { core_ctx: _, bindings_ctx } = self;
        bindings_ctx.with_inner(|ctx| f(&ctx.timers))
    }

    fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId<FakeBindingsCtx>>) -> O>(
        &mut self,
        f: F,
    ) -> O {
        let Self { core_ctx: _, bindings_ctx } = self;
        bindings_ctx.with_inner_mut(|ctx| f(&mut ctx.timers))
    }
}

impl WithFakeFrameContext<EthernetWeakDeviceId<crate::testutil::FakeBindingsCtx>> for FakeCtx {
    fn with_fake_frame_ctx_mut<
        O,
        F: FnOnce(&mut FakeFrameCtx<EthernetWeakDeviceId<crate::testutil::FakeBindingsCtx>>) -> O,
    >(
        &mut self,
        f: F,
    ) -> O {
        let Self { core_ctx: _, bindings_ctx } = self;
        bindings_ctx.with_inner_mut(|ctx| f(&mut ctx.frames))
    }
}

impl WithFakeTimerContext<TimerId<FakeBindingsCtx>> for FakeBindingsCtx {
    fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId<FakeBindingsCtx>>) -> O>(
        &self,
        f: F,
    ) -> O {
        self.with_inner(|ctx| f(&ctx.timers))
    }

    fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId<FakeBindingsCtx>>) -> O>(
        &mut self,
        f: F,
    ) -> O {
        self.with_inner_mut(|ctx| f(&mut ctx.timers))
    }
}

impl InstantBindingsTypes for FakeBindingsCtx {
    type Instant = FakeInstant;
}

impl InstantContext for FakeBindingsCtx {
    fn now(&self) -> FakeInstant {
        self.with_inner(|ctx| ctx.now())
    }
}

impl TimerContext<TimerId<FakeBindingsCtx>> for FakeBindingsCtx {
    fn schedule_timer_instant(
        &mut self,
        time: FakeInstant,
        id: TimerId<FakeBindingsCtx>,
    ) -> Option<FakeInstant> {
        self.with_inner_mut(|ctx| ctx.schedule_timer_instant(time, id))
    }

    fn cancel_timer(&mut self, id: TimerId<FakeBindingsCtx>) -> Option<FakeInstant> {
        self.with_inner_mut(|ctx| ctx.cancel_timer(id))
    }

    fn cancel_timers_with<F: FnMut(&TimerId<FakeBindingsCtx>) -> bool>(&mut self, f: F) {
        self.with_inner_mut(|ctx| ctx.cancel_timers_with(f))
    }

    fn scheduled_instant(&self, id: TimerId<FakeBindingsCtx>) -> Option<FakeInstant> {
        self.with_inner_mut(|ctx| ctx.scheduled_instant(id))
    }
}

impl RngContext for FakeBindingsCtx {
    type Rng<'a> = FakeCryptoRng<XorShiftRng>;

    fn rng(&mut self) -> Self::Rng<'_> {
        let Self(this) = self;
        this.lock().rng()
    }
}

impl<T: Into<DispatchedEvent>> EventContext<T> for FakeBindingsCtx {
    fn on_event(&mut self, event: T) {
        self.with_inner_mut(|ctx| ctx.events.on_event(event.into()))
    }
}

impl TracingContext for FakeBindingsCtx {
    type DurationScope = ();

    fn duration(&self, _: &'static CStr) {}
}

impl TcpBindingsTypes for FakeBindingsCtx {
    type ReceiveBuffer = Arc<Mutex<RingBuffer>>;

    type SendBuffer = TestSendBuffer;

    type ReturnedBuffers = ClientBuffers;

    type ListenerNotifierOrProvidedBuffers = ProvidedBuffers;

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers) {
        let client = ClientBuffers::new(buffer_sizes);
        (
            Arc::clone(&client.receive),
            TestSendBuffer::new(Arc::clone(&client.send), RingBuffer::default()),
            client,
        )
    }

    fn default_buffer_sizes() -> BufferSizes {
        // Use the test-only default impl.
        BufferSizes::default()
    }
}

impl crate::ReferenceNotifiers for FakeBindingsCtx {
    type ReferenceReceiver<T: 'static> = Never;

    type ReferenceNotifier<T: Send + 'static> = Never;

    fn new_reference_notifier<T: Send + 'static, D: Debug>(
        debug_references: D,
    ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>) {
        // NB: We don't want deferred destruction in core tests. These are
        // always single-threaded and single-task, and we want to encourage
        // explicit cleanup.
        panic!(
            "FakeBindingsCtx can't create deferred reference notifiers for type {}: \
            debug_references={debug_references:?}",
            core::any::type_name::<T>()
        );
    }
}

impl<D: LinkDevice> LinkResolutionContext<D> for FakeBindingsCtx {
    type Notifier = ();
}

impl<D: LinkDevice> LinkResolutionNotifier<D> for () {
    type Observer = ();

    fn new() -> (Self, Self::Observer) {
        ((), ())
    }

    fn notify(self, _result: Result<D::Address, crate::error::AddressResolutionFailed>) {}
}

/// A wrapper which implements `RngCore` and `CryptoRng` for any `RngCore`.
///
/// This is used to satisfy [`EventDispatcher`]'s requirement that the
/// associated `Rng` type implements `CryptoRng`.
///
/// # Security
///
/// This is obviously insecure. Don't use it except in testing!
#[derive(Clone, Debug)]
pub struct FakeCryptoRng<R>(Arc<Mutex<R>>);

impl Default for FakeCryptoRng<XorShiftRng> {
    fn default() -> FakeCryptoRng<XorShiftRng> {
        FakeCryptoRng::new_xorshift(12957992561116578403)
    }
}

impl FakeCryptoRng<XorShiftRng> {
    /// Creates a new [`FakeCryptoRng<XorShiftRng>`] from a seed.
    pub(crate) fn new_xorshift(seed: u128) -> FakeCryptoRng<XorShiftRng> {
        Self(Arc::new(Mutex::new(new_rng(seed))))
    }

    #[cfg(test)]
    pub(crate) fn deep_clone(&self) -> Self {
        Self(Arc::new(Mutex::new(self.0.lock().clone())))
    }
}

impl<R: RngCore> RngCore for FakeCryptoRng<R> {
    fn next_u32(&mut self) -> u32 {
        self.0.lock().next_u32()
    }
    fn next_u64(&mut self) -> u64 {
        self.0.lock().next_u64()
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.lock().fill_bytes(dest)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.0.lock().try_fill_bytes(dest)
    }
}

impl<R: RngCore> CryptoRng for FakeCryptoRng<R> {}

impl<R: SeedableRng> SeedableRng for FakeCryptoRng<R> {
    type Seed = R::Seed;

    fn from_seed(seed: Self::Seed) -> Self {
        Self(Arc::new(Mutex::new(R::from_seed(seed))))
    }
}

impl<R: RngCore> crate::context::RngContext for FakeCryptoRng<R> {
    type Rng<'a> = &'a mut Self where Self: 'a;

    fn rng(&mut self) -> Self::Rng<'_> {
        self
    }
}

/// Create a new deterministic RNG from a seed.
pub(crate) fn new_rng(mut seed: u128) -> XorShiftRng {
    if seed == 0 {
        // XorShiftRng can't take 0 seeds
        seed = 1;
    }
    XorShiftRng::from_seed(seed.to_ne_bytes())
}

/// Creates `iterations` fake RNGs.
///
/// `with_fake_rngs` will create `iterations` different [`FakeCryptoRng`]s and
/// call the function `f` for each one of them.
///
/// This function can be used for tests that weed out weirdness that can
/// happen with certain random number sequences.
#[cfg(test)]
pub(crate) fn with_fake_rngs<F: Fn(FakeCryptoRng<XorShiftRng>)>(iterations: u128, f: F) {
    for seed in 0..iterations {
        f(FakeCryptoRng::new_xorshift(seed))
    }
}

/// Invokes a function multiple times with different RNG seeds.
#[cfg(test)]
pub(crate) fn run_with_many_seeds<F: FnMut(u128)>(mut f: F) {
    // Arbitrary seed.
    let mut rng = new_rng(0x0fe50fae6c37593d71944697f1245847);
    for _ in 0..64 {
        f(rng.gen());
    }
}

#[cfg(test)]
struct SimpleFormatter;

#[cfg(test)]
impl<S, N> FormatEvent<S, N> for SimpleFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Install a logger for tests.
///
/// Call this method at the beginning of the test for which logging is desired.
/// This function sets global program state, so all tests that run after this
/// function is called will use the logger.
#[cfg(test)]
pub(crate) fn set_logger_for_test() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .event_format(SimpleFormatter)
            .with_max_level(tracing::Level::TRACE)
            .with_test_writer()
            .finish(),
    )
    .unwrap_or({
        // Ignore errors caused by some other test invocation having already set
        // the global default subscriber.
    })
}

/// An extension trait for `Ip` providing test-related functionality.
#[cfg(test)]
pub(crate) trait TestIpExt:
    crate::ip::IpExt + crate::ip::IpLayerIpExt + crate::ip::device::IpDeviceIpExt
{
    /// Either [`FAKE_CONFIG_V4`] or [`FAKE_CONFIG_V6`].
    const FAKE_CONFIG: FakeEventDispatcherConfig<Self::Addr>;

    /// Get an IP address in the same subnet as `Self::FAKE_CONFIG`.
    ///
    /// `last` is the value to be put in the last octet of the IP address.
    fn get_other_ip_address(last: u8) -> SpecifiedAddr<Self::Addr>;

    /// Get an IP address in a different subnet from `Self::FAKE_CONFIG`.
    ///
    /// `last` is the value to be put in the last octet of the IP address.
    fn get_other_remote_ip_address(last: u8) -> SpecifiedAddr<Self::Addr>;

    /// Get a multicast IP address.
    ///
    /// `last` is the value to be put in the last octet of the IP address.
    fn get_multicast_addr(last: u8) -> MulticastAddr<Self::Addr>;
}

#[cfg(test)]
impl TestIpExt for Ipv4 {
    const FAKE_CONFIG: FakeEventDispatcherConfig<Ipv4Addr> = FAKE_CONFIG_V4;

    fn get_other_ip_address(last: u8) -> SpecifiedAddr<Ipv4Addr> {
        let mut bytes = Self::FAKE_CONFIG.local_ip.get().ipv4_bytes();
        bytes[bytes.len() - 1] = last;
        SpecifiedAddr::new(Ipv4Addr::new(bytes)).unwrap()
    }

    fn get_other_remote_ip_address(last: u8) -> SpecifiedAddr<Self::Addr> {
        let mut bytes = Self::FAKE_CONFIG.local_ip.get().ipv4_bytes();
        bytes[bytes.len() - 3] += 1;
        bytes[bytes.len() - 1] = last;
        SpecifiedAddr::new(Ipv4Addr::new(bytes)).unwrap()
    }

    fn get_multicast_addr(last: u8) -> MulticastAddr<Self::Addr> {
        assert!(u32::from(Self::Addr::BYTES * 8 - Self::MULTICAST_SUBNET.prefix()) > u8::BITS);
        let mut bytes = Self::MULTICAST_SUBNET.network().ipv4_bytes();
        bytes[bytes.len() - 1] = last;
        MulticastAddr::new(Ipv4Addr::new(bytes)).unwrap()
    }
}

#[cfg(test)]
impl TestIpExt for Ipv6 {
    const FAKE_CONFIG: FakeEventDispatcherConfig<Ipv6Addr> = FAKE_CONFIG_V6;

    fn get_other_ip_address(last: u8) -> SpecifiedAddr<Ipv6Addr> {
        let mut bytes = Self::FAKE_CONFIG.local_ip.get().ipv6_bytes();
        bytes[bytes.len() - 1] = last;
        SpecifiedAddr::new(Ipv6Addr::from(bytes)).unwrap()
    }

    fn get_other_remote_ip_address(last: u8) -> SpecifiedAddr<Self::Addr> {
        let mut bytes = Self::FAKE_CONFIG.local_ip.get().ipv6_bytes();
        bytes[bytes.len() - 3] += 1;
        bytes[bytes.len() - 1] = last;
        SpecifiedAddr::new(Ipv6Addr::from(bytes)).unwrap()
    }

    fn get_multicast_addr(last: u8) -> MulticastAddr<Self::Addr> {
        assert!((Self::Addr::BYTES * 8 - Self::MULTICAST_SUBNET.prefix()) as u32 > u8::BITS);
        let mut bytes = Self::MULTICAST_SUBNET.network().ipv6_bytes();
        bytes[bytes.len() - 1] = last;
        MulticastAddr::new(Ipv6Addr::from_bytes(bytes)).unwrap()
    }
}

/// A configuration for a simple network.
///
/// `FakeEventDispatcherConfig` describes a simple network with two IP hosts
/// - one remote and one local - both on the same Ethernet network.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct FakeEventDispatcherConfig<A: IpAddress> {
    /// The subnet of the local Ethernet network.
    pub(crate) subnet: Subnet<A>,
    /// The IP address of our interface to the local network (must be in
    /// subnet).
    pub(crate) local_ip: SpecifiedAddr<A>,
    /// The MAC address of our interface to the local network.
    pub(crate) local_mac: UnicastAddr<Mac>,
    /// The remote host's IP address (must be in subnet if provided).
    pub(crate) remote_ip: SpecifiedAddr<A>,
    /// The remote host's MAC address.
    pub(crate) remote_mac: UnicastAddr<Mac>,
}

/// A `FakeEventDispatcherConfig` with reasonable values for an IPv4 network.
#[cfg(test)]
pub(crate) const FAKE_CONFIG_V4: FakeEventDispatcherConfig<Ipv4Addr> = unsafe {
    FakeEventDispatcherConfig {
        subnet: Subnet::new_unchecked(Ipv4Addr::new([192, 168, 0, 0]), 16),
        local_ip: SpecifiedAddr::new_unchecked(Ipv4Addr::new([192, 168, 0, 1])),
        local_mac: UnicastAddr::new_unchecked(Mac::new([0, 1, 2, 3, 4, 5])),
        remote_ip: SpecifiedAddr::new_unchecked(Ipv4Addr::new([192, 168, 0, 2])),
        remote_mac: UnicastAddr::new_unchecked(Mac::new([6, 7, 8, 9, 10, 11])),
    }
};

/// A `FakeEventDispatcherConfig` with reasonable values for an IPv6 network.
#[cfg(test)]
pub(crate) const FAKE_CONFIG_V6: FakeEventDispatcherConfig<Ipv6Addr> = unsafe {
    FakeEventDispatcherConfig {
        subnet: Subnet::new_unchecked(
            Ipv6Addr::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 0]),
            112,
        ),
        local_ip: SpecifiedAddr::new_unchecked(Ipv6Addr::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 1,
        ])),
        local_mac: UnicastAddr::new_unchecked(Mac::new([0, 1, 2, 3, 4, 5])),
        remote_ip: SpecifiedAddr::new_unchecked(Ipv6Addr::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 2,
        ])),
        remote_mac: UnicastAddr::new_unchecked(Mac::new([6, 7, 8, 9, 10, 11])),
    }
};

#[cfg(test)]
impl<A: IpAddress> FakeEventDispatcherConfig<A> {
    /// Creates a copy of `self` with all the remote and local fields reversed.
    pub(crate) fn swap(&self) -> Self {
        Self {
            subnet: self.subnet,
            local_ip: self.remote_ip,
            local_mac: self.remote_mac,
            remote_ip: self.local_ip,
            remote_mac: self.local_mac,
        }
    }

    /// Shorthand for `FakeEventDispatcherBuilder::from_config(self)`.
    pub(crate) fn into_builder(self) -> FakeEventDispatcherBuilder {
        FakeEventDispatcherBuilder::from_config(self)
    }
}

#[derive(Clone)]
struct DeviceConfig {
    mac: UnicastAddr<Mac>,
    addr_subnet: Option<AddrSubnetEither>,
    ipv4_config: Option<Ipv4DeviceConfigurationUpdate>,
    ipv6_config: Option<Ipv6DeviceConfigurationUpdate>,
}

/// A builder for `FakeEventDispatcher`s.
///
/// A `FakeEventDispatcherBuilder` is capable of storing the configuration of a
/// network stack including forwarding table entries, devices and their assigned
/// addresses and configurations, ARP table entries, etc. It can be built using
/// `build`, producing a `Context<FakeEventDispatcher>` with all of the
/// appropriate state configured.
#[derive(Clone, Default)]
pub struct FakeEventDispatcherBuilder {
    devices: Vec<DeviceConfig>,
    // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
    arp_table_entries: Vec<(usize, SpecifiedAddr<Ipv4Addr>, UnicastAddr<Mac>)>,
    ndp_table_entries: Vec<(usize, UnicastAddr<Ipv6Addr>, UnicastAddr<Mac>)>,
    // usize refers to index into devices Vec.
    device_routes: Vec<(SubnetEither, usize)>,
}

impl FakeEventDispatcherBuilder {
    /// Construct a `FakeEventDispatcherBuilder` from a
    /// `FakeEventDispatcherConfig`.
    #[cfg(test)]
    pub(crate) fn from_config<A: IpAddress>(
        cfg: FakeEventDispatcherConfig<A>,
    ) -> FakeEventDispatcherBuilder {
        assert!(cfg.subnet.contains(&cfg.local_ip));
        assert!(cfg.subnet.contains(&cfg.remote_ip));

        let mut builder = FakeEventDispatcherBuilder::default();
        builder.devices.push(DeviceConfig {
            mac: cfg.local_mac,
            addr_subnet: Some(
                AddrSubnetEither::new(cfg.local_ip.get().into(), cfg.subnet.prefix()).unwrap(),
            ),
            ipv4_config: None,
            ipv6_config: None,
        });

        match cfg.remote_ip.into() {
            IpAddr::V4(ip) => builder.arp_table_entries.push((0, ip, cfg.remote_mac)),
            IpAddr::V6(ip) => builder.ndp_table_entries.push((
                0,
                UnicastAddr::new(ip.get()).unwrap(),
                cfg.remote_mac,
            )),
        };

        // Even with fixed ipv4 address we can have IPv6 link local addresses
        // pre-cached.
        builder.ndp_table_entries.push((
            0,
            cfg.remote_mac.to_ipv6_link_local().addr().get(),
            cfg.remote_mac,
        ));

        builder.device_routes.push((cfg.subnet.into(), 0));
        builder
    }

    /// Add a device.
    ///
    /// `add_device` returns a key which can be used to refer to the device in
    /// future calls to `add_arp_table_entry` and `add_device_route`.
    pub fn add_device(&mut self, mac: UnicastAddr<Mac>) -> usize {
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: None,
            ipv4_config: None,
            ipv6_config: None,
        });
        idx
    }

    /// Add a device with an IPv4 and IPv6 configuration.
    ///
    /// `add_device_with_config` is like `add_device`, except that it takes an
    /// IPv4 and IPv6 configuration to apply to the device when it is enabled.
    #[cfg(test)]
    pub(crate) fn add_device_with_config(
        &mut self,
        mac: UnicastAddr<Mac>,
        ipv4_config: Ipv4DeviceConfigurationUpdate,
        ipv6_config: Ipv6DeviceConfigurationUpdate,
    ) -> usize {
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: None,
            ipv4_config: Some(ipv4_config),
            ipv6_config: Some(ipv6_config),
        });
        idx
    }

    /// Add a device with an associated IP address.
    ///
    /// `add_device_with_ip` is like `add_device`, except that it takes an
    /// associated IP address and subnet to assign to the device.
    pub fn add_device_with_ip<A: IpAddress>(
        &mut self,
        mac: UnicastAddr<Mac>,
        ip: A,
        subnet: Subnet<A>,
    ) -> usize {
        assert!(subnet.contains(&ip));
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: Some(AddrSubnetEither::new(ip.into(), subnet.prefix()).unwrap()),
            ipv4_config: None,
            ipv6_config: None,
        });
        self.device_routes.push((subnet.into(), idx));
        idx
    }

    /// Add a device with an associated IP address and a particular IPv4 and
    /// IPv6 configuration.
    ///
    /// `add_device_with_ip_and_config` is like `add_device`, except that it
    /// takes an associated IP address and subnet to assign to the device, as
    /// well as IPv4 and IPv6 configurations to apply to the device when it is
    /// enabled.
    #[cfg(test)]
    pub(crate) fn add_device_with_ip_and_config<A: IpAddress>(
        &mut self,
        mac: UnicastAddr<Mac>,
        ip: A,
        subnet: Subnet<A>,
        ipv4_config: Ipv4DeviceConfigurationUpdate,
        ipv6_config: Ipv6DeviceConfigurationUpdate,
    ) -> usize {
        assert!(subnet.contains(&ip));
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: Some(AddrSubnetEither::new(ip.into(), subnet.prefix()).unwrap()),
            ipv4_config: Some(ipv4_config),
            ipv6_config: Some(ipv6_config),
        });
        self.device_routes.push((subnet.into(), idx));
        idx
    }

    /// Add an ARP table entry for a device's ARP table.
    #[cfg(test)]
    pub(crate) fn add_arp_table_entry(
        &mut self,
        device: usize,
        // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
        ip: SpecifiedAddr<Ipv4Addr>,
        mac: UnicastAddr<Mac>,
    ) {
        self.arp_table_entries.push((device, ip, mac));
    }

    /// Add an NDP table entry for a device's NDP table.
    #[cfg(test)]
    pub(crate) fn add_ndp_table_entry(
        &mut self,
        device: usize,
        // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
        ip: UnicastAddr<Ipv6Addr>,
        mac: UnicastAddr<Mac>,
    ) {
        self.ndp_table_entries.push((device, ip, mac));
    }

    /// Builds a `Ctx` from the present configuration with a default dispatcher.
    #[cfg(any(test, feature = "testutils"))]
    pub fn build(self) -> (FakeCtx, Vec<EthernetDeviceId<FakeBindingsCtx>>) {
        self.build_with_modifications(|_| {})
    }

    /// `build_with_modifications` is equivalent to `build`, except that after
    /// the `StackStateBuilder` is initialized, it is passed to `f` for further
    /// modification before the `Ctx` is constructed.
    #[cfg(any(test, feature = "testutils"))]
    pub(crate) fn build_with_modifications<F: FnOnce(&mut StackStateBuilder)>(
        self,
        f: F,
    ) -> (FakeCtx, Vec<EthernetDeviceId<FakeBindingsCtx>>) {
        let mut stack_builder = StackStateBuilder::default();
        f(&mut stack_builder);
        self.build_with(stack_builder)
    }

    /// Build a `Ctx` from the present configuration with a caller-provided
    /// dispatcher and `StackStateBuilder`.
    #[cfg(any(test, feature = "testutils"))]
    pub(crate) fn build_with(
        self,
        state_builder: StackStateBuilder,
    ) -> (FakeCtx, Vec<EthernetDeviceId<FakeBindingsCtx>>) {
        let mut ctx = Ctx::new_with_builder(state_builder);
        let Ctx { core_ctx, bindings_ctx } = &mut ctx;

        let FakeEventDispatcherBuilder {
            devices,
            arp_table_entries,
            ndp_table_entries,
            device_routes,
        } = self;
        let idx_to_device_id: Vec<_> = devices
            .into_iter()
            .map(|DeviceConfig { mac, addr_subnet: ip_and_subnet, ipv4_config, ipv6_config }| {
                let eth_id = crate::device::add_ethernet_device(
                    core_ctx,
                    mac,
                    IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                    DEFAULT_INTERFACE_METRIC,
                );
                let id = eth_id.clone().into();
                if let Some(ipv4_config) = ipv4_config {
                    let _previous = crate::device::testutil::update_ipv4_configuration(
                        core_ctx,
                        bindings_ctx,
                        &id,
                        ipv4_config,
                    )
                    .unwrap();
                }
                if let Some(ipv6_config) = ipv6_config {
                    let _previous = crate::device::testutil::update_ipv6_configuration(
                        core_ctx,
                        bindings_ctx,
                        &id,
                        ipv6_config,
                    )
                    .unwrap();
                }
                crate::device::testutil::enable_device(core_ctx, bindings_ctx, &id);
                match ip_and_subnet {
                    Some(AddrSubnetEither::V4(addr_sub)) => {
                        crate::device::add_ip_addr_subnet(core_ctx, bindings_ctx, &id, addr_sub)
                            .unwrap();
                    }
                    Some(AddrSubnetEither::V6(addr_sub)) => {
                        crate::device::add_ip_addr_subnet(core_ctx, bindings_ctx, &id, addr_sub)
                            .unwrap();
                    }
                    None => {}
                }
                eth_id
            })
            .collect();
        for (idx, ip, mac) in arp_table_entries {
            let device = &idx_to_device_id[idx];
            crate::device::insert_static_arp_table_entry(
                core_ctx,
                bindings_ctx,
                &device.clone().into(),
                ip,
                mac,
            )
            .expect("error inserting static ARP entry");
        }
        for (idx, ip, mac) in ndp_table_entries {
            let device = &idx_to_device_id[idx];
            crate::device::insert_static_ndp_table_entry(
                core_ctx,
                bindings_ctx,
                &device.clone().into(),
                ip,
                mac,
            )
            .expect("error inserting static NDP entry");
        }

        for (subnet, idx) in device_routes {
            let device = &idx_to_device_id[idx];
            crate::testutil::add_route(
                core_ctx,
                bindings_ctx,
                crate::ip::types::AddableEntryEither::without_gateway(
                    subnet,
                    device.clone().into(),
                    AddableMetric::ExplicitMetric(RawMetric(0)),
                ),
            )
            .expect("add device route");
        }

        (ctx, idx_to_device_id)
    }
}

/// Add either an NDP entry (if IPv6) or ARP entry (if IPv4) to a
/// `FakeEventDispatcherBuilder`.
#[cfg(test)]
pub(crate) fn add_arp_or_ndp_table_entry<A: IpAddress>(
    builder: &mut FakeEventDispatcherBuilder,
    device: usize,
    // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
    ip: SpecifiedAddr<A>,
    mac: UnicastAddr<Mac>,
) {
    match ip.into() {
        IpAddr::V4(ip) => builder.add_arp_table_entry(device, ip, mac),
        IpAddr::V6(ip) => {
            builder.add_ndp_table_entry(device, UnicastAddr::new(ip.get()).unwrap(), mac)
        }
    }
}

impl FakeNetworkContext for FakeCtx {
    type TimerId = TimerId<FakeBindingsCtx>;
    type SendMeta = EthernetWeakDeviceId<FakeBindingsCtx>;
}

impl<I: IcmpIpExt> UdpBindingsContext<I, DeviceId<Self>> for FakeBindingsCtx {
    fn receive_udp<B: BufferMut>(
        &mut self,
        id: udp::SocketId<I>,
        _device: &DeviceId<Self>,
        _dst_addr: (<I>::Addr, core::num::NonZeroU16),
        _src_addr: (<I>::Addr, Option<core::num::NonZeroU16>),
        body: &B,
    ) {
        let mut state = self.state_mut();
        let received = (&mut *state).udp_state_mut::<I>().entry(id).or_insert_with(Vec::default);
        received.push(body.as_ref().to_owned());
    }
}

impl IcmpEchoBindingsContext<Ipv4, DeviceId<Self>> for FakeBindingsCtx {
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        conn: crate::ip::icmp::SocketId<Ipv4>,
        _device: &DeviceId<Self>,
        _src_ip: Ipv4Addr,
        _dst_ip: Ipv4Addr,
        _id: u16,
        data: B,
    ) {
        let mut state = self.state_mut();
        let replies = state.icmpv4_replies.entry(conn).or_insert_with(Vec::default);
        replies.push(data.as_ref().to_owned());
    }
}

impl IcmpEchoBindingsContext<Ipv6, DeviceId<Self>> for FakeBindingsCtx {
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        conn: crate::ip::icmp::SocketId<Ipv6>,
        _device: &DeviceId<Self>,
        _src_ip: Ipv6Addr,
        _dst_ip: Ipv6Addr,
        _id: u16,
        data: B,
    ) {
        let mut state = self.state_mut();
        let replies = state.icmpv6_replies.entry(conn).or_insert_with(Vec::default);
        replies.push(data.as_ref().to_owned())
    }
}

impl crate::device::socket::DeviceSocketTypes for FakeBindingsCtx {
    type SocketState = Mutex<Vec<(WeakDeviceId<FakeBindingsCtx>, Vec<u8>)>>;
}

impl crate::device::socket::DeviceSocketBindingsContext<DeviceId<Self>> for FakeBindingsCtx {
    fn receive_frame(
        &self,
        state: &Self::SocketState,
        device: &DeviceId<Self>,
        _frame: crate::device::socket::Frame<&[u8]>,
        raw_frame: &[u8],
    ) {
        state.lock().push((device.downgrade(), raw_frame.into()));
    }
}

impl DeviceLayerStateTypes for FakeBindingsCtx {
    type LoopbackDeviceState = ();
    type EthernetDeviceState = ();
    type DeviceIdentifier = MonotonicIdentifier;
}

impl DeviceLayerEventDispatcher for FakeBindingsCtx {
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<FakeBindingsCtx>) {
        self.state_mut().rx_available.push(device.clone());
    }

    fn wake_tx_task(&mut self, device: &DeviceId<FakeBindingsCtx>) {
        self.state_mut().tx_available.push(device.clone());
    }

    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<FakeBindingsCtx>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        self.with_inner_mut(|ctx| ctx.frame_ctx_mut().push(device.downgrade(), frame.into_inner()));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn handle_queued_rx_packets(core_ctx: &FakeCoreCtx, bindings_ctx: &mut FakeBindingsCtx) {
    loop {
        let rx_available = core::mem::take(&mut bindings_ctx.state_mut().rx_available);
        if rx_available.len() == 0 {
            break;
        }

        for id in rx_available.into_iter() {
            loop {
                match crate::device::handle_queued_rx_packets(core_ctx, bindings_ctx, &id) {
                    crate::work_queue::WorkQueueReport::AllDone => break,
                    crate::work_queue::WorkQueueReport::Pending => (),
                }
            }
        }
    }
}

/// Wraps all events emitted by Core into a single enum type.
#[derive(Debug, Eq, PartialEq, Hash, GenericOverIp)]
#[generic_over_ip()]
#[allow(missing_docs)]
pub enum DispatchedEvent {
    IpDeviceIpv4(IpDeviceEvent<WeakDeviceId<FakeBindingsCtx>, Ipv4, FakeInstant>),
    IpDeviceIpv6(IpDeviceEvent<WeakDeviceId<FakeBindingsCtx>, Ipv6, FakeInstant>),
    IpLayerIpv4(IpLayerEvent<WeakDeviceId<FakeBindingsCtx>, Ipv4>),
    IpLayerIpv6(IpLayerEvent<WeakDeviceId<FakeBindingsCtx>, Ipv6>),
    NeighborIpv4(nud::Event<Mac, EthernetWeakDeviceId<FakeBindingsCtx>, Ipv4, FakeInstant>),
    NeighborIpv6(nud::Event<Mac, EthernetWeakDeviceId<FakeBindingsCtx>, Ipv6, FakeInstant>),
}

impl<I: Ip> From<IpDeviceEvent<DeviceId<FakeBindingsCtx>, I, FakeInstant>>
    for IpDeviceEvent<WeakDeviceId<FakeBindingsCtx>, I, FakeInstant>
{
    fn from(
        e: IpDeviceEvent<DeviceId<FakeBindingsCtx>, I, FakeInstant>,
    ) -> IpDeviceEvent<WeakDeviceId<FakeBindingsCtx>, I, FakeInstant> {
        match e {
            IpDeviceEvent::AddressAdded { device, addr, state, valid_until } => {
                IpDeviceEvent::AddressAdded { device: device.downgrade(), addr, state, valid_until }
            }
            IpDeviceEvent::AddressRemoved { device, addr, reason } => {
                IpDeviceEvent::AddressRemoved { device: device.downgrade(), addr, reason }
            }
            IpDeviceEvent::AddressStateChanged { device, addr, state } => {
                IpDeviceEvent::AddressStateChanged { device: device.downgrade(), addr, state }
            }
            IpDeviceEvent::EnabledChanged { device, ip_enabled } => {
                IpDeviceEvent::EnabledChanged { device: device.downgrade(), ip_enabled }
            }
            IpDeviceEvent::AddressPropertiesChanged { device, addr, valid_until } => {
                IpDeviceEvent::AddressPropertiesChanged {
                    device: device.downgrade(),
                    addr,
                    valid_until,
                }
            }
        }
    }
}

impl From<IpDeviceEvent<DeviceId<FakeBindingsCtx>, Ipv4, FakeInstant>> for DispatchedEvent {
    fn from(e: IpDeviceEvent<DeviceId<FakeBindingsCtx>, Ipv4, FakeInstant>) -> DispatchedEvent {
        DispatchedEvent::IpDeviceIpv4(e.into())
    }
}

impl From<IpDeviceEvent<DeviceId<FakeBindingsCtx>, Ipv6, FakeInstant>> for DispatchedEvent {
    fn from(e: IpDeviceEvent<DeviceId<FakeBindingsCtx>, Ipv6, FakeInstant>) -> DispatchedEvent {
        DispatchedEvent::IpDeviceIpv6(e.into())
    }
}

impl<I: Ip> From<IpLayerEvent<DeviceId<FakeBindingsCtx>, I>>
    for IpLayerEvent<WeakDeviceId<FakeBindingsCtx>, I>
{
    fn from(
        e: IpLayerEvent<DeviceId<FakeBindingsCtx>, I>,
    ) -> IpLayerEvent<WeakDeviceId<FakeBindingsCtx>, I> {
        match e {
            IpLayerEvent::AddRoute(AddableEntry { subnet, device, gateway, metric }) => {
                IpLayerEvent::AddRoute(AddableEntry {
                    subnet,
                    device: device.downgrade(),
                    gateway,
                    metric,
                })
            }
            IpLayerEvent::RemoveRoutes { subnet, device, gateway } => {
                IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway }
            }
        }
    }
}

impl From<IpLayerEvent<DeviceId<FakeBindingsCtx>, Ipv4>> for DispatchedEvent {
    fn from(e: IpLayerEvent<DeviceId<FakeBindingsCtx>, Ipv4>) -> DispatchedEvent {
        DispatchedEvent::IpLayerIpv4(e.into())
    }
}

impl From<IpLayerEvent<DeviceId<FakeBindingsCtx>, Ipv6>> for DispatchedEvent {
    fn from(e: IpLayerEvent<DeviceId<FakeBindingsCtx>, Ipv6>) -> DispatchedEvent {
        DispatchedEvent::IpLayerIpv6(e.into())
    }
}

impl<I: Ip> From<nud::Event<Mac, EthernetDeviceId<FakeBindingsCtx>, I, FakeInstant>>
    for nud::Event<Mac, EthernetWeakDeviceId<FakeBindingsCtx>, I, FakeInstant>
{
    fn from(
        nud::Event { device, kind, addr, at }: nud::Event<
            Mac,
            EthernetDeviceId<FakeBindingsCtx>,
            I,
            FakeInstant,
        >,
    ) -> Self {
        Self { device: device.downgrade(), kind, addr, at }
    }
}

impl<I: Ip> From<nud::Event<Mac, EthernetDeviceId<FakeBindingsCtx>, I, FakeInstant>>
    for DispatchedEvent
{
    fn from(
        e: nud::Event<Mac, EthernetDeviceId<FakeBindingsCtx>, I, FakeInstant>,
    ) -> DispatchedEvent {
        I::map_ip(
            e,
            |e| DispatchedEvent::NeighborIpv4(e.into()),
            |e| DispatchedEvent::NeighborIpv6(e.into()),
        )
    }
}

#[cfg(test)]
pub(crate) fn handle_timer(
    FakeCtx { core_ctx, bindings_ctx }: &mut FakeCtx,
    _bindings_ctx: &mut (),
    id: TimerId<FakeBindingsCtx>,
) {
    crate::time::handle_timer(core_ctx, bindings_ctx, id)
}

pub(crate) const IPV6_MIN_IMPLIED_MAX_FRAME_SIZE: MaxEthernetFrameSize =
    const_unwrap::const_unwrap_option(MaxEthernetFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU));

/// Add a route directly to the forwarding table.
#[cfg(any(test, feature = "testutils"))]
pub fn add_route<BC: crate::BindingsContext>(
    core_ctx: &crate::SyncCtx<BC>,
    bindings_ctx: &mut BC,
    entry: crate::ip::types::AddableEntryEither<crate::device::DeviceId<BC>>,
) -> Result<(), crate::ip::forwarding::AddRouteError> {
    let mut core_ctx = lock_order::Locked::new(core_ctx);
    match entry {
        crate::ip::types::AddableEntryEither::V4(entry) => {
            crate::ip::forwarding::testutil::add_route::<Ipv4, _, _>(
                &mut core_ctx,
                bindings_ctx,
                entry,
            )
        }
        crate::ip::types::AddableEntryEither::V6(entry) => {
            crate::ip::forwarding::testutil::add_route::<Ipv6, _, _>(
                &mut core_ctx,
                bindings_ctx,
                entry,
            )
        }
    }
}

/// Delete a route from the forwarding table, returning `Err` if no route was
/// found to be deleted.
#[cfg(any(test, feature = "testutils"))]
pub fn del_routes_to_subnet<BC: crate::BindingsContext>(
    core_ctx: &crate::SyncCtx<BC>,
    bindings_ctx: &mut BC,
    subnet: net_types::ip::SubnetEither,
) -> crate::error::Result<()> {
    let mut core_ctx = lock_order::Locked::new(core_ctx);

    match subnet {
        SubnetEither::V4(subnet) => crate::ip::forwarding::testutil::del_routes_to_subnet::<
            Ipv4,
            _,
            _,
        >(&mut core_ctx, bindings_ctx, subnet),
        SubnetEither::V6(subnet) => crate::ip::forwarding::testutil::del_routes_to_subnet::<
            Ipv6,
            _,
            _,
        >(&mut core_ctx, bindings_ctx, subnet),
    }
    .map_err(From::from)
}

pub(crate) fn del_device_routes<BC: crate::BindingsContext>(
    core_ctx: &crate::SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
) {
    let mut core_ctx = lock_order::Locked::new(core_ctx);
    crate::ip::forwarding::testutil::del_device_routes::<Ipv4, _, _>(
        &mut core_ctx,
        bindings_ctx,
        device,
    );
    crate::ip::forwarding::testutil::del_device_routes::<Ipv6, _, _>(
        &mut core_ctx,
        bindings_ctx,
        device,
    );
}

/// Removes all of the routes through the device, then removes the device.
pub fn clear_routes_and_remove_ethernet_device<BC: crate::BindingsContext>(
    core_ctx: &crate::SyncCtx<BC>,
    bindings_ctx: &mut BC,
    ethernet_device: crate::device::EthernetDeviceId<BC>,
) {
    let device_id = crate::device::DeviceId::Ethernet(ethernet_device);
    del_device_routes(core_ctx, bindings_ctx, &device_id);
    let ethernet_device = match device_id {
        crate::device::DeviceId::Ethernet(ethernet_device) => ethernet_device,
        crate::device::DeviceId::Loopback(_) => unreachable!(),
    };
    match crate::device::remove_ethernet_device(core_ctx, bindings_ctx, ethernet_device) {
        crate::device::RemoveDeviceResult::Removed(_external_state) => {}
        crate::device::RemoveDeviceResult::Deferred(_reference_receiver) => {
            panic!("failed to remove ethernet device")
        }
    }
}

/// A convenient monotonically increasing identifier to use as the bindings'
/// `DeviceIdentifier` in tests.
pub struct MonotonicIdentifier(usize);

impl Debug for MonotonicIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NB: This type is used as part of the debug implementation in device
        // IDs which should provide enough context themselves on the type. For
        // brevity we omit the type name.
        let Self(id) = self;
        Debug::fmt(id, f)
    }
}

impl Display for MonotonicIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(self, f)
    }
}

static MONOTONIC_COUNTER: AtomicUsize = AtomicUsize::new(1);

impl MonotonicIdentifier {
    /// Creates a new identifier with the next value.
    pub fn new() -> Self {
        Self(MONOTONIC_COUNTER.fetch_add(1, core::sync::atomic::Ordering::SeqCst))
    }
}

impl Default for MonotonicIdentifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use ip_test_macro::ip_test;
    use lock_order::Locked;
    use packet::{Buf, Serializer};
    use packet_formats::{
        icmp::{IcmpEchoRequest, IcmpPacketBuilder, IcmpUnusedCode},
        ip::Ipv4Proto,
    };

    use super::*;
    use crate::{
        context::testutil::{FakeNetwork, FakeNetworkLinks},
        device::testutil::receive_frame,
        ip::{
            socket::{DefaultSendOptions, IpSocketHandler},
            IpLayerHandler,
        },
        socket::address::SocketIpAddr,
        time::TimerIdInner,
    };

    #[test]
    fn test_fake_network_transmits_packets() {
        set_logger_for_test();
        let (alice_ctx, alice_device_ids) = FAKE_CONFIG_V4.into_builder().build();
        let (bob_ctx, bob_device_ids) = FAKE_CONFIG_V4.swap().into_builder().build();
        let mut net = crate::context::testutil::new_simple_fake_network(
            "alice",
            alice_ctx,
            alice_device_ids[0].downgrade(),
            "bob",
            bob_ctx,
            bob_device_ids[0].downgrade(),
        );
        core::mem::drop((alice_device_ids, bob_device_ids));

        // Alice sends Bob a ping.

        net.with_context("alice", |Ctx { core_ctx, bindings_ctx }| {
            IpSocketHandler::<Ipv4, _>::send_oneshot_ip_packet(
                &mut Locked::new(&*core_ctx),
                bindings_ctx,
                None, // device
                None, // local_ip
                SocketIpAddr::new_from_specified_or_panic(FAKE_CONFIG_V4.remote_ip),
                Ipv4Proto::Icmp,
                DefaultSendOptions,
                |_| {
                    let req = IcmpEchoRequest::new(0, 0);
                    let req_body = &[1, 2, 3, 4];
                    Buf::new(req_body.to_vec(), ..).encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                        FAKE_CONFIG_V4.local_ip,
                        FAKE_CONFIG_V4.remote_ip,
                        IcmpUnusedCode,
                        req,
                    ))
                },
                None,
            )
            .unwrap();
        });

        // Send from Alice to Bob.
        assert_eq!(net.step(receive_frame, handle_timer).frames_sent, 1);
        // Respond from Bob to Alice.
        assert_eq!(net.step(receive_frame, handle_timer).frames_sent, 1);
        // Should've starved all events.
        assert!(net.step(receive_frame, handle_timer).is_idle());
    }

    #[test]
    fn test_fake_network_timers() {
        set_logger_for_test();
        let (ctx_1, device_ids_1) = FAKE_CONFIG_V4.into_builder().build();
        let (ctx_2, device_ids_2) = FAKE_CONFIG_V4.swap().into_builder().build();
        let mut net = crate::context::testutil::new_simple_fake_network(
            1,
            ctx_1,
            device_ids_1[0].downgrade(),
            2,
            ctx_2,
            device_ids_2[0].downgrade(),
        );
        core::mem::drop((device_ids_1, device_ids_2));

        net.with_context(1, |Ctx { core_ctx: _, bindings_ctx }| {
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(1), TimerId(TimerIdInner::Nop(1))),
                None
            );
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(4), TimerId(TimerIdInner::Nop(4))),
                None
            );
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(5), TimerId(TimerIdInner::Nop(5))),
                None
            );
        });

        net.with_context(2, |Ctx { core_ctx: _, bindings_ctx }| {
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(2), TimerId(TimerIdInner::Nop(2))),
                None
            );
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(3), TimerId(TimerIdInner::Nop(3))),
                None
            );
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(5), TimerId(TimerIdInner::Nop(6))),
                None
            );
        });

        // No timers fired before.
        assert_eq!(net.core_ctx(1).state.timer_counters.nop.get(), 0);
        assert_eq!(net.core_ctx(2).state.timer_counters.nop.get(), 0);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 1 should have fired.
        assert_eq!(net.core_ctx(1).state.timer_counters.nop.get(), 1);
        assert_eq!(net.core_ctx(2).state.timer_counters.nop.get(), 0);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 2 should have fired.
        assert_eq!(net.core_ctx(1).state.timer_counters.nop.get(), 1);
        assert_eq!(net.core_ctx(2).state.timer_counters.nop.get(), 1);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 2 should have fired.
        assert_eq!(net.core_ctx(1).state.timer_counters.nop.get(), 1);
        assert_eq!(net.core_ctx(2).state.timer_counters.nop.get(), 2);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 1 should have fired.
        assert_eq!(net.core_ctx(1).state.timer_counters.nop.get(), 2);
        assert_eq!(net.core_ctx(2).state.timer_counters.nop.get(), 2);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 2);
        // Both timers have fired at the same time.
        assert_eq!(net.core_ctx(1).state.timer_counters.nop.get(), 3);
        assert_eq!(net.core_ctx(2).state.timer_counters.nop.get(), 3);

        assert!(net.step(receive_frame, handle_timer).is_idle());
        // Check that current time on contexts tick together.
        let t1 = net.with_context(1, |Ctx { core_ctx: _, bindings_ctx }| bindings_ctx.now());
        let t2 = net.with_context(2, |Ctx { core_ctx: _, bindings_ctx }| bindings_ctx.now());
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_fake_network_until_idle() {
        set_logger_for_test();
        let (ctx_1, device_ids_1) = FAKE_CONFIG_V4.into_builder().build();
        let (ctx_2, device_ids_2) = FAKE_CONFIG_V4.swap().into_builder().build();
        let mut net = crate::context::testutil::new_simple_fake_network(
            1,
            ctx_1,
            device_ids_1[0].downgrade(),
            2,
            ctx_2,
            device_ids_2[0].downgrade(),
        );
        core::mem::drop((device_ids_1, device_ids_2));

        net.with_context(1, |Ctx { core_ctx: _, bindings_ctx }| {
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(1), TimerId(TimerIdInner::Nop(1))),
                None
            );
        });
        net.with_context(2, |Ctx { core_ctx: _, bindings_ctx }| {
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(2), TimerId(TimerIdInner::Nop(2))),
                None
            );
            assert_eq!(
                bindings_ctx.schedule_timer(Duration::from_secs(3), TimerId(TimerIdInner::Nop(3))),
                None
            );
        });

        while !net.step(receive_frame, handle_timer).is_idle()
            && net.core_ctx(1).state.timer_counters.nop.get() < 1
            || net.core_ctx(2).state.timer_counters.nop.get() < 1
        {}
        // Assert that we stopped before all times were fired, meaning we can
        // step again.
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
    }

    #[test]
    fn test_delayed_packets() {
        set_logger_for_test();
        // Create a network that takes 5ms to get any packet to go through.
        let latency = Duration::from_millis(5);
        let (alice_ctx, alice_device_ids) = FAKE_CONFIG_V4.into_builder().build();
        let (bob_ctx, bob_device_ids) = FAKE_CONFIG_V4.swap().into_builder().build();
        let alice_device_id = alice_device_ids[0].clone();
        let bob_device_id = bob_device_ids[0].clone();
        core::mem::drop((alice_device_ids, bob_device_ids));
        let mut net = FakeNetwork::new(
            [("alice", alice_ctx), ("bob", bob_ctx)],
            move |net: &'static str, _device_id: EthernetWeakDeviceId<FakeBindingsCtx>| {
                if net == "alice" {
                    vec![("bob", bob_device_id.clone(), Some(latency))]
                } else {
                    vec![("alice", alice_device_id.clone(), Some(latency))]
                }
            },
        );

        // Alice sends Bob a ping.
        net.with_context("alice", |Ctx { core_ctx, bindings_ctx }| {
            IpSocketHandler::<Ipv4, _>::send_oneshot_ip_packet(
                &mut Locked::new(&*core_ctx),
                bindings_ctx,
                None, // device
                None, // local_ip
                SocketIpAddr::new_from_specified_or_panic(FAKE_CONFIG_V4.remote_ip),
                Ipv4Proto::Icmp,
                DefaultSendOptions,
                |_| {
                    let req = IcmpEchoRequest::new(0, 0);
                    let req_body = &[1, 2, 3, 4];
                    Buf::new(req_body.to_vec(), ..).encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                        FAKE_CONFIG_V4.local_ip,
                        FAKE_CONFIG_V4.remote_ip,
                        IcmpUnusedCode,
                        req,
                    ))
                },
                None,
            )
            .unwrap();
        });

        net.with_context("alice", |Ctx { core_ctx: _, bindings_ctx }| {
            assert_eq!(
                bindings_ctx
                    .schedule_timer(Duration::from_millis(3), TimerId(TimerIdInner::Nop(1))),
                None
            );
        });
        net.with_context("bob", |Ctx { core_ctx: _, bindings_ctx }| {
            assert_eq!(
                bindings_ctx
                    .schedule_timer(Duration::from_millis(7), TimerId(TimerIdInner::Nop(2))),
                None
            );
            assert_eq!(
                bindings_ctx
                    .schedule_timer(Duration::from_millis(10), TimerId(TimerIdInner::Nop(1))),
                None
            );
        });

        // Order of expected events is as follows:
        // - Alice's timer expires at t = 3
        // - Bob receives Alice's packet at t = 5
        // - Bob's timer expires at t = 7
        // - Alice receives Bob's response and Bob's last timer fires at t = 10

        fn assert_full_state<
            'a,
            L: FakeNetworkLinks<
                EthernetWeakDeviceId<FakeBindingsCtx>,
                EthernetDeviceId<FakeBindingsCtx>,
                &'a str,
            >,
        >(
            net: &mut FakeNetwork<&'a str, EthernetDeviceId<FakeBindingsCtx>, FakeCtx, L>,
            alice_nop: u64,
            bob_nop: u64,
            bob_echo_request: u64,
            alice_echo_response: u64,
        ) {
            assert_eq!(net.core_ctx("alice").state.timer_counters.nop.get(), alice_nop);
            assert_eq!(
                net.core_ctx("alice").state.icmp_rx_counters::<Ipv4>().echo_reply.get(),
                alice_echo_response
            );

            assert_eq!(net.core_ctx("bob").state.timer_counters.nop.get(), bob_nop);
            assert_eq!(
                net.core_ctx("bob").state.icmp_rx_counters::<Ipv4>().echo_request.get(),
                bob_echo_request
            );
        }

        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        assert_full_state(&mut net, 1, 0, 0, 0);
        assert_eq!(net.step(receive_frame, handle_timer).frames_sent, 1);
        assert_full_state(&mut net, 1, 0, 1, 0);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        assert_full_state(&mut net, 1, 1, 1, 0);
        let step = net.step(receive_frame, handle_timer);
        assert_eq!(step.frames_sent, 1);
        assert_eq!(step.timers_fired, 1);
        assert_full_state(&mut net, 1, 2, 1, 1);

        // Should've starved all events.
        assert!(net.step(receive_frame, handle_timer).is_idle());
    }

    fn send_packet<'a, A: IpAddress>(
        core_ctx: &'a FakeCoreCtx,
        bindings_ctx: &mut FakeBindingsCtx,
        src_ip: SpecifiedAddr<A>,
        dst_ip: SpecifiedAddr<A>,
        device: &DeviceId<FakeBindingsCtx>,
    ) where
        A::Version: TestIpExt,
        Locked<&'a FakeCoreCtx, crate::lock_ordering::Unlocked>:
            IpLayerHandler<A::Version, FakeBindingsCtx, DeviceId = DeviceId<FakeBindingsCtx>>,
    {
        let meta = SendIpPacketMeta {
            device,
            src_ip: Some(src_ip),
            dst_ip,
            next_hop: dst_ip,
            proto: IpProto::Udp.into(),
            ttl: None,
            mtu: None,
        };
        IpLayerHandler::<A::Version, _>::send_ip_packet_from_device(
            &mut Locked::new(core_ctx),
            bindings_ctx,
            meta,
            Buf::new(vec![1, 2, 3, 4], ..),
        )
        .unwrap();
    }

    #[ip_test]
    fn test_send_to_many<I: Ip + TestIpExt>()
    where
        for<'a> Locked<&'a FakeCoreCtx, crate::lock_ordering::Unlocked>: IpLayerHandler<
            I,
            FakeBindingsCtx,
            DeviceId = DeviceId<FakeBindingsCtx>,
            WeakDeviceId = WeakDeviceId<FakeBindingsCtx>,
        >,
    {
        let mac_a = UnicastAddr::new(Mac::new([2, 3, 4, 5, 6, 7])).unwrap();
        let mac_b = UnicastAddr::new(Mac::new([2, 3, 4, 5, 6, 8])).unwrap();
        let mac_c = UnicastAddr::new(Mac::new([2, 3, 4, 5, 6, 9])).unwrap();
        let ip_a = I::get_other_ip_address(1);
        let ip_b = I::get_other_ip_address(2);
        let ip_c = I::get_other_ip_address(3);
        let subnet = Subnet::new(I::get_other_ip_address(0).get(), I::Addr::BYTES * 8 - 8).unwrap();
        let mut alice = FakeEventDispatcherBuilder::default();
        let alice_device_idx = alice.add_device_with_ip(mac_a, ip_a.get(), subnet);
        let mut bob = FakeEventDispatcherBuilder::default();
        let bob_device_idx = bob.add_device_with_ip(mac_b, ip_b.get(), subnet);
        let mut calvin = FakeEventDispatcherBuilder::default();
        let calvin_device_idx = calvin.add_device_with_ip(mac_c, ip_c.get(), subnet);
        add_arp_or_ndp_table_entry(&mut alice, alice_device_idx, ip_b, mac_b);
        add_arp_or_ndp_table_entry(&mut alice, alice_device_idx, ip_c, mac_c);
        add_arp_or_ndp_table_entry(&mut bob, bob_device_idx, ip_a, mac_a);
        add_arp_or_ndp_table_entry(&mut bob, bob_device_idx, ip_c, mac_c);
        add_arp_or_ndp_table_entry(&mut calvin, calvin_device_idx, ip_a, mac_a);
        add_arp_or_ndp_table_entry(&mut calvin, calvin_device_idx, ip_b, mac_b);
        let (alice_ctx, alice_device_ids) = alice.build();
        let (bob_ctx, bob_device_ids) = bob.build();
        let (calvin_ctx, calvin_device_ids) = calvin.build();
        let alice_device_id = alice_device_ids[alice_device_idx].clone();
        let bob_device_id = bob_device_ids[bob_device_idx].clone();
        let calvin_device_id = calvin_device_ids[calvin_device_idx].clone();
        let mut net = FakeNetwork::new(
            [("alice", alice_ctx), ("bob", bob_ctx), ("calvin", calvin_ctx)],
            move |net: &'static str, _device_id: EthernetWeakDeviceId<FakeBindingsCtx>| match net {
                "alice" => vec![
                    ("bob", bob_device_id.clone(), None),
                    ("calvin", calvin_device_id.clone(), None),
                ],
                "bob" => vec![("alice", alice_device_id.clone(), None)],
                "calvin" => Vec::new(),
                _ => unreachable!(),
            },
        );
        let alice_device_id = alice_device_ids[alice_device_idx].clone();
        let bob_device_id = bob_device_ids[bob_device_idx].clone();
        let calvin_device_id = calvin_device_ids[calvin_device_idx].clone();
        core::mem::drop((alice_device_ids, bob_device_ids, calvin_device_ids));

        net.collect_frames();
        assert_empty(net.bindings_ctx("alice").frames_sent().iter());
        assert_empty(net.bindings_ctx("bob").frames_sent().iter());
        assert_empty(net.bindings_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());

        // Bob and Calvin should get any packet sent by Alice.

        net.with_context("alice", |Ctx { core_ctx, bindings_ctx }| {
            send_packet(core_ctx, bindings_ctx, ip_a, ip_b, &alice_device_id.clone().into());
        });
        assert_eq!(net.bindings_ctx("alice").frames_sent().len(), 1);
        assert_empty(net.bindings_ctx("bob").frames_sent().iter());
        assert_empty(net.bindings_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());
        net.collect_frames();
        assert_empty(net.bindings_ctx("alice").frames_sent().iter());
        assert_empty(net.bindings_ctx("bob").frames_sent().iter());
        assert_empty(net.bindings_ctx("calvin").frames_sent().iter());
        assert_eq!(net.iter_pending_frames().count(), 2);
        assert!(net
            .iter_pending_frames()
            .any(|InstantAndData(_, x)| (x.dst_context == "bob") && (&x.meta == &bob_device_id)));
        assert!(net
            .iter_pending_frames()
            .any(|InstantAndData(_, x)| (x.dst_context == "calvin")
                && (&x.meta == &calvin_device_id)));

        // Only Alice should get packets sent by Bob.

        net.drop_pending_frames();
        net.with_context("bob", |Ctx { core_ctx, bindings_ctx }| {
            send_packet(core_ctx, bindings_ctx, ip_b, ip_a, &bob_device_id.clone().into());
        });
        assert_empty(net.bindings_ctx("alice").frames_sent().iter());
        assert_eq!(net.bindings_ctx("bob").frames_sent().len(), 1);
        assert_empty(net.bindings_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());
        net.collect_frames();
        assert_empty(net.bindings_ctx("alice").frames_sent().iter());
        assert_empty(net.bindings_ctx("bob").frames_sent().iter());
        assert_empty(net.bindings_ctx("calvin").frames_sent().iter());
        assert_eq!(net.iter_pending_frames().count(), 1);
        assert!(net.iter_pending_frames().any(
            |InstantAndData(_, x)| (x.dst_context == "alice") && (&x.meta == &alice_device_id)
        ));

        // No one gets packets sent by Calvin.

        net.drop_pending_frames();
        net.with_context("calvin", |Ctx { core_ctx, bindings_ctx }| {
            send_packet(core_ctx, bindings_ctx, ip_c, ip_a, &calvin_device_id.clone().into());
        });
        assert_empty(net.bindings_ctx("alice").frames_sent().iter());
        assert_empty(net.bindings_ctx("bob").frames_sent().iter());
        assert_eq!(net.bindings_ctx("calvin").frames_sent().len(), 1);
        assert_empty(net.iter_pending_frames());
        net.collect_frames();
        assert_empty(net.bindings_ctx("alice").frames_sent().iter());
        assert_empty(net.bindings_ctx("bob").frames_sent().iter());
        assert_empty(net.bindings_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());
    }
}
