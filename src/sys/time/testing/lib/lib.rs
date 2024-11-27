// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use chrono::{Datelike, TimeZone, Timelike};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_hardware_rtc::{DeviceRequest, DeviceRequestStream};
use fidl_fuchsia_metrics::MetricEvent;
use fidl_fuchsia_metrics_test::{
    LogMethod, MetricEventLoggerQuerierMarker, MetricEventLoggerQuerierProxy,
};
use fidl_fuchsia_testing::{
    FakeClockControlMarker, FakeClockControlProxy, FakeClockMarker, FakeClockProxy,
};
use fidl_fuchsia_time::{MaintenanceRequest, MaintenanceRequestStream};
use fidl_fuchsia_time_external::{PushSourceMarker, Status, TimeSample};
use fidl_test_time::{TimeSourceControlRequest, TimeSourceControlRequestStream};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, ChildRef, LocalComponentHandles, RealmBuilder, RealmInstance, Ref,
    Route,
};
use fuchsia_sync::Mutex;
use futures::channel::mpsc::Sender;
use futures::stream::{Stream, StreamExt, TryStreamExt};
use futures::{Future, FutureExt, SinkExt};
use lazy_static::lazy_static;
use push_source::{PushSource, TestUpdateAlgorithm, Update};
use std::ops::Deref;
use std::sync::Arc;
use time_metrics_registry::PROJECT_ID;
use vfs::directory::entry_container::Directory;
use vfs::execution_scope::ExecutionScope;
use vfs::pseudo_directory;
use zx::{self as zx, HandleBased, Rights};
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

/// URL for timekeeper.
const TIMEKEEPER_URL: &str = "#meta/timekeeper_for_integration.cm";
/// URL for timekeeper with fake time.
const TIMEKEEPER_FAKE_TIME_URL: &str = "#meta/timekeeper_with_fake_time.cm";
/// URL for fake cobalt.
const COBALT_URL: &str = "#meta/fake_cobalt.cm";
/// URL for the fake clock component.
const FAKE_CLOCK_URL: &str = "#meta/fake_clock.cm";

/// A reference to a timekeeper running inside a nested environment which runs fake versions of
/// the services timekeeper requires.
pub struct NestedTimekeeper {
    _realm_instance: RealmInstance,
}

impl Into<RealmInstance> for NestedTimekeeper {
    // Deconstructs [Self] into an underlying [RealmInstance].
    fn into(self) -> RealmInstance {
        self._realm_instance
    }
}

impl NestedTimekeeper {
    /// Creates a new [NestedTimekeeper].
    ///
    /// Launches an instance of timekeeper maintaining the provided |clock| in a nested
    /// environment.
    ///
    /// If |initial_rtc_time| is provided, then the environment contains a fake RTC
    /// device that reports the time as |initial_rtc_time|.
    ///
    /// If use_fake_clock is true, also launches a fake monotonic clock service.
    ///
    /// Returns a `NestedTimekeeper`, handles to the PushSource and RTC it obtains updates from,
    /// Cobalt debug querier, and a fake clock control handle if use_fake_clock is true.
    pub async fn new(
        clock: Arc<zx::Clock>,
        rtc_options: RtcOptions,
        use_fake_clock: bool,
    ) -> (
        Self,
        Arc<PushSourcePuppet>,
        RtcUpdates,
        MetricEventLoggerQuerierProxy,
        Option<FakeClockController>,
    ) {
        let push_source_puppet = Arc::new(PushSourcePuppet::new());

        let builder = RealmBuilder::new().await.unwrap();
        let fake_cobalt =
            builder.add_child("fake_cobalt", COBALT_URL, ChildOptions::new()).await.unwrap();

        let timekeeper_url = if use_fake_clock { TIMEKEEPER_FAKE_TIME_URL } else { TIMEKEEPER_URL };
        tracing::trace!("using timekeeper_url: {}", timekeeper_url);
        let timekeeper = builder
            .add_child("timekeeper_test", timekeeper_url, ChildOptions::new().eager())
            .await
            .with_context(|| format!("while starting up timekeeper_test from: {timekeeper_url}"))
            .unwrap();

        let timesource_server = builder
            .add_local_child(
                "timesource_mock",
                {
                    let push_source_puppet = Arc::clone(&push_source_puppet);
                    move |handles: LocalComponentHandles| {
                        Box::pin(timesource_mock_server(handles, Arc::clone(&push_source_puppet)))
                    }
                },
                ChildOptions::new(),
            )
            .await
            .context("while starting up timesource_mock")
            .unwrap();

        let maintenance_server = builder
            .add_local_child(
                "maintenance_mock",
                move |handles: LocalComponentHandles| {
                    Box::pin(maintenance_mock_server(handles, Arc::clone(&clock)))
                },
                ChildOptions::new(),
            )
            .await
            .context("while starting up maintenance_mock")
            .unwrap();

        // Launch fake clock if needed.
        if use_fake_clock {
            let fake_clock =
                builder.add_child("fake_clock", FAKE_CLOCK_URL, ChildOptions::new()).await.unwrap();

            builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name(
                            "fuchsia.testing.FakeClockControl",
                        ))
                        .from(&fake_clock)
                        .to(Ref::parent()),
                )
                .await
                .context("while setting up FakeClockControl")
                .unwrap();

            builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.testing.FakeClock"))
                        .from(&fake_clock)
                        .to(Ref::parent())
                        .to(&timekeeper),
                )
                .await
                .context("while setting up FakeClock")
                .unwrap();

            builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                        .from(Ref::parent())
                        .to(&fake_clock),
                )
                .await
                .context("while setting up LogSink")
                .unwrap();
        };

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.time.Maintenance"))
                    .from(&maintenance_server)
                    .to(&timekeeper),
            )
            .await
            .context("while setting up Maintenance")
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("test.time.TimeSourceControl"))
                    .from(&timesource_server)
                    .to(&timekeeper),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name(
                        "fuchsia.metrics.test.MetricEventLoggerQuerier",
                    ))
                    .from(&fake_cobalt)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name(
                        "fuchsia.metrics.MetricEventLoggerFactory",
                    ))
                    .from(&fake_cobalt)
                    .to(&timekeeper),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .from(Ref::parent())
                    .to(&fake_cobalt)
                    .to(&timekeeper)
                    .to(&timesource_server)
                    .to(&maintenance_server),
            )
            .await
            .unwrap();

        let rtc_updates = setup_rtc(rtc_options, &builder, &timekeeper).await;
        let realm_instance = builder.build().await.unwrap();

        let fake_clock_control = if use_fake_clock {
            let control_proxy = realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<FakeClockControlMarker>()
                .unwrap();
            let clock_proxy = realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<FakeClockMarker>()
                .unwrap();
            Some(FakeClockController { control_proxy, clock_proxy })
        } else {
            None
        };

        let cobalt_querier = realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<MetricEventLoggerQuerierMarker>()
            .expect("the connection succeeds");

        let nested_timekeeper = Self { _realm_instance: realm_instance };

        (nested_timekeeper, push_source_puppet, rtc_updates, cobalt_querier, fake_clock_control)
    }
}

pub struct RemotePushSourcePuppet {
    proxy: fidl_test_time_realm::PushSourcePuppetProxy,
}

impl RemotePushSourcePuppet {
    /// Creates a new [RemotePushSourcePuppet].
    pub fn new(proxy: fidl_test_time_realm::PushSourcePuppetProxy) -> Arc<Self> {
        Arc::new(Self { proxy })
    }

    /// Set the next sample reported by the time source.
    pub async fn set_sample(&self, sample: TimeSample) {
        self.proxy.set_sample(&sample).await.expect("original API was infallible");
    }

    /// Set the next status reported by the time source.
    pub async fn set_status(&self, status: Status) {
        self.proxy.set_status(status).await.expect("original API was infallible");
    }

    /// Simulate a crash by closing client channels and wiping state.
    pub async fn simulate_crash(&self) {
        self.proxy.crash().await.expect("original local API was infallible");
    }

    /// Returns the number of cumulative connections served. This allows asserting
    /// behavior such as whether Timekeeper has restarted a connection.
    pub async fn lifetime_served_connections(&self) -> u32 {
        self.proxy.get_lifetime_served_connections().await.expect("original API was infallible")
    }
}

/// A `PushSource` that allows a single client and can be controlled by a test.
pub struct PushSourcePuppet {
    /// Internal state for the current PushSource. May be dropped and replaced
    /// to clear all state.
    inner: Mutex<PushSourcePuppetInner>,
    /// The number of client connections received over the lifetime of the puppet.
    cumulative_clients: Mutex<u32>,
}

impl PushSourcePuppet {
    /// Create a new `PushSourcePuppet`.
    fn new() -> Self {
        Self { inner: Mutex::new(PushSourcePuppetInner::new()), cumulative_clients: Mutex::new(0) }
    }

    /// Serve the `PushSource` service to a client.
    fn serve_client(&self, server_end: ServerEnd<PushSourceMarker>) {
        tracing::debug!("serve_client entry");
        let mut inner = self.inner.lock();
        // Timekeeper should only need to connect to a push source once, except when it is
        // restarting a time source. This case appears to the test as a second connection to the
        // puppet. Since the puppet is restarted, all its state should be cleared as well.
        if inner.served_client() {
            *inner = PushSourcePuppetInner::new();
        }
        inner.serve_client(server_end);
        *self.cumulative_clients.lock() += 1;
    }

    /// Set the next sample reported by the time source.
    pub async fn set_sample(&self, sample: TimeSample) {
        let mut sink = self.inner.lock().get_sink();
        sink.send(sample.into()).await.unwrap();
    }

    /// Set the next status reported by the time source.
    pub async fn set_status(&self, status: Status) {
        let mut sink = self.inner.lock().get_sink();
        sink.send(status.into()).await.unwrap();
    }

    /// Simulate a crash by closing client channels and wiping state.
    pub fn simulate_crash(&self) {
        *self.inner.lock() = PushSourcePuppetInner::new();
        // This drops the old inner and cleans up any tasks it owns.
    }

    /// Returns the number of cumulative connections served. This allows asserting
    /// behavior such as whether Timekeeper has restarted a connection.
    pub fn lifetime_served_connections(&self) -> u32 {
        *self.cumulative_clients.lock()
    }
}

/// Internal state for a PushSourcePuppet. This struct contains a PushSource and
/// all Tasks needed for it to serve requests,
struct PushSourcePuppetInner {
    push_source: Arc<PushSource<TestUpdateAlgorithm>>,
    /// Tasks serving PushSource clients.
    tasks: Vec<fasync::Task<()>>,
    /// Sink through which updates are passed to the PushSource.
    update_sink: Sender<Update>,
}

impl PushSourcePuppetInner {
    fn new() -> Self {
        let (update_algorithm, update_sink) = TestUpdateAlgorithm::new();
        let push_source = Arc::new(PushSource::new(update_algorithm, Status::Ok).unwrap());
        let push_source_clone = Arc::clone(&push_source);
        let tasks = vec![fasync::Task::spawn(async move {
            push_source_clone.poll_updates().await.unwrap();
        })];
        Self { push_source, tasks, update_sink }
    }

    /// Returns true if this puppet has or is currently serving a client.
    fn served_client(&self) -> bool {
        self.tasks.len() > 1
    }

    /// Serve the `PushSource` service to a client.
    fn serve_client(&mut self, server_end: ServerEnd<PushSourceMarker>) {
        let push_source_clone = Arc::clone(&self.push_source);
        self.tasks.push(fasync::Task::spawn(async move {
            push_source_clone
                .handle_requests_for_stream(server_end.into_stream().unwrap())
                .await
                .unwrap();
        }));
    }

    /// Obtains the sink used to send commands to the push source puppet.
    ///
    /// The sink is detached from the puppet, so can be used whenever needed
    /// without locking.
    fn get_sink(&self) -> Sender<Update> {
        self.update_sink.clone()
    }
}

/// The list of RTC update requests received by a `NestedTimekeeper`.
#[derive(Clone, Debug)]
pub struct RtcUpdates(Arc<Mutex<Vec<fidl_fuchsia_hardware_rtc::Time>>>);

impl RtcUpdates {
    /// Get all received RTC times as a vec.
    pub fn to_vec(&self) -> Vec<fidl_fuchsia_hardware_rtc::Time> {
        self.0.lock().clone()
    }
}

/// Remote RTC updates - peek into the life of the RTC on the other side of a
/// RTC connection.
pub struct RemoteRtcUpdates {
    proxy: fidl_test_time_realm::RtcUpdatesProxy,
}

impl RemoteRtcUpdates {
    pub async fn to_vec(&self) -> Vec<fidl_fuchsia_hardware_rtc::Time> {
        self.proxy
            .get(fidl_test_time_realm::GetRequest::default())
            .await
            .expect("no errors or overflows") // Original API was infallible.
            .unwrap()
            .0
    }
    pub fn new(proxy: fidl_test_time_realm::RtcUpdatesProxy) -> Self {
        RemoteRtcUpdates { proxy }
    }
}

/// A wrapper around a `FakeClockControlProxy` that also allows a client to read
/// the current fake time.
pub struct FakeClockController {
    control_proxy: FakeClockControlProxy,
    clock_proxy: FakeClockProxy,
}

impl Deref for FakeClockController {
    type Target = FakeClockControlProxy;

    fn deref(&self) -> &Self::Target {
        &self.control_proxy
    }
}

impl FakeClockController {
    /// Re-constructs FakeClockController from the constituents.
    pub fn new(control_proxy: FakeClockControlProxy, clock_proxy: FakeClockProxy) -> Self {
        FakeClockController { control_proxy, clock_proxy }
    }

    /// Deconstructs [Self] into fake clock proxies.
    pub fn into_components(self) -> (FakeClockControlProxy, FakeClockProxy) {
        (self.control_proxy, self.clock_proxy)
    }

    pub async fn get_monotonic(&self) -> Result<i64, fidl::Error> {
        self.clock_proxy.get().await
    }

    /// Returns the current fake instant on the reference timeline.
    pub async fn get_reference(&self) -> Result<zx::BootInstant, fidl::Error> {
        self.get_monotonic().await.map(|v| zx::BootInstant::from_nanos(v))
    }
}

/// The RTC configuration options.
pub enum RtcOptions {
    /// No real-time clock available. This configuration simulates a system that
    /// does not have a RTC circuit available.
    None,
    /// Fake real-time clock. Supplied initial RTC time to report.
    InitialRtcTime(zx::SyntheticInstant),
    /// Injected real-time clock.
    ///
    /// This is the handle that will appear as the directory
    /// `/dev/class/rtc` in the Timekeeper's namespace.
    ///
    /// The caller must set this directory up so that it serves
    /// a RTC device (e.g. named `/dev/class/rtc/000`, and serving
    /// the FIDL `fuchsia.hardware.rtc/Device`) from this directory.
    ///
    /// It is also possible to serve more RTCs from the directory, or
    /// other files and file types at the caller's option.
    ///
    /// Use this option if you need to implement corner cases, or
    /// very specific RTC behavior, such as abnormal configuration
    /// or anomalous behavior.
    InjectedRtc(fidl_fuchsia_io::DirectoryProxy),
}

impl From<fidl_test_time_realm::RtcOptions> for RtcOptions {
    fn from(value: fidl_test_time_realm::RtcOptions) -> Self {
        match value {
            fidl_test_time_realm::RtcOptions::DevClassRtc(h) => {
                RtcOptions::InjectedRtc(h.into_proxy())
            }
            fidl_test_time_realm::RtcOptions::InitialRtcTime(t) => {
                RtcOptions::InitialRtcTime(zx::SyntheticInstant::from_nanos(t))
            }
            _ => unimplemented!(),
        }
    }
}

impl From<zx::SyntheticInstant> for RtcOptions {
    fn from(value: zx::SyntheticInstant) -> Self {
        RtcOptions::InitialRtcTime(value)
    }
}

impl From<Option<zx::SyntheticInstant>> for RtcOptions {
    fn from(value: Option<zx::SyntheticInstant>) -> Self {
        value.map(|t| t.into()).unwrap_or(Self::None)
    }
}

/// Sets up the RTC serving.
///
/// Args:
/// - `rtc_options`: options for RTC setup.
/// - `build`: the `RealmBuilder` that will construct the realm.
/// - `timekeeper`: the Timekeeper component instance.
///
/// Returns:
/// - `RtcUpdates`: A vector of RTC updates received from a fake RTC. If the
///   client serves the RTC directory, then the return value is useless.
async fn setup_rtc(
    rtc_options: RtcOptions,
    builder: &RealmBuilder,
    timekeeper: &ChildRef,
) -> RtcUpdates {
    let rtc_updates = RtcUpdates(Arc::new(Mutex::new(vec![])));

    let rtc_dir = match rtc_options {
        RtcOptions::InitialRtcTime(initial_time) => {
            tracing::debug!("using fake /dev/class/rtc/000");
            pseudo_directory! {
                "class" => pseudo_directory! {
                    "rtc" => pseudo_directory! {
                        "000" => vfs::service::host({
                            let rtc_updates = rtc_updates.clone();
                            move |stream| {
                                serve_fake_rtc(initial_time, rtc_updates.clone(), stream)
                            }
                        })
                    }
                }
            }
        }
        RtcOptions::None => {
            tracing::debug!("using an empty /dev/class/rtc directory");
            pseudo_directory! {
                "class" => pseudo_directory! {
                    "rtc" => pseudo_directory! {
                    }
                }
            }
        }
        RtcOptions::InjectedRtc(h) => {
            tracing::debug!("using /dev/class/rtc provided by client");
            pseudo_directory! {
                "class" => pseudo_directory! {
                    "rtc" => vfs::remote::remote_dir(h)
                }
            }
        }
    };

    let fake_rtc_server = builder
        .add_local_child(
            "fake_rtc",
            {
                move |handles| {
                    let rtc_dir = rtc_dir.clone();
                    async move {
                        let _ = &handles;
                        let scope = ExecutionScope::new();
                        let (client_end, server_end) =
                            fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
                        let () = rtc_dir.open(
                            scope.clone(),
                            fio::OpenFlags::RIGHT_READABLE
                                | fio::OpenFlags::RIGHT_WRITABLE
                                | fio::OpenFlags::RIGHT_EXECUTABLE,
                            vfs::path::Path::dot(),
                            ServerEnd::new(server_end.into_channel()),
                        );
                        let mut fs = ServiceFs::new();
                        fs.add_remote("dev", client_end.into_proxy());
                        fs.serve_connection(handles.outgoing_dir)
                            .expect("failed to serve fake RTC ServiceFs");
                        fs.collect::<()>().await;
                        Ok(())
                    }
                    .boxed()
                }
            },
            ChildOptions::new().eager(),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("dev-topological").path("/dev").rights(fio::RW_STAR_DIR),
                )
                .from(&fake_rtc_server)
                .to(&*timekeeper),
        )
        .await
        .unwrap();

    rtc_updates
}

async fn serve_fake_rtc(
    initial_time: zx::SyntheticInstant,
    rtc_updates: RtcUpdates,
    mut stream: DeviceRequestStream,
) {
    while let Some(req) = stream.try_next().await.unwrap() {
        match req {
            DeviceRequest::Get { responder } => {
                tracing::debug!("serve_fake_rtc: DeviceRequest::Get");
                // Since timekeeper only pulls a time off of the RTC device once on startup, we
                // don't attempt to update the sent time.
                responder.send(Ok(&zx_time_to_rtc_time(initial_time))).unwrap();
            }
            DeviceRequest::Set { rtc, responder } => {
                tracing::debug!("serve_fake_rtc: DeviceRequest::Set");
                rtc_updates.0.lock().push(rtc);
                responder.send(zx::Status::OK.into_raw()).unwrap();
            }
            DeviceRequest::_UnknownMethod { .. } => {}
        }
    }
}

async fn serve_test_control(puppet: &PushSourcePuppet, stream: TimeSourceControlRequestStream) {
    stream
        .try_for_each_concurrent(None, |req| async {
            let _ = &req;
            let TimeSourceControlRequest::ConnectPushSource { push_source, .. } = req;
            puppet.serve_client(push_source);
            Ok(())
        })
        .await
        .unwrap();
}

async fn serve_maintenance(clock_handle: Arc<zx::Clock>, mut stream: MaintenanceRequestStream) {
    while let Some(req) = stream.try_next().await.unwrap() {
        let MaintenanceRequest::GetWritableUtcClock { responder } = req;
        responder.send(clock_handle.duplicate_handle(Rights::SAME_RIGHTS).unwrap()).unwrap();
    }
}

async fn timesource_mock_server(
    handles: LocalComponentHandles,
    push_source_puppet: Arc<PushSourcePuppet>,
) -> Result<(), anyhow::Error> {
    let mut fs = ServiceFs::new();
    let mut tasks = vec![];

    fs.dir("svc").add_fidl_service(move |stream: TimeSourceControlRequestStream| {
        let puppet_clone = Arc::clone(&push_source_puppet);

        tasks.push(fasync::Task::local(async move {
            serve_test_control(&*puppet_clone, stream).await;
        }));
    });

    fs.serve_connection(handles.outgoing_dir)?;
    fs.collect::<()>().await;

    Ok(())
}

async fn maintenance_mock_server(
    handles: LocalComponentHandles,
    clock: Arc<zx::Clock>,
) -> Result<(), anyhow::Error> {
    let mut fs = ServiceFs::new();
    let mut tasks = vec![];

    fs.dir("svc").add_fidl_service(move |stream: MaintenanceRequestStream| {
        let clock_clone = Arc::clone(&clock);

        tasks.push(fasync::Task::local(async move {
            serve_maintenance(clock_clone, stream).await;
        }));
    });

    fs.serve_connection(handles.outgoing_dir)?;
    fs.collect::<()>().await;

    Ok(())
}

fn from_rfc2822(date: &str) -> zx::SyntheticInstant {
    zx::SyntheticInstant::from_nanos(
        chrono::DateTime::parse_from_rfc2822(date).unwrap().timestamp_nanos_opt().unwrap(),
    )
}

lazy_static! {
    pub static ref BACKSTOP_TIME: zx::SyntheticInstant =
        from_rfc2822("Sun, 20 Sep 2020 01:01:01 GMT");
    pub static ref VALID_RTC_TIME: zx::SyntheticInstant =
        from_rfc2822("Sun, 20 Sep 2020 02:02:02 GMT");
    pub static ref BEFORE_BACKSTOP_TIME: zx::SyntheticInstant =
        from_rfc2822("Fri, 06 Mar 2020 04:04:04 GMT");
    pub static ref VALID_TIME: zx::SyntheticInstant = from_rfc2822("Tue, 29 Sep 2020 02:19:01 GMT");
    pub static ref VALID_TIME_2: zx::SyntheticInstant =
        from_rfc2822("Wed, 30 Sep 2020 14:59:59 GMT");
}

/// Time between each reported sample.
pub const BETWEEN_SAMPLES: zx::BootDuration = zx::BootDuration::from_seconds(5);

/// The standard deviation to report on valid time samples.
pub const STD_DEV: zx::BootDuration = zx::BootDuration::from_millis(50);

/// Create a new clock with backstop time set to `BACKSTOP_TIME`.
// TODO: b/306024715 - To be removed once all tests are migrated to TTRF.
pub fn new_clock() -> Arc<zx::SyntheticClock> {
    Arc::new(new_nonshareable_clock())
}

/// Create a new clock with backstop time set to `BACKSTOP_TIME`.
pub fn new_nonshareable_clock() -> zx::SyntheticClock {
    zx::SyntheticClock::create(zx::ClockOpts::empty(), Some(*BACKSTOP_TIME)).unwrap()
}

fn zx_time_to_rtc_time(zx_time: zx::SyntheticInstant) -> fidl_fuchsia_hardware_rtc::Time {
    let date = chrono::Utc.timestamp_nanos(zx_time.into_nanos());
    fidl_fuchsia_hardware_rtc::Time {
        seconds: date.second() as u8,
        minutes: date.minute() as u8,
        hours: date.hour() as u8,
        day: date.day() as u8,
        month: date.month() as u8,
        year: date.year() as u16,
    }
}

pub fn rtc_time_to_zx_time(rtc_time: fidl_fuchsia_hardware_rtc::Time) -> zx::SyntheticInstant {
    let date = chrono::Utc
        .with_ymd_and_hms(
            rtc_time.year as i32,
            rtc_time.month as u32,
            rtc_time.day as u32,
            rtc_time.hours as u32,
            rtc_time.minutes as u32,
            rtc_time.seconds as u32,
        )
        .unwrap();
    zx::SyntheticInstant::from_nanos(date.timestamp_nanos_opt().unwrap())
}

/// Create a stream of MetricEvents from a proxy.
pub fn create_cobalt_event_stream(
    proxy: Arc<MetricEventLoggerQuerierProxy>,
    log_method: LogMethod,
) -> std::pin::Pin<Box<dyn Stream<Item = MetricEvent>>> {
    async_utils::hanging_get::client::HangingGetStream::new(proxy, move |p| {
        p.watch_logs(PROJECT_ID, log_method)
    })
    .map(|res| futures::stream::iter(res.expect("there should be a valid result here").0))
    .flatten()
    .boxed()
}

/// Repeatedly evaluates `condition` until it returns `Some(v)`. Returns `v`.
#[macro_export]
macro_rules! poll_until_some {
    ($condition:expr) => {
        $crate::poll_until_some_impl(
            $condition,
            &$crate::SourceLocation::new(file!(), line!(), column!()),
        )
    };
}

/// Repeatedly evaluates an async `condition` until it returns `Some(v)`. Returns `v`.
/// Use if your condition is an async fn.
#[macro_export]
macro_rules! poll_until_some_async {
    ($condition:expr) => {{
        let loc = $crate::SourceLocation::new(file!(), line!(), column!());
        tracing::info!("=> poll_until_some_async() for {}", &loc);
        let mut result = None;
        loop {
            result = $condition.await;
            if result.is_some() {
                break;
            }
            fasync::Timer::new(fasync::MonotonicInstant::after($crate::RETRY_WAIT_DURATION)).await;
        }
        tracing::info!("=> poll_until_some_async() done for {}", &loc);
        result.expect("we loop around while result is None")
    }};
}

/// Repeatedly evaluates `condition` to create a `Future`, and then awaits the `Future`.
/// Returns `()` when the (most recently created) `Future` resolves to `true`.
#[macro_export]
macro_rules! poll_until_async {
    ($condition:expr) => {
        $crate::poll_until_async_impl(
            $condition,
            &$crate::SourceLocation::new(file!(), line!(), column!()),
        )
    };
}

/// A reimplementation of the above, which deals better with borrows.
#[macro_export]
macro_rules! poll_until_async_2 {
    ($condition:expr) => {{
        let loc = $crate::SourceLocation::new(file!(), line!(), column!());
        tracing::info!("=> poll_until_async() for {}", &loc);
        let mut result = true;
        loop {
            result = $condition.await;
            if result {
                break;
            }
            fasync::Timer::new(fasync::MonotonicInstant::after($crate::RETRY_WAIT_DURATION)).await;
        }
        tracing::info!("=> poll_until_async_2() done for {}", &loc);
        result
    }};
}

/// Repeatedly evaluates `condition` until it returns `true`. Returns `()`.
#[macro_export]
macro_rules! poll_until {
    ($condition:expr) => {
        $crate::poll_until_impl(
            $condition,
            &$crate::SourceLocation::new(file!(), line!(), column!()),
        )
    };
}

/// Wait duration for polling.
pub const RETRY_WAIT_DURATION: zx::MonotonicDuration = zx::MonotonicDuration::from_millis(10);

pub struct SourceLocation {
    file: &'static str,
    line: u32,
    column: u32,
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "(file: {}, line: {}, column: {})", self.file, self.line, self.column)
    }
}

impl SourceLocation {
    pub fn new(file: &'static str, line: u32, column: u32) -> Self {
        Self { file, line, column }
    }
}

/// Use `poll_until_some!()` instead.
pub async fn poll_until_some_impl<T, F>(poll_fn: F, loc: &SourceLocation) -> T
where
    F: Fn() -> Option<T>,
{
    tracing::info!("=> poll_until_some() for {}", loc);
    loop {
        match poll_fn() {
            Some(value) => {
                tracing::info!("<= poll_until_some() for {}", loc);
                return value;
            }
            None => fasync::Timer::new(fasync::MonotonicInstant::after(RETRY_WAIT_DURATION)).await,
        }
    }
}

/// Use `poll_until_async!()` instead.
pub async fn poll_until_async_impl<F, Fut>(poll_fn: F, loc: &SourceLocation)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    tracing::info!("=> poll_until_async() for {}", loc);
    while !poll_fn().await {
        fasync::Timer::new(fasync::MonotonicInstant::after(RETRY_WAIT_DURATION)).await
    }
    tracing::info!("<= poll_until_async() for {}", loc);
}

/// Use `poll_until!()` instead.
pub async fn poll_until_impl<F: Fn() -> bool>(poll_fn: F, loc: &SourceLocation) {
    tracing::info!("=> poll_until() for {}", loc);
    while !poll_fn() {
        fasync::Timer::new(fasync::MonotonicInstant::after(RETRY_WAIT_DURATION)).await
    }
    tracing::info!("<= poll_until() for {}", loc);
}
