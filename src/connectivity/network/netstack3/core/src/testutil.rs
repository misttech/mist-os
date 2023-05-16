// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Testing-related utilities.

use alloc::{borrow::ToOwned, collections::HashMap, vec, vec::Vec};
use core::{
    fmt::Debug,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    time::Duration,
};

use net_types::{
    ethernet::Mac,
    ip::{AddrSubnet, Ip, IpAddr, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet, SubnetEither},
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{Buf, BufferMut};
use packet_formats::ip::IpProto;
use rand::{self, CryptoRng, Rng as _, RngCore, SeedableRng};
use rand_xorshift::XorShiftRng;
use tracing::Subscriber;
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields},
        FmtContext,
    },
    registry::LookupSpan,
};

use crate::{
    context::{
        testutil::{FakeFrameCtx, FakeInstant, FakeNetworkContext, FakeTimerCtx, InstantAndData},
        EventContext, InstantContext, RngContext, TimerContext,
    },
    device::{
        ethernet, loopback::LoopbackDeviceId, DeviceId, DeviceLayerEventDispatcher,
        DeviceLayerStateTypes, DeviceSendFrameError, EthernetDeviceId, EthernetWeakDeviceId,
        WeakDeviceId,
    },
    ip::{
        device::IpDeviceEvent,
        icmp::{BufferIcmpContext, IcmpConnId, IcmpContext, IcmpIpExt},
        types::{AddableEntryEither, AddableMetric, Entry, RawMetric},
        IpLayerEvent, SendIpPacketMeta,
    },
    transport::{
        tcp::{buffer::RingBuffer, socket::NonSyncContext, BufferSizes},
        udp,
    },
    StackStateBuilder, SyncCtx, TimerId,
};

/// The default interface routing metric for test interfaces.
pub(crate) const DEFAULT_INTERFACE_METRIC: RawMetric = RawMetric(100);

/// Context available during the execution of the netstack.
#[derive(Default)]
pub(crate) struct Ctx<NonSyncCtx: crate::NonSyncContext> {
    /// The synchronized context.
    pub sync_ctx: SyncCtx<NonSyncCtx>,
    /// The non-synchronized context.
    // We put `non_sync_ctx` after `sync_ctx` to make sure that `sync_ctx` is
    // dropped before `non-sync_ctx` so that the existence of strongly-referenced
    // device IDs in `non_sync_ctx` causes test failures, forcing proper cleanup
    // of device IDs in our unit tests.
    //
    // Note that if strongly-referenced (device) IDs exist when dropping the
    // primary reference, the primary reference's drop impl will panic. See
    // `crate::sync::PrimaryRc::drop` for details.
    pub non_sync_ctx: NonSyncCtx,
}

impl<NonSyncCtx: crate::NonSyncContext + Default> Ctx<NonSyncCtx> {
    pub(crate) fn new_with_builder(builder: StackStateBuilder) -> Self {
        let mut non_sync_ctx = Default::default();
        let state = builder.build_with_ctx(&mut non_sync_ctx);
        Self { sync_ctx: SyncCtx { state, non_sync_ctx_marker: PhantomData }, non_sync_ctx }
    }
}

/// Asserts that an iterable object produces zero items.
///
/// `assert_empty` drains `into_iter.into_iter()` and asserts that zero
/// items are produced. It panics with a message which includes the produced
/// items if this assertion fails.
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
pub(crate) struct FakeNonSyncCtxState {
    icmpv4_replies: HashMap<IcmpConnId<Ipv4>, Vec<(u16, Vec<u8>)>>,
    icmpv6_replies: HashMap<IcmpConnId<Ipv6>, Vec<(u16, Vec<u8>)>>,
    pub(crate) rx_available: Vec<LoopbackDeviceId<FakeNonSyncCtx>>,
    pub(crate) tx_available: Vec<DeviceId<FakeNonSyncCtx>>,
}

// Use the `Never` type for the `crate::context::testutil::FakeCtx`'s frame
// metadata type. This ensures that we don't accidentally send frames to its
// `FakeFrameCtx`, which isn't actually used (instead, we use the
// `FakeFrameCtx` stored in `FakeEventDispatcher`). Note that this doesn't
// prevent code from attempting to read from this context (code which only
// accesses the frame contents rather than the frame metadata will still
// compile).
pub(crate) type FakeCtx = Ctx<FakeNonSyncCtx>;
pub(crate) type FakeSyncCtx = SyncCtx<FakeNonSyncCtx>;

#[derive(Default)]
pub(crate) struct FakeNonSyncCtx(
    crate::context::testutil::FakeNonSyncCtx<TimerId<Self>, DispatchedEvent, FakeNonSyncCtxState>,
);

impl DerefMut for FakeNonSyncCtx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let Self(this) = self;
        this
    }
}

impl Deref for FakeNonSyncCtx {
    type Target = crate::context::testutil::FakeNonSyncCtx<
        TimerId<Self>,
        DispatchedEvent,
        FakeNonSyncCtxState,
    >;

    fn deref(&self) -> &Self::Target {
        let Self(this) = self;
        this
    }
}

impl<T> AsRef<T> for FakeNonSyncCtx
where
    crate::context::testutil::FakeNonSyncCtx<TimerId<Self>, DispatchedEvent, FakeNonSyncCtxState>:
        AsRef<T>,
{
    fn as_ref(&self) -> &T {
        let Self(this) = self;
        this.as_ref()
    }
}

impl<T> AsMut<T> for FakeNonSyncCtx
where
    crate::context::testutil::FakeNonSyncCtx<TimerId<Self>, DispatchedEvent, FakeNonSyncCtxState>:
        AsMut<T>,
{
    fn as_mut(&mut self) -> &mut T {
        let Self(this) = self;
        this.as_mut()
    }
}

impl TimerContext<TimerId<FakeNonSyncCtx>> for FakeNonSyncCtx {
    fn schedule_timer_instant(
        &mut self,
        time: FakeInstant,
        id: TimerId<FakeNonSyncCtx>,
    ) -> Option<FakeInstant> {
        let Self(this) = self;
        this.schedule_timer_instant(time, id)
    }

    fn cancel_timer(&mut self, id: TimerId<FakeNonSyncCtx>) -> Option<FakeInstant> {
        let Self(this) = self;
        this.cancel_timer(id)
    }

    fn cancel_timers_with<F: FnMut(&TimerId<FakeNonSyncCtx>) -> bool>(&mut self, f: F) {
        let Self(this) = self;
        this.cancel_timers_with(f);
    }

    fn scheduled_instant(&self, id: TimerId<FakeNonSyncCtx>) -> Option<FakeInstant> {
        let Self(this) = self;
        this.scheduled_instant(id)
    }
}

impl RngContext for FakeNonSyncCtx {
    type Rng<'a> = &'a mut FakeCryptoRng<XorShiftRng> where Self: 'a;

    fn rng(&mut self) -> Self::Rng<'_> {
        let Self(this) = self;
        this.rng()
    }
}

impl NonSyncContext for FakeNonSyncCtx {
    type ReceiveBuffer = RingBuffer;

    type SendBuffer = RingBuffer;

    type ReturnedBuffers = ();

    type ProvidedBuffers = ();

    fn on_waiting_connections_change<I: Ip>(
        &mut self,
        _listener: crate::transport::tcp::socket::ListenerId<I>,
        _count: usize,
    ) {
    }

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers) {
        let BufferSizes { send, receive } = buffer_sizes;
        (RingBuffer::new(receive), RingBuffer::new(send), ())
    }

    fn on_connection_status_change<I: Ip>(
        &mut self,
        _connection: crate::transport::tcp::socket::ConnectionId<I>,
        _status: crate::transport::tcp::socket::ConnectionStatusUpdate,
    ) {
    }

    fn default_buffer_sizes() -> BufferSizes {
        // Use the test-only default impl.
        BufferSizes::default()
    }
}

impl FakeNonSyncCtx {
    pub(crate) fn take_frames(&mut self) -> Vec<(EthernetWeakDeviceId<FakeNonSyncCtx>, Vec<u8>)> {
        self.frame_ctx_mut().take_frames()
    }

    pub(crate) fn frames_sent(&self) -> &[(EthernetWeakDeviceId<FakeNonSyncCtx>, Vec<u8>)] {
        self.frame_ctx().frames()
    }
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
pub(crate) struct FakeCryptoRng<R>(R);

impl Default for FakeCryptoRng<XorShiftRng> {
    fn default() -> FakeCryptoRng<XorShiftRng> {
        FakeCryptoRng::new_xorshift(12957992561116578403)
    }
}

impl FakeCryptoRng<XorShiftRng> {
    /// Creates a new [`FakeCryptoRng<XorShiftRng>`] from a seed.
    pub(crate) fn new_xorshift(seed: u128) -> FakeCryptoRng<XorShiftRng> {
        FakeCryptoRng(new_rng(seed))
    }
}

impl<R: RngCore> RngCore for FakeCryptoRng<R> {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }
    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.0.try_fill_bytes(dest)
    }
}

impl<R: RngCore> CryptoRng for FakeCryptoRng<R> {}

impl<R: SeedableRng> SeedableRng for FakeCryptoRng<R> {
    type Seed = R::Seed;

    fn from_seed(seed: Self::Seed) -> Self {
        Self(R::from_seed(seed))
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
pub(crate) fn with_fake_rngs<F: Fn(FakeCryptoRng<XorShiftRng>)>(iterations: u128, f: F) {
    for seed in 0..iterations {
        f(FakeCryptoRng::new_xorshift(seed))
    }
}

/// Invokes a function multiple times with different RNG seeds.
pub(crate) fn run_with_many_seeds<F: FnMut(u128)>(mut f: F) {
    // Arbitrary seed.
    let mut rng = new_rng(0x0fe50fae6c37593d71944697f1245847);
    for _ in 0..64 {
        f(rng.gen());
    }
}

struct SimpleFormatter;

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

static LOGGER_ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Install a logger for tests.
///
/// Call this method at the beginning of the test for which logging is desired.
/// This function sets global program state, so all tests that run after this
/// function is called will use the logger.
pub(crate) fn set_logger_for_test() {
    // `init` will panic if called multiple times.
    if LOGGER_ONCE.swap(false, core::sync::atomic::Ordering::AcqRel) {
        tracing_subscriber::fmt()
            .event_format(SimpleFormatter)
            .with_max_level(tracing::Level::TRACE)
            .init();
    }
}

/// Get the counter value for a `key`.
pub(crate) fn get_counter_val(ctx: &FakeNonSyncCtx, key: &str) -> usize {
    ctx.counter_ctx().get_counter_val(key)
}

/// An extension trait for `Ip` providing test-related functionality.
pub(crate) trait TestIpExt: crate::ip::IpExt + crate::ip::IpLayerIpExt {
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

/// A builder for `FakeEventDispatcher`s.
///
/// A `FakeEventDispatcherBuilder` is capable of storing the configuration of a
/// network stack including forwarding table entries, devices and their assigned
/// IP addresses, ARP table entries, etc. It can be built using `build`,
/// producing a `Context<FakeEventDispatcher>` with all of the appropriate
/// state configured.
#[derive(Clone, Default)]
pub(crate) struct FakeEventDispatcherBuilder {
    devices: Vec<(UnicastAddr<Mac>, Option<(IpAddr, SubnetEither)>)>,
    arp_table_entries: Vec<(usize, Ipv4Addr, UnicastAddr<Mac>)>,
    ndp_table_entries: Vec<(usize, UnicastAddr<Ipv6Addr>, UnicastAddr<Mac>)>,
    // usize refers to index into devices Vec.
    device_routes: Vec<(SubnetEither, usize)>,
    routes: Vec<AddableEntryEither<DeviceId<FakeNonSyncCtx>>>,
}

impl FakeEventDispatcherBuilder {
    /// Construct a `FakeEventDispatcherBuilder` from a
    /// `FakeEventDispatcherConfig`.
    pub(crate) fn from_config<A: IpAddress>(
        cfg: FakeEventDispatcherConfig<A>,
    ) -> FakeEventDispatcherBuilder {
        assert!(cfg.subnet.contains(&cfg.local_ip));
        assert!(cfg.subnet.contains(&cfg.remote_ip));

        let mut builder = FakeEventDispatcherBuilder::default();
        builder.devices.push((cfg.local_mac, Some((cfg.local_ip.get().into(), cfg.subnet.into()))));

        match cfg.remote_ip.get().into() {
            IpAddr::V4(ip) => builder.arp_table_entries.push((0, ip, cfg.remote_mac)),
            IpAddr::V6(ip) => {
                builder.ndp_table_entries.push((0, UnicastAddr::new(ip).unwrap(), cfg.remote_mac))
            }
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
    pub(crate) fn add_device(&mut self, mac: UnicastAddr<Mac>) -> usize {
        let idx = self.devices.len();
        self.devices.push((mac, None));
        idx
    }

    /// Add a device with an associated IP address.
    ///
    /// `add_device_with_ip` is like `add_device`, except that it takes an
    /// associated IP address and subnet to assign to the device.
    pub(crate) fn add_device_with_ip<A: IpAddress>(
        &mut self,
        mac: UnicastAddr<Mac>,
        ip: A,
        subnet: Subnet<A>,
    ) -> usize {
        let idx = self.devices.len();
        self.devices.push((mac, Some((ip.into(), subnet.into()))));
        self.device_routes.push((subnet.into(), idx));
        idx
    }

    /// Add an ARP table entry for a device's ARP table.
    pub(crate) fn add_arp_table_entry(
        &mut self,
        device: usize,
        ip: Ipv4Addr,
        mac: UnicastAddr<Mac>,
    ) {
        self.arp_table_entries.push((device, ip, mac));
    }

    /// Add an NDP table entry for a device's NDP table.
    pub(crate) fn add_ndp_table_entry(
        &mut self,
        device: usize,
        ip: UnicastAddr<Ipv6Addr>,
        mac: UnicastAddr<Mac>,
    ) {
        self.ndp_table_entries.push((device, ip, mac));
    }

    /// Builds a `Ctx` from the present configuration with a default dispatcher.
    pub(crate) fn build(self) -> (FakeCtx, Vec<EthernetDeviceId<FakeNonSyncCtx>>) {
        self.build_with_modifications(|_| {})
    }

    /// `build_with_modifications` is equivalent to `build`, except that after
    /// the `StackStateBuilder` is initialized, it is passed to `f` for further
    /// modification before the `Ctx` is constructed.
    pub(crate) fn build_with_modifications<F: FnOnce(&mut StackStateBuilder)>(
        self,
        f: F,
    ) -> (FakeCtx, Vec<EthernetDeviceId<FakeNonSyncCtx>>) {
        let mut stack_builder = StackStateBuilder::default();
        f(&mut stack_builder);
        self.build_with(stack_builder)
    }

    /// Build a `Ctx` from the present configuration with a caller-provided
    /// dispatcher and `StackStateBuilder`.
    pub(crate) fn build_with(
        self,
        state_builder: StackStateBuilder,
    ) -> (FakeCtx, Vec<EthernetDeviceId<FakeNonSyncCtx>>) {
        let mut ctx = Ctx::new_with_builder(state_builder);
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        let FakeEventDispatcherBuilder {
            devices,
            arp_table_entries,
            ndp_table_entries,
            device_routes,
            routes,
        } = self;
        let idx_to_device_id: Vec<_> = devices
            .into_iter()
            .map(|(mac, ip_subnet)| {
                let eth_id = crate::device::add_ethernet_device(
                    sync_ctx,
                    mac,
                    IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                    DEFAULT_INTERFACE_METRIC,
                );
                let id = eth_id.clone().into();
                crate::device::testutil::enable_device(sync_ctx, non_sync_ctx, &id);
                match ip_subnet {
                    Some((IpAddr::V4(ip), SubnetEither::V4(subnet))) => {
                        let addr_sub = AddrSubnet::new(ip, subnet.prefix()).unwrap();
                        crate::device::add_ip_addr_subnet(sync_ctx, non_sync_ctx, &id, addr_sub)
                            .unwrap();
                    }
                    Some((IpAddr::V6(ip), SubnetEither::V6(subnet))) => {
                        let addr_sub = AddrSubnet::new(ip, subnet.prefix()).unwrap();
                        crate::device::add_ip_addr_subnet(sync_ctx, non_sync_ctx, &id, addr_sub)
                            .unwrap();
                    }
                    None => {}
                    _ => unreachable!(),
                }
                eth_id
            })
            .collect();
        for (idx, ip, mac) in arp_table_entries {
            let device = &idx_to_device_id[idx];
            crate::device::insert_static_arp_table_entry(
                sync_ctx,
                non_sync_ctx,
                &device.clone().into(),
                ip,
                mac,
            )
            .expect("error inserting static ARP entry");
        }
        for (idx, ip, mac) in ndp_table_entries {
            let device = &idx_to_device_id[idx];
            crate::device::insert_ndp_table_entry(
                sync_ctx,
                non_sync_ctx,
                &device.clone().into(),
                ip,
                mac.get(),
            )
            .expect("error inserting static NDP entry");
        }
        for (subnet, idx) in device_routes {
            let device = &idx_to_device_id[idx];
            crate::add_route(
                sync_ctx,
                non_sync_ctx,
                AddableEntryEither::without_gateway(
                    subnet,
                    device.clone().into(),
                    AddableMetric::ExplicitMetric(RawMetric(0)),
                ),
            )
            .expect("add device route");
        }
        for entry in routes {
            crate::add_route(sync_ctx, non_sync_ctx, entry).expect("add remote route");
        }

        (ctx, idx_to_device_id)
    }
}

/// Add either an NDP entry (if IPv6) or ARP entry (if IPv4) to a
/// `FakeEventDispatcherBuilder`.
pub(crate) fn add_arp_or_ndp_table_entry<A: IpAddress>(
    builder: &mut FakeEventDispatcherBuilder,
    device: usize,
    ip: A,
    mac: UnicastAddr<Mac>,
) {
    match ip.into() {
        IpAddr::V4(ip) => builder.add_arp_table_entry(device, ip, mac),
        IpAddr::V6(ip) => builder.add_ndp_table_entry(device, UnicastAddr::new(ip).unwrap(), mac),
    }
}

impl AsMut<FakeFrameCtx<EthernetWeakDeviceId<FakeNonSyncCtx>>> for FakeCtx {
    fn as_mut(&mut self) -> &mut FakeFrameCtx<EthernetWeakDeviceId<FakeNonSyncCtx>> {
        self.non_sync_ctx.frame_ctx_mut()
    }
}

impl AsRef<FakeTimerCtx<TimerId<FakeNonSyncCtx>>> for FakeCtx {
    fn as_ref(&self) -> &FakeTimerCtx<TimerId<FakeNonSyncCtx>> {
        self.non_sync_ctx.as_ref()
    }
}

impl AsMut<FakeTimerCtx<TimerId<FakeNonSyncCtx>>> for FakeCtx {
    fn as_mut(&mut self) -> &mut FakeTimerCtx<TimerId<FakeNonSyncCtx>> {
        self.non_sync_ctx.as_mut()
    }
}

impl FakeNetworkContext for FakeCtx {
    type TimerId = TimerId<FakeNonSyncCtx>;
    type SendMeta = EthernetWeakDeviceId<FakeNonSyncCtx>;
}

pub(crate) trait TestutilIpExt: Ip {
    fn icmp_replies(
        evt: &mut FakeNonSyncCtx,
    ) -> &mut HashMap<IcmpConnId<Self>, Vec<(u16, Vec<u8>)>>;
}

impl TestutilIpExt for Ipv4 {
    fn icmp_replies(
        FakeNonSyncCtx(evt): &mut FakeNonSyncCtx,
    ) -> &mut HashMap<IcmpConnId<Ipv4>, Vec<(u16, Vec<u8>)>> {
        &mut evt.state_mut().icmpv4_replies
    }
}

impl TestutilIpExt for Ipv6 {
    fn icmp_replies(
        FakeNonSyncCtx(evt): &mut FakeNonSyncCtx,
    ) -> &mut HashMap<IcmpConnId<Ipv6>, Vec<(u16, Vec<u8>)>> {
        &mut evt.state_mut().icmpv6_replies
    }
}

impl FakeNonSyncCtx {
    /// Takes all the received ICMP replies for a given `conn`.
    pub(crate) fn take_icmp_replies<I: TestutilIpExt>(
        &mut self,
        conn: IcmpConnId<I>,
    ) -> Vec<(u16, Vec<u8>)> {
        I::icmp_replies(self).remove(&conn).unwrap_or_else(Vec::default)
    }
}

impl<I: IcmpIpExt> udp::NonSyncContext<I> for FakeNonSyncCtx {}

impl<I: crate::ip::IpExt, B: BufferMut> udp::BufferNonSyncContext<I, B> for FakeNonSyncCtx {}

impl<I: IcmpIpExt> IcmpContext<I> for FakeNonSyncCtx {
    fn receive_icmp_error(&mut self, _conn: IcmpConnId<I>, _seq_num: u16, _err: I::ErrorCode) {
        unimplemented!()
    }
}

impl<B: BufferMut> BufferIcmpContext<Ipv4, B> for FakeNonSyncCtx {
    fn receive_icmp_echo_reply(
        &mut self,
        conn: IcmpConnId<Ipv4>,
        _src_ip: Ipv4Addr,
        _dst_ip: Ipv4Addr,
        _id: u16,
        seq_num: u16,
        data: B,
    ) {
        let replies = self.state_mut().icmpv4_replies.entry(conn).or_insert_with(Vec::default);
        replies.push((seq_num, data.as_ref().to_owned()))
    }
}

impl<B: BufferMut> BufferIcmpContext<Ipv6, B> for FakeNonSyncCtx {
    fn receive_icmp_echo_reply(
        &mut self,
        conn: IcmpConnId<Ipv6>,
        _src_ip: Ipv6Addr,
        _dst_ip: Ipv6Addr,
        _id: u16,
        seq_num: u16,
        data: B,
    ) {
        let replies = self.state_mut().icmpv6_replies.entry(conn).or_insert_with(Vec::default);
        replies.push((seq_num, data.as_ref().to_owned()))
    }
}

impl crate::device::socket::DeviceSocketTypes for FakeNonSyncCtx {
    type SocketState = ();
}

impl crate::device::socket::NonSyncContext<DeviceId<Self>> for FakeNonSyncCtx {
    fn receive_frame(
        &self,
        _state: &Self::SocketState,
        _device: &DeviceId<Self>,
        _frame: crate::device::socket::Frame<&[u8]>,
        _raw_frame: &[u8],
    ) {
    }
}

impl DeviceLayerStateTypes for FakeNonSyncCtx {
    type LoopbackDeviceState = ();
    type EthernetDeviceState = ();
}

impl DeviceLayerEventDispatcher for FakeNonSyncCtx {
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<FakeNonSyncCtx>) {
        self.state_mut().rx_available.push(device.clone());
    }

    fn wake_tx_task(&mut self, device: &DeviceId<FakeNonSyncCtx>) {
        self.state_mut().tx_available.push(device.clone());
    }

    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<FakeNonSyncCtx>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        self.frame_ctx_mut().push(device.downgrade(), frame.into_inner());
        Ok(())
    }
}

pub(crate) fn handle_queued_rx_packets(sync_ctx: &FakeSyncCtx, ctx: &mut FakeNonSyncCtx) {
    loop {
        let rx_available = core::mem::take(&mut ctx.state_mut().rx_available);
        if rx_available.len() == 0 {
            break;
        }

        for id in rx_available.into_iter() {
            crate::device::handle_queued_rx_packets(sync_ctx, ctx, &id);
        }
    }
}

/// Wraps all events emitted by Core into a single enum type.
#[derive(Debug, Eq, PartialEq, Hash)]
pub(crate) enum DispatchedEvent {
    IpDeviceIpv4(IpDeviceEvent<WeakDeviceId<FakeNonSyncCtx>, Ipv4>),
    IpDeviceIpv6(IpDeviceEvent<WeakDeviceId<FakeNonSyncCtx>, Ipv6>),
    IpLayerIpv4(IpLayerEvent<WeakDeviceId<FakeNonSyncCtx>, Ipv4>),
    IpLayerIpv6(IpLayerEvent<WeakDeviceId<FakeNonSyncCtx>, Ipv6>),
}

impl<I: Ip> From<IpDeviceEvent<DeviceId<FakeNonSyncCtx>, I>>
    for IpDeviceEvent<WeakDeviceId<FakeNonSyncCtx>, I>
{
    fn from(
        e: IpDeviceEvent<DeviceId<FakeNonSyncCtx>, I>,
    ) -> IpDeviceEvent<WeakDeviceId<FakeNonSyncCtx>, I> {
        match e {
            IpDeviceEvent::AddressAdded { device, addr, state } => {
                IpDeviceEvent::AddressAdded { device: device.downgrade(), addr, state }
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
        }
    }
}

impl From<IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv4>> for DispatchedEvent {
    fn from(e: IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv4>) -> DispatchedEvent {
        DispatchedEvent::IpDeviceIpv4(e.into())
    }
}

impl From<IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv6>> for DispatchedEvent {
    fn from(e: IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv6>) -> DispatchedEvent {
        DispatchedEvent::IpDeviceIpv6(e.into())
    }
}

impl<I: Ip> From<IpLayerEvent<DeviceId<FakeNonSyncCtx>, I>>
    for IpLayerEvent<WeakDeviceId<FakeNonSyncCtx>, I>
{
    fn from(
        e: IpLayerEvent<DeviceId<FakeNonSyncCtx>, I>,
    ) -> IpLayerEvent<WeakDeviceId<FakeNonSyncCtx>, I> {
        match e {
            IpLayerEvent::RouteAdded(Entry { subnet, device, gateway, metric }) => {
                IpLayerEvent::RouteAdded(Entry {
                    subnet,
                    device: device.downgrade(),
                    gateway,
                    metric,
                })
            }
            IpLayerEvent::RouteRemoved(Entry { subnet, device, gateway, metric }) => {
                IpLayerEvent::RouteRemoved(Entry {
                    subnet,
                    device: device.downgrade(),
                    gateway,
                    metric,
                })
            }
        }
    }
}

impl From<IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv4>> for DispatchedEvent {
    fn from(e: IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv4>) -> DispatchedEvent {
        DispatchedEvent::IpLayerIpv4(e.into())
    }
}

impl From<IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv6>> for DispatchedEvent {
    fn from(e: IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv6>) -> DispatchedEvent {
        DispatchedEvent::IpLayerIpv6(e.into())
    }
}

impl EventContext<IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv4>> for FakeNonSyncCtx {
    fn on_event(&mut self, event: IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv4>) {
        let Self(this) = self;
        this.on_event(DispatchedEvent::from(event))
    }
}

impl EventContext<IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv6>> for FakeNonSyncCtx {
    fn on_event(&mut self, event: IpLayerEvent<DeviceId<FakeNonSyncCtx>, Ipv6>) {
        let Self(this) = self;
        this.on_event(DispatchedEvent::from(event))
    }
}

impl EventContext<IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv4>> for FakeNonSyncCtx {
    fn on_event(&mut self, event: IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv4>) {
        let Self(this) = self;
        this.on_event(DispatchedEvent::from(event))
    }
}

impl EventContext<IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv6>> for FakeNonSyncCtx {
    fn on_event(&mut self, event: IpDeviceEvent<DeviceId<FakeNonSyncCtx>, Ipv6>) {
        let Self(this) = self;
        this.on_event(DispatchedEvent::from(event))
    }
}

pub(crate) fn handle_timer(
    FakeCtx { sync_ctx, non_sync_ctx }: &mut FakeCtx,
    _ctx: &mut (),
    id: TimerId<FakeNonSyncCtx>,
) {
    crate::handle_timer(sync_ctx, non_sync_ctx, id)
}

pub(crate) const IPV6_MIN_IMPLIED_MAX_FRAME_SIZE: ethernet::MaxFrameSize =
    const_unwrap::const_unwrap_option(ethernet::MaxFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU));

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
            socket::{BufferIpSocketHandler, DefaultSendOptions},
            BufferIpLayerHandler,
        },
        TimerIdInner,
    };

    #[test]
    fn test_fake_network_transmits_packets() {
        set_logger_for_test();
        let (alice_ctx, alice_device_ids) = FAKE_CONFIG_V4.into_builder().build();
        let (bob_ctx, bob_device_ids) = FAKE_CONFIG_V4.swap().into_builder().build();
        let mut net = crate::context::testutil::new_legacy_simple_fake_network(
            "alice",
            alice_ctx,
            alice_device_ids[0].clone().into(),
            "bob",
            bob_ctx,
            bob_device_ids[0].clone().into(),
        );
        core::mem::drop((alice_device_ids, bob_device_ids));

        // Alice sends Bob a ping.

        net.with_context("alice", |Ctx { sync_ctx, non_sync_ctx }| {
            BufferIpSocketHandler::<Ipv4, _, _>::send_oneshot_ip_packet(
                &mut Locked::new(&*sync_ctx),
                non_sync_ctx,
                None, // device
                None, // local_ip
                FAKE_CONFIG_V4.remote_ip,
                Ipv4Proto::Icmp,
                DefaultSendOptions,
                |_| {
                    let req = IcmpEchoRequest::new(0, 0);
                    let req_body = &[1, 2, 3, 4];
                    Buf::new(req_body.to_vec(), ..).encapsulate(
                        IcmpPacketBuilder::<Ipv4, &[u8], _>::new(
                            FAKE_CONFIG_V4.local_ip,
                            FAKE_CONFIG_V4.remote_ip,
                            IcmpUnusedCode,
                            req,
                        ),
                    )
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
        let mut net = crate::context::testutil::new_legacy_simple_fake_network(
            1,
            ctx_1,
            device_ids_1[0].clone().into(),
            2,
            ctx_2,
            device_ids_2[0].clone().into(),
        );
        core::mem::drop((device_ids_1, device_ids_2));

        net.with_context(1, |Ctx { sync_ctx: _, non_sync_ctx }| {
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(1), TimerId(TimerIdInner::Nop(1))),
                None
            );
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(4), TimerId(TimerIdInner::Nop(4))),
                None
            );
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(5), TimerId(TimerIdInner::Nop(5))),
                None
            );
        });

        net.with_context(2, |Ctx { sync_ctx: _, non_sync_ctx }| {
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(2), TimerId(TimerIdInner::Nop(2))),
                None
            );
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(3), TimerId(TimerIdInner::Nop(3))),
                None
            );
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(5), TimerId(TimerIdInner::Nop(6))),
                None
            );
        });

        // No timers fired before.
        assert_eq!(get_counter_val(net.non_sync_ctx(1), "timer::nop"), 0);
        assert_eq!(get_counter_val(net.non_sync_ctx(2), "timer::nop"), 0);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 1 should have fired.
        assert_eq!(get_counter_val(net.non_sync_ctx(1), "timer::nop"), 1);
        assert_eq!(get_counter_val(net.non_sync_ctx(2), "timer::nop"), 0);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 2 should have fired.
        assert_eq!(get_counter_val(net.non_sync_ctx(1), "timer::nop"), 1);
        assert_eq!(get_counter_val(net.non_sync_ctx(2), "timer::nop"), 1);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 2 should have fired.
        assert_eq!(get_counter_val(net.non_sync_ctx(1), "timer::nop"), 1);
        assert_eq!(get_counter_val(net.non_sync_ctx(2), "timer::nop"), 2);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 1);
        // Only timer in context 1 should have fired.
        assert_eq!(get_counter_val(net.non_sync_ctx(1), "timer::nop"), 2);
        assert_eq!(get_counter_val(net.non_sync_ctx(2), "timer::nop"), 2);
        assert_eq!(net.step(receive_frame, handle_timer).timers_fired, 2);
        // Both timers have fired at the same time.
        assert_eq!(get_counter_val(net.non_sync_ctx(1), "timer::nop"), 3);
        assert_eq!(get_counter_val(net.non_sync_ctx(2), "timer::nop"), 3);

        assert!(net.step(receive_frame, handle_timer).is_idle());
        // Check that current time on contexts tick together.
        let t1 = net.with_context(1, |Ctx { sync_ctx: _, non_sync_ctx }| non_sync_ctx.now());
        let t2 = net.with_context(2, |Ctx { sync_ctx: _, non_sync_ctx }| non_sync_ctx.now());
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_fake_network_until_idle() {
        set_logger_for_test();
        let (ctx_1, device_ids_1) = FAKE_CONFIG_V4.into_builder().build();
        let (ctx_2, device_ids_2) = FAKE_CONFIG_V4.swap().into_builder().build();
        let mut net = crate::context::testutil::new_legacy_simple_fake_network(
            1,
            ctx_1,
            device_ids_1[0].clone().into(),
            2,
            ctx_2,
            device_ids_2[0].clone().into(),
        );
        core::mem::drop((device_ids_1, device_ids_2));

        net.with_context(1, |Ctx { sync_ctx: _, non_sync_ctx }| {
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(1), TimerId(TimerIdInner::Nop(1))),
                None
            );
        });
        net.with_context(2, |Ctx { sync_ctx: _, non_sync_ctx }| {
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(2), TimerId(TimerIdInner::Nop(2))),
                None
            );
            assert_eq!(
                non_sync_ctx.schedule_timer(Duration::from_secs(3), TimerId(TimerIdInner::Nop(3))),
                None
            );
        });

        while !net.step(receive_frame, handle_timer).is_idle()
            && (get_counter_val(net.non_sync_ctx(1), "timer::nop") < 1
                || get_counter_val(net.non_sync_ctx(2), "timer::nop") < 1)
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
            move |net: &'static str, _device_id: EthernetWeakDeviceId<FakeNonSyncCtx>| {
                if net == "alice" {
                    vec![("bob", bob_device_id.clone(), Some(latency))]
                } else {
                    vec![("alice", alice_device_id.clone(), Some(latency))]
                }
            },
        );

        // Alice sends Bob a ping.
        net.with_context("alice", |Ctx { sync_ctx, non_sync_ctx }| {
            BufferIpSocketHandler::<Ipv4, _, _>::send_oneshot_ip_packet(
                &mut Locked::new(&*sync_ctx),
                non_sync_ctx,
                None, // device
                None, // local_ip
                FAKE_CONFIG_V4.remote_ip,
                Ipv4Proto::Icmp,
                DefaultSendOptions,
                |_| {
                    let req = IcmpEchoRequest::new(0, 0);
                    let req_body = &[1, 2, 3, 4];
                    Buf::new(req_body.to_vec(), ..).encapsulate(
                        IcmpPacketBuilder::<Ipv4, &[u8], _>::new(
                            FAKE_CONFIG_V4.local_ip,
                            FAKE_CONFIG_V4.remote_ip,
                            IcmpUnusedCode,
                            req,
                        ),
                    )
                },
                None,
            )
            .unwrap();
        });

        net.with_context("alice", |Ctx { sync_ctx: _, non_sync_ctx }| {
            assert_eq!(
                non_sync_ctx
                    .schedule_timer(Duration::from_millis(3), TimerId(TimerIdInner::Nop(1))),
                None
            );
        });
        net.with_context("bob", |Ctx { sync_ctx: _, non_sync_ctx }| {
            assert_eq!(
                non_sync_ctx
                    .schedule_timer(Duration::from_millis(7), TimerId(TimerIdInner::Nop(2))),
                None
            );
            assert_eq!(
                non_sync_ctx
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
                EthernetWeakDeviceId<FakeNonSyncCtx>,
                EthernetDeviceId<FakeNonSyncCtx>,
                &'a str,
            >,
        >(
            net: &mut FakeNetwork<&'a str, EthernetDeviceId<FakeNonSyncCtx>, FakeCtx, L>,
            alice_nop: usize,
            bob_nop: usize,
            bob_echo_request: usize,
            alice_echo_response: usize,
        ) {
            let alice = net.non_sync_ctx("alice");
            assert_eq!(get_counter_val(alice, "timer::nop"), alice_nop);
            assert_eq!(get_counter_val(alice, "<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::echo_reply"),
                alice_echo_response
            );

            let bob = net.non_sync_ctx("bob");
            assert_eq!(get_counter_val(bob, "timer::nop"), bob_nop);
            assert_eq!(get_counter_val(bob, "<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::echo_request"),
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
        sync_ctx: &'a FakeSyncCtx,
        ctx: &mut FakeNonSyncCtx,
        src_ip: SpecifiedAddr<A>,
        dst_ip: SpecifiedAddr<A>,
        device: &DeviceId<FakeNonSyncCtx>,
    ) where
        A::Version: TestIpExt,
        Locked<&'a FakeSyncCtx, crate::lock_ordering::Unlocked>: BufferIpLayerHandler<
            A::Version,
            FakeNonSyncCtx,
            Buf<Vec<u8>>,
            DeviceId = DeviceId<FakeNonSyncCtx>,
        >,
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
        BufferIpLayerHandler::<A::Version, _, _>::send_ip_packet_from_device(
            &mut Locked::new(sync_ctx),
            ctx,
            meta,
            Buf::new(vec![1, 2, 3, 4], ..),
        )
        .unwrap();
    }

    #[ip_test]
    fn test_send_to_many<I: Ip + TestIpExt>()
    where
        for<'a> Locked<&'a FakeSyncCtx, crate::lock_ordering::Unlocked>: BufferIpLayerHandler<
            I,
            FakeNonSyncCtx,
            Buf<Vec<u8>>,
            DeviceId = DeviceId<FakeNonSyncCtx>,
            WeakDeviceId = WeakDeviceId<FakeNonSyncCtx>,
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
        add_arp_or_ndp_table_entry(&mut alice, alice_device_idx, ip_b.get(), mac_b);
        add_arp_or_ndp_table_entry(&mut alice, alice_device_idx, ip_c.get(), mac_c);
        add_arp_or_ndp_table_entry(&mut bob, bob_device_idx, ip_a.get(), mac_a);
        add_arp_or_ndp_table_entry(&mut bob, bob_device_idx, ip_c.get(), mac_c);
        add_arp_or_ndp_table_entry(&mut calvin, calvin_device_idx, ip_a.get(), mac_a);
        add_arp_or_ndp_table_entry(&mut calvin, calvin_device_idx, ip_b.get(), mac_b);
        let (alice_ctx, alice_device_ids) = alice.build();
        let (bob_ctx, bob_device_ids) = bob.build();
        let (calvin_ctx, calvin_device_ids) = calvin.build();
        let alice_device_id = alice_device_ids[alice_device_idx].clone();
        let bob_device_id = bob_device_ids[bob_device_idx].clone();
        let calvin_device_id = calvin_device_ids[calvin_device_idx].clone();
        let mut net = FakeNetwork::new(
            [("alice", alice_ctx), ("bob", bob_ctx), ("calvin", calvin_ctx)],
            move |net: &'static str, _device_id: EthernetWeakDeviceId<FakeNonSyncCtx>| match net {
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
        assert_empty(net.non_sync_ctx("alice").frames_sent().iter());
        assert_empty(net.non_sync_ctx("bob").frames_sent().iter());
        assert_empty(net.non_sync_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());

        // Bob and Calvin should get any packet sent by Alice.

        net.with_context("alice", |Ctx { sync_ctx, non_sync_ctx }| {
            send_packet(sync_ctx, non_sync_ctx, ip_a, ip_b, &alice_device_id.clone().into());
        });
        assert_eq!(net.non_sync_ctx("alice").frames_sent().len(), 1);
        assert_empty(net.non_sync_ctx("bob").frames_sent().iter());
        assert_empty(net.non_sync_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());
        net.collect_frames();
        assert_empty(net.non_sync_ctx("alice").frames_sent().iter());
        assert_empty(net.non_sync_ctx("bob").frames_sent().iter());
        assert_empty(net.non_sync_ctx("calvin").frames_sent().iter());
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
        net.with_context("bob", |Ctx { sync_ctx, non_sync_ctx }| {
            send_packet(sync_ctx, non_sync_ctx, ip_b, ip_a, &bob_device_id.clone().into());
        });
        assert_empty(net.non_sync_ctx("alice").frames_sent().iter());
        assert_eq!(net.non_sync_ctx("bob").frames_sent().len(), 1);
        assert_empty(net.non_sync_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());
        net.collect_frames();
        assert_empty(net.non_sync_ctx("alice").frames_sent().iter());
        assert_empty(net.non_sync_ctx("bob").frames_sent().iter());
        assert_empty(net.non_sync_ctx("calvin").frames_sent().iter());
        assert_eq!(net.iter_pending_frames().count(), 1);
        assert!(net.iter_pending_frames().any(
            |InstantAndData(_, x)| (x.dst_context == "alice") && (&x.meta == &alice_device_id)
        ));

        // No one gets packets sent by Calvin.

        net.drop_pending_frames();
        net.with_context("calvin", |Ctx { sync_ctx, non_sync_ctx }| {
            send_packet(sync_ctx, non_sync_ctx, ip_c, ip_a, &calvin_device_id.clone().into());
        });
        assert_empty(net.non_sync_ctx("alice").frames_sent().iter());
        assert_empty(net.non_sync_ctx("bob").frames_sent().iter());
        assert_eq!(net.non_sync_ctx("calvin").frames_sent().len(), 1);
        assert_empty(net.iter_pending_frames());
        net.collect_frames();
        assert_empty(net.non_sync_ctx("alice").frames_sent().iter());
        assert_empty(net.non_sync_ctx("bob").frames_sent().iter());
        assert_empty(net.non_sync_ctx("calvin").frames_sent().iter());
        assert_empty(net.iter_pending_frames());
    }
}
