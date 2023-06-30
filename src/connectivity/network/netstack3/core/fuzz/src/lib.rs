// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(dead_code, unused_imports, unused_macros)]

extern crate netstack3_core_testutils as netstack3_core;

use core::{
    convert::{Infallible as Never, TryInto as _},
    time::Duration,
};

use arbitrary::{Arbitrary, Unstructured};
use fuzz_util::Fuzzed;
use net_declare::net_mac;
use net_types::{
    ip::{IpAddress, Ipv4Addr, Ipv6Addr},
    UnicastAddr,
};
use netstack3_core::{
    context::testutil::FakeTimerCtxExt as _,
    device::EthernetDeviceId,
    testutil::{Ctx, FakeCtx, FakeNonSyncCtx},
    TimerId,
};
use packet::{
    serialize::{Buf, Either, PacketBuilder, PacketConstraints, SerializeError},
    FragmentedBuffer, Serializer,
};
use packet_formats::{
    ethernet::EthernetFrameBuilder,
    ip::{IpExt, IpPacketBuilder},
    ipv4::Ipv4PacketBuilder,
    ipv6::Ipv6PacketBuilder,
    tcp::TcpSegmentBuilder,
    udp::UdpPacketBuilder,
};

mod print_on_panic {
    use core::fmt::Display;
    use std::sync::Mutex;

    use lazy_static::lazy_static;
    use tracing::Subscriber;

    use tracing_subscriber::{
        filter::LevelFilter,
        fmt::{
            format::{self, FormatEvent, FormatFields},
            FmtContext,
        },
        registry::LookupSpan,
    };

    lazy_static! {
        pub static ref PRINT_ON_PANIC: PrintOnPanicLog = PrintOnPanicLog::new();
        static ref PRINT_ON_PANIC_LOGGER: PrintOnPanicLogger = PrintOnPanicLogger;
    }

    /// LogLevel to output at configured at build time. Defaults to `LevelFilter::OFF`.
    const MAX_LOG_LEVEL: LevelFilter = if cfg!(feature = "log_trace") {
        LevelFilter::TRACE
    } else if cfg!(feature = "log_debug") {
        LevelFilter::DEBUG
    } else if cfg!(feature = "log_info") {
        LevelFilter::INFO
    } else {
        LevelFilter::OFF
    };

    /// A simple log whose contents get printed to stdout on panic.
    ///
    /// The singleton instance of this can be obtained via the static singleton
    /// [`PRINT_ON_PANIC`].
    pub struct PrintOnPanicLog(Mutex<Vec<String>>);

    impl PrintOnPanicLog {
        /// Constructs the singleton log instance.
        fn new() -> Self {
            let default_hook = std::panic::take_hook();

            std::panic::set_hook(Box::new(move |panic_info| {
                let Self(mutex): &PrintOnPanicLog = &PRINT_ON_PANIC;
                let dispatched = core::mem::take(&mut *mutex.lock().unwrap());
                for o in dispatched.into_iter() {
                    println!("{}", o);
                }

                // Resume panicking normally.
                (*default_hook)(panic_info);
            }));
            Self(Mutex::new(Vec::new()))
        }

        /// Adds an entry to the log.
        fn record<T: Display>(&self, t: &T) {
            let Self(mutex) = self;
            mutex.lock().unwrap().push(t.to_string());
        }

        /// Clears the log.
        pub fn clear_log(&self) {
            let Self(mutex) = self;
            mutex.lock().unwrap().clear();
        }
    }

    struct PrintOnPanicFormatter;
    impl<S, N> FormatEvent<S, N> for PrintOnPanicFormatter
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
            let level = *event.metadata().level();
            let path = event.metadata().module_path().unwrap_or("_unknown_");
            write!(writer, "[{path}][{level}] ")?;
            ctx.field_format().format_fields(writer.by_ref(), event)
        }
    }

    struct PrintOnPanicLogger;
    impl std::io::Write for PrintOnPanicLogger {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            PRINT_ON_PANIC.record(&String::from_utf8_lossy(buf));
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Tests if the given `level` is enabled.
    pub fn log_enabled(level: tracing::Level) -> bool {
        MAX_LOG_LEVEL >= level
    }

    /// Initializes the [`log`] crate so that all logs at or above the given
    /// severity level get written to [`PRINT_ON_PANIC`].
    ///
    /// When
    pub fn initialize_logging() {
        if MAX_LOG_LEVEL != LevelFilter::OFF {
            static LOGGER_ONCE: core::sync::atomic::AtomicBool =
                core::sync::atomic::AtomicBool::new(true);

            // This function gets called on every fuzz iteration, but we only need to set up logging the
            // first time.
            if LOGGER_ONCE.swap(false, core::sync::atomic::Ordering::AcqRel) {
                tracing_subscriber::fmt()
                    .event_format(PrintOnPanicFormatter)
                    .with_writer(|| PrintOnPanicLogger)
                    .with_max_level(MAX_LOG_LEVEL)
                    .init();
                println!("Saving {:?} logs in case of panic", MAX_LOG_LEVEL);
            };
        }
    }
}

/// Wrapper around Duration that limits the range of possible values. This keeps the fuzzer
/// from generating Duration values that, when added up, would cause overflow.
struct SmallDuration(Duration);

impl<'a> Arbitrary<'a> for SmallDuration {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // The maximum time increment to advance by in a single step.
        const MAX_DURATION: Duration = Duration::from_secs(60 * 60);

        let max = MAX_DURATION.as_nanos().try_into().unwrap();
        let nanos = u.int_in_range::<u64>(0..=max)?;
        Ok(Self(Duration::from_nanos(nanos)))
    }
}

#[derive(Copy, Clone, Debug, Arbitrary)]
enum FrameType {
    Raw,
    EthernetWith(EthernetFrameType),
}

#[derive(Copy, Clone, Debug, Arbitrary)]
enum EthernetFrameType {
    Raw,
    Ipv4(IpFrameType),
    Ipv6(IpFrameType),
}

#[derive(Copy, Clone, Debug, Arbitrary)]
enum IpFrameType {
    Raw,
    Udp,
    Tcp,
}

impl FrameType {
    fn arbitrary_buf(&self, u: &mut Unstructured<'_>) -> arbitrary::Result<(Buf<Vec<u8>>, String)> {
        match self {
            FrameType::Raw => Ok((Buf::new(u.arbitrary()?, ..), "[raw]".into())),
            FrameType::EthernetWith(ether_type) => {
                let builder = Fuzzed::<EthernetFrameBuilder>::arbitrary(u)?.into();
                ether_type.arbitrary_buf(builder, u)
            }
        }
    }
}

impl EthernetFrameType {
    fn arbitrary_buf<O: PacketBuilder + core::fmt::Debug>(
        &self,
        outer: O,
        u: &mut Unstructured<'_>,
    ) -> arbitrary::Result<(Buf<Vec<u8>>, String)> {
        match self {
            EthernetFrameType::Raw => arbitrary_packet((outer,), u),
            EthernetFrameType::Ipv4(ip_type) => {
                ip_type.arbitrary_buf::<Ipv4Addr, Ipv4PacketBuilder, _>(outer, u)
            }
            EthernetFrameType::Ipv6(ip_type) => {
                ip_type.arbitrary_buf::<Ipv6Addr, Ipv6PacketBuilder, _>(outer, u)
            }
        }
    }
}

impl IpFrameType {
    fn arbitrary_buf<
        'a,
        A: IpAddress,
        IPB: IpPacketBuilder<A::Version>,
        O: PacketBuilder + core::fmt::Debug,
    >(
        &self,
        outer: O,
        u: &mut Unstructured<'a>,
    ) -> arbitrary::Result<(Buf<Vec<u8>>, String)>
    where
        A::Version: IpExt,
        Fuzzed<IPB>: Arbitrary<'a>,
    {
        match self {
            IpFrameType::Raw => arbitrary_packet((outer,), u),
            IpFrameType::Udp => {
                // Note that the UDP checksum includes parameters from the IP
                // layer (source and destination address), and so it's important
                // that the same parameters are used to generate builders for
                // both layers.
                let ip = Fuzzed::<IPB>::arbitrary(u)?.into();
                let udp =
                    UdpPacketBuilder::new(ip.src_ip(), ip.dst_ip(), u.arbitrary()?, u.arbitrary()?);
                arbitrary_packet((udp, ip, outer), u)
            }
            IpFrameType::Tcp => {
                // Note that the TCP checksum includes parameters from the IP
                // layer (source and destination address), and so it's important
                // that the same parameters are used to generate builders for
                // both layers.
                let ip = Fuzzed::<IPB>::arbitrary(u)?.into();
                let mut tcp = TcpSegmentBuilder::new(
                    ip.src_ip(),
                    ip.dst_ip(),
                    u.arbitrary()?,
                    u.arbitrary()?,
                    u.arbitrary()?,
                    u.arbitrary()?,
                    u.arbitrary()?,
                );
                tcp.psh(u.arbitrary()?);
                tcp.rst(u.arbitrary()?);
                tcp.syn(u.arbitrary()?);
                tcp.fin(u.arbitrary()?);
                arbitrary_packet((tcp, ip, outer), u)
            }
        }
    }
}

struct ArbitraryFrame {
    frame_type: FrameType,
    buf: Buf<Vec<u8>>,
    description: String,
}

impl<'a> Arbitrary<'a> for ArbitraryFrame {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let frame_type = u.arbitrary::<FrameType>()?;
        let (buf, description) = frame_type.arbitrary_buf(u)?;
        Ok(Self { frame_type, buf, description })
    }
}

#[derive(Arbitrary)]
enum FuzzAction {
    ReceiveFrame(ArbitraryFrame),
    AdvanceTime(SmallDuration),
}

#[derive(Arbitrary)]
pub(crate) struct FuzzInput {
    actions: Vec<FuzzAction>,
}

impl core::fmt::Display for FuzzAction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FuzzAction::ReceiveFrame(ArbitraryFrame { frame_type, buf, description }) => write!(
                f,
                "Send {:?} frame with {} total bytes: {}",
                frame_type,
                buf.len(),
                description
            ),
            FuzzAction::AdvanceTime(SmallDuration(duration)) => {
                write!(f, "Advance time by {:?}", duration)
            }
        }
    }
}

// A `PacketBuilder` or multiple `PacketBuilder`s which will be encapsulated in
// sequence.
trait FuzzablePacket {
    fn try_constraints(&self) -> Option<PacketConstraints>;

    fn serialize(self, buf: Buf<Vec<u8>>) -> Result<Buf<Vec<u8>>, SerializeError<Never>>;
}

// Implement for `(B,)` rather than for `B` to avoid a blanket impl conflict.
impl<B: PacketBuilder> FuzzablePacket for (B,) {
    fn try_constraints(&self) -> Option<PacketConstraints> {
        Some(self.0.constraints())
    }

    fn serialize(self, buf: Buf<Vec<u8>>) -> Result<Buf<Vec<u8>>, SerializeError<Never>> {
        buf.encapsulate(self.0)
            .serialize_vec_outer()
            .map(Either::into_inner)
            .map_err(|(err, _ser)| err)
    }
}

impl<BA: PacketBuilder, BB: PacketBuilder, BC: PacketBuilder> FuzzablePacket for (BA, BB, BC) {
    fn try_constraints(&self) -> Option<PacketConstraints> {
        let (a, b, c) = self;
        a.constraints()
            .try_encapsulate(&b.constraints())
            .and_then(|constraints| constraints.try_encapsulate(&c.constraints()))
    }

    fn serialize(self, buf: Buf<Vec<u8>>) -> Result<Buf<Vec<u8>>, SerializeError<Never>> {
        let (a, b, c) = self;
        buf.encapsulate(a)
            .encapsulate(b)
            .encapsulate(c)
            .serialize_vec_outer()
            .map(Either::into_inner)
            .map_err(|(err, _ser)| err)
    }
}

fn arbitrary_packet<P: FuzzablePacket + std::fmt::Debug>(
    packet: P,
    u: &mut Unstructured<'_>,
) -> arbitrary::Result<(Buf<Vec<u8>>, String)> {
    let constraints = packet.try_constraints().ok_or(arbitrary::Error::IncorrectFormat)?;
    let body_len = core::cmp::min(
        core::cmp::max(u.arbitrary_len::<u8>()?, constraints.min_body_len()),
        constraints.max_body_len(),
    );

    // Generate a description that is used for logging. If logging is disabled,
    // the value here will never be printed. `String::new()` does not allocate,
    // so use that to save CPU and memory when the value would otherwise be
    // thrown away.
    let description = if print_on_panic::log_enabled(tracing::Level::INFO) {
        format!("{:?} with body length {}", packet, body_len)
    } else {
        String::new()
    };

    let mut buffer = vec![0; body_len + constraints.header_len() + constraints.footer_len()];
    u.fill_buffer(&mut buffer[constraints.header_len()..(constraints.header_len() + body_len)])?;

    let bytes = packet
        .serialize(Buf::new(
            buffer,
            constraints.header_len()..(constraints.header_len() + body_len),
        ))
        .map_err(|e| match e {
            SerializeError::Alloc(e) => match e {},
            SerializeError::SizeLimitExceeded => arbitrary::Error::IncorrectFormat,
        })?;
    Ok((bytes, description))
}

fn dispatch(
    Ctx { sync_ctx, non_sync_ctx }: &mut FakeCtx,
    device_id: &EthernetDeviceId<FakeNonSyncCtx>,
    action: FuzzAction,
) {
    use FuzzAction::*;
    match action {
        ReceiveFrame(ArbitraryFrame { frame_type: _, buf, description: _ }) => {
            netstack3_core::device::receive_frame(sync_ctx, non_sync_ctx, device_id, buf)
        }
        AdvanceTime(SmallDuration(duration)) => {
            let _: Vec<TimerId<_>> = non_sync_ctx
                .trigger_timers_for(duration, |non_sync_ctx, id| {
                    netstack3_core::handle_timer(sync_ctx, non_sync_ctx, id)
                });
        }
    }
}

#[::fuzz::fuzz]
pub(crate) fn single_device_arbitrary_packets(input: FuzzInput) {
    print_on_panic::initialize_logging();

    let mut builder = netstack3_core::testutil::FakeEventDispatcherBuilder::default();
    let device_index = builder.add_device(UnicastAddr::new(net_mac!("10:20:30:40:50:60")).unwrap());

    let (mut ctx, ethernet_devices) = builder.build();
    let device_id = &ethernet_devices[device_index];

    let FuzzInput { actions } = input;

    tracing::info!("Processing {} actions", actions.len());
    for action in actions {
        tracing::info!("{}", action);
        dispatch(&mut ctx, device_id, action);
    }

    // No panic occurred, so clear the log for the next run.
    print_on_panic::PRINT_ON_PANIC.clear_log();
}
