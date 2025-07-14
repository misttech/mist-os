// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Alarm management subsystem.
//!
//! This subsystem serves the FIDL API `fuchsia.time.alarms/Wake`. To instantiate,
//! you can use the following approach:
//!
//! ```ignore
//! let proxy = client::connect_to_protocol::<ffhh::DeviceMarker>().map_err(
//!    |e| error!("error: {}", e)).expect("add proper error handling");
//!    let timer_loop = alarms::Handle::new(proxy);
//! ```
//!
//! From here, use the standard approach with [ServiceFs::new] to expose the
//! discoverable FIDL endpoint and call:
//!
//! ```ignore
//! let stream: fidl_fuchsia_time_alarms::WakeRequestStream = ... ;
//! alarms::serve(timer_loop, stream).await;
//! // ...
//! ```
//!
//! Of course, for everything to work well, your component will need appropriate
//! capability routing.  Refer to capability routing docs for those details.

mod emu;

use crate::emu::EmulationTimerOps;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fidl::encoding::ProxyChannelBox;
use fidl::endpoints::RequestStream;
use fidl::HandleBased;
use fuchsia_component::client::connect_to_named_protocol_at_dir_root;
use fuchsia_inspect::{HistogramProperty, Property};
use futures::channel::mpsc;
use futures::sink::SinkExt;
use futures::{StreamExt, TryStreamExt};
use log::{debug, error, warn};
use scopeguard::defer;
use std::cell::RefCell;
use std::cmp;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::rc::Rc;
use std::sync::LazyLock;
use time_pretty::{format_duration, format_timer, MSEC_IN_NANOS};
use zx::AsHandleRef;
use {
    fidl_fuchsia_hardware_hrtimer as ffhh, fidl_fuchsia_time_alarms as fta,
    fuchsia_async as fasync, fuchsia_inspect as finspect, fuchsia_trace as trace,
};

static I64_MAX_AS_U64: LazyLock<u64> = LazyLock::new(|| i64::MAX.try_into().expect("infallible"));
static I32_MAX_AS_U64: LazyLock<u64> = LazyLock::new(|| i32::MAX.try_into().expect("infallible"));

/// The largest value of timer "ticks" that is still considered useful.
static MAX_USEFUL_TICKS: LazyLock<u64> = LazyLock::new(|| *I32_MAX_AS_U64);

/// The hrtimer ID used for scheduling wake alarms.  This ID is reused from
/// Starnix, and should eventually no longer be critical.
const MAIN_TIMER_ID: usize = 6;

/// This is what we consider a "long" delay in alarm operations.
const LONG_DELAY_NANOS: i64 = 2000 * MSEC_IN_NANOS;

/// A macro that waits on a future, but if the future takes longer than
/// 30 seconds to complete, logs a warning message.
macro_rules! log_long_op {
    ($fut:expr) => {{
        use futures::FutureExt;
        let fut = $fut;
        futures::pin_mut!(fut);
        loop {
            let timeout = fasync::Timer::new(std::time::Duration::from_secs(30));
            futures::select! {
                res = fut.as_mut().fuse() => {
                    break res;
                }
                _ = timeout.fuse() => {
                    warn!("unexpected blocking: long-running async operation at {}:{}", file!(), line!());
                    #[cfg(all(target_os = "fuchsia", not(doc)))]
                    ::debug::backtrace_request_all_threads();
                }
            }
        }
    }};
}

/// Compares two optional deadlines and returns true if the `before is different from `after.
/// Nones compare as equal.
fn is_deadline_changed(
    before: Option<fasync::BootInstant>,
    after: Option<fasync::BootInstant>,
) -> bool {
    match (before, after) {
        (None, None) => false,
        (None, Some(_)) | (Some(_), None) => true,
        (Some(before), Some(after)) => before != after,
    }
}

// Errors returnable from [TimerOps] calls.
#[derive(Debug)]
pub(crate) enum TimerOpsError {
    /// The driver reported an error.
    Driver(ffhh::DriverError),
    /// FIDL-specific RPC error.
    Fidl(fidl::Error),
}

trait SawResponseFut: std::future::Future<Output = Result<zx::EventPair, TimerOpsError>> {
    // nop
}

/// Abstracts away timer operations.
#[async_trait(?Send)]
pub(crate) trait TimerOps {
    /// Stop the timer with the specified ID.
    async fn stop(&self, id: u64);

    /// Examine the timer's properties, such as supported resolutions and tick
    /// counts.
    async fn get_timer_properties(&self) -> TimerConfig;

    /// This method must return an actual future, to handle the borrow checker:
    /// making this async will assume that `self` remains borrowed, which will
    /// thwart attempts to move the return value of this call into a separate
    /// closure.
    fn start_and_wait(
        &self,
        id: u64,
        resolution: &ffhh::Resolution,
        ticks: u64,
        setup_event: zx::Event,
    ) -> std::pin::Pin<Box<dyn SawResponseFut>>;
}

/// TimerOps backed by an actual hardware timer.
struct HardwareTimerOps {
    proxy: ffhh::DeviceProxy,
}

impl HardwareTimerOps {
    fn new(proxy: ffhh::DeviceProxy) -> Box<Self> {
        Box::new(Self { proxy })
    }
}

#[async_trait(?Send)]
impl TimerOps for HardwareTimerOps {
    async fn stop(&self, id: u64) {
        let _ = self
            .proxy
            .stop(id)
            .await
            .map(|result| {
                let _ = result.map_err(|e| warn!("stop_hrtimer: driver error: {:?}", e));
            })
            .map_err(|e| warn!("stop_hrtimer: could not stop prior timer: {}", e));
    }

    async fn get_timer_properties(&self) -> TimerConfig {
        match log_long_op!(self.proxy.get_properties()) {
            Ok(p) => {
                let timers_properties = &p.timers_properties.expect("timers_properties must exist");
                debug!("get_timer_properties: got: {:?}", timers_properties);

                // Pick the correct hrtimer to use for wakes.
                let timer_index = if timers_properties.len() > MAIN_TIMER_ID {
                    // Mostly vim3, where we have pre-existing timer allocations
                    // that we don't need to change.
                    MAIN_TIMER_ID
                } else if timers_properties.len() > 0 {
                    // Newer devices that don't need to allocate timer IDs, and/or
                    // may not even have as many timers as vim3 does. But, at least
                    // one timer is needed.
                    0
                } else {
                    // Give up.
                    return TimerConfig::new_empty();
                };
                let main_timer_properties = &timers_properties[timer_index];
                debug!("alarms: main_timer_properties: {:?}", main_timer_properties);
                // Not sure whether it is useful to have more ticks than this, so limit it.
                let max_ticks: u64 = std::cmp::min(
                    main_timer_properties.max_ticks.unwrap_or(*MAX_USEFUL_TICKS),
                    *MAX_USEFUL_TICKS,
                );
                let resolutions = &main_timer_properties
                    .supported_resolutions
                    .as_ref()
                    .expect("supported_resolutions is populated")
                    .iter()
                    .last() //  Limits the resolution to the coarsest available.
                    .map(|r| match *r {
                        ffhh::Resolution::Duration(d) => d,
                        _ => {
                            error!(
                            "get_timer_properties: Unknown resolution type, returning millisecond."
                        );
                            MSEC_IN_NANOS
                        }
                    })
                    .map(|d| zx::BootDuration::from_nanos(d))
                    .into_iter() // Used with .last() above.
                    .collect::<Vec<_>>();
                let timer_id = main_timer_properties.id.expect("timer ID is always present");
                TimerConfig::new_from_data(timer_id, resolutions, max_ticks)
            }
            Err(e) => {
                error!("could not get timer properties: {:?}", e);
                TimerConfig::new_empty()
            }
        }
    }

    fn start_and_wait(
        &self,
        id: u64,
        resolution: &ffhh::Resolution,
        ticks: u64,
        setup_event: zx::Event,
    ) -> std::pin::Pin<Box<dyn SawResponseFut>> {
        let inner = self.proxy.start_and_wait(id, resolution, ticks, setup_event);
        Box::pin(HwResponseFut { pinner: Box::pin(inner) })
    }
}

// Untangles the borrow checker issues that otherwise result from making
// TimerOps::start_and_wait an async function.
struct HwResponseFut {
    pinner: std::pin::Pin<
        Box<
            fidl::client::QueryResponseFut<
                ffhh::DeviceStartAndWaitResult,
                fidl::encoding::DefaultFuchsiaResourceDialect,
            >,
        >,
    >,
}

use std::task::Poll;
impl SawResponseFut for HwResponseFut {}
impl std::future::Future for HwResponseFut {
    type Output = Result<zx::EventPair, TimerOpsError>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let inner_poll = self.pinner.as_mut().poll(cx);
        match inner_poll {
            Poll::Ready(result) => Poll::Ready(match result {
                Ok(Ok(keep_alive)) => Ok(keep_alive),
                Ok(Err(e)) => Err(TimerOpsError::Driver(e)),
                Err(e) => Err(TimerOpsError::Fidl(e)),
            }),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Stops a currently running hardware timer.
async fn stop_hrtimer(hrtimer: &Box<dyn TimerOps>, timer_config: &TimerConfig) {
    trace::duration!(c"alarms", c"hrtimer:stop", "id" => timer_config.id);
    debug!("stop_hrtimer: stopping hardware timer: {}", timer_config.id);
    hrtimer.stop(timer_config.id).await;
    debug!("stop_hrtimer: stopped  hardware timer: {}", timer_config.id);
}

// The default size of the channels created in this module.
const CHANNEL_SIZE: usize = 100;

/// A type handed around between the concurrent loops run by this module.
#[derive(Debug)]
enum Cmd {
    /// Request a timer to be started.
    Start {
        /// The unique connection ID.
        cid: zx::Koid,
        /// A timestamp (presumably in the future), at which to expire the timer.
        deadline: fasync::BootInstant,
        // The API supports several modes. See fuchsia.time.alarms/Wake.fidl.
        mode: fta::SetMode,
        /// An alarm identifier, chosen by the caller.
        alarm_id: String,
        /// A responder that will be called when the timer expires. The
        /// client end of the connection will block until we send something
        /// on this responder.
        ///
        /// This is packaged into a Rc... only because both the "happy path"
        /// and the error path must consume the responder.  This allows them
        /// to be consumed, without the responder needing to implement Default.
        responder: Rc<RefCell<Option<fta::WakeAlarmsSetAndWaitResponder>>>,
    },
    StopById {
        done: zx::Event,
        timer_id: TimerId,
    },
    Alarm {
        expired_deadline: fasync::BootInstant,
        keep_alive: fidl::EventPair,
    },
    AlarmFidlError {
        expired_deadline: fasync::BootInstant,
        error: fidl::Error,
    },
    AlarmDriverError {
        expired_deadline: fasync::BootInstant,
        error: ffhh::DriverError,
    },
}

impl std::fmt::Display for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::Start { cid, deadline, alarm_id, .. } => {
                write!(
                    f,
                    "Start[alarm_id=\"{}\", cid={:?}, deadline={}]",
                    alarm_id,
                    cid,
                    format_timer((*deadline).into())
                )
            }
            Cmd::Alarm { expired_deadline, .. } => {
                write!(f, "Alarm[deadline={}]", format_timer((*expired_deadline).into()))
            }
            Cmd::AlarmFidlError { expired_deadline, error } => {
                write!(
                    f,
                    "FIDLError[deadline={}, err={}, NO_WAKE_LEASE!]",
                    format_timer((*expired_deadline).into()),
                    error
                )
            }
            Cmd::AlarmDriverError { expired_deadline, error } => {
                write!(
                    f,
                    "DriverError[deadline={}, err={:?}, NO_WAKE_LEASE!]",
                    format_timer((*expired_deadline).into()),
                    error
                )
            }
            Cmd::StopById { timer_id, done: _ } => {
                write!(f, "StopById[timerId={}]", timer_id,)
            }
        }
    }
}

/// Extracts a KOID from the underlying channel of the provided [stream].
///
/// # Returns
/// - zx::Koid: the KOID you wanted.
/// - fta::WakeAlarmsRequestStream: the stream; we had to deconstruct it briefly,
///   so this gives it back to you.
pub fn get_stream_koid(
    stream: fta::WakeAlarmsRequestStream,
) -> (zx::Koid, fta::WakeAlarmsRequestStream) {
    let (inner, is_terminated) = stream.into_inner();
    let koid = inner.channel().as_channel().get_koid().expect("infallible");
    let stream = fta::WakeAlarmsRequestStream::from_inner(inner, is_terminated);
    (koid, stream)
}

/// Serves a single Wake API client.
pub async fn serve(timer_loop: Rc<Loop>, requests: fta::WakeAlarmsRequestStream) {
    let timer_loop = timer_loop.clone();
    let timer_loop_send = || timer_loop.get_sender();
    let (cid, mut requests) = get_stream_koid(requests);
    let mut request_count = 0;
    debug!("alarms::serve: opened connection: {:?}", cid);
    while let Some(maybe_request) = requests.next().await {
        request_count += 1;
        debug!("alarms::serve: cid: {:?} incoming request: {}", cid, request_count);
        match maybe_request {
            Ok(request) => {
                // Should return quickly.
                handle_request(cid, timer_loop_send(), request).await;
            }
            Err(e) => {
                warn!("alarms::serve: error in request: {:?}", e);
            }
        }
        debug!("alarms::serve: cid: {:?} done request: {}", cid, request_count);
    }
    // Check if connection closure was intentional. It is way too easy to close
    // a FIDL connection inadvertently if doing non-mainstream things with FIDL.
    warn!("alarms::serve: CLOSED CONNECTION: cid: {:?}", cid);
}

async fn handle_cancel(alarm_id: String, cid: zx::Koid, cmd: &mut mpsc::Sender<Cmd>) {
    let done = zx::Event::create();
    let timer_id = TimerId { alarm_id: alarm_id.clone(), cid };
    if let Err(e) = cmd.send(Cmd::StopById { timer_id, done: clone_handle(&done) }).await {
        warn!("handle_request: error while trying to cancel: {}: {:?}", alarm_id, e);
    }
    wait_signaled(&done).await;
}

/// Processes a single Wake API request from a single client.
/// This function is expected to return quickly.
///
/// # Args
/// - `cid`: the unique identifier of the connection producing these requests.
/// - `cmd`: the outbound queue of commands to deliver to the timer manager.
/// - `request`: a single inbound Wake FIDL API request.
async fn handle_request(
    cid: zx::Koid,
    mut cmd: mpsc::Sender<Cmd>,
    request: fta::WakeAlarmsRequest,
) {
    match request {
        fta::WakeAlarmsRequest::SetAndWait { deadline, mode, alarm_id, responder } => {
            // Since responder is consumed by the happy path and the error path, but not both,
            // and because the responder does not implement Default, this is a way to
            // send it in two mutually exclusive directions.  Each direction will reverse
            // this wrapping once the responder makes it to the other side.
            //
            // Rc required because of sharing a noncopyable struct; RefCell required because
            // borrow_mut() is needed to move out; and Option is required so we can
            // use take() to replace the struct with None so it does not need to leave
            // a Default in its place.
            let responder = Rc::new(RefCell::new(Some(responder)));

            // Alarm is not scheduled yet!
            debug!(
                "handle_request: scheduling alarm_id: \"{}\"\n\tcid: {:?}\n\tdeadline: {}",
                alarm_id,
                cid,
                format_timer(deadline.into())
            );
            // Expected to return quickly.
            if let Err(e) = log_long_op!(cmd.send(Cmd::Start {
                cid,
                deadline: deadline.into(),
                mode,
                alarm_id: alarm_id.clone(),
                responder: responder.clone(),
            })) {
                warn!("handle_request: error while trying to schedule `{}`: {:?}", alarm_id, e);
                responder
                    .borrow_mut()
                    .take()
                    .expect("always present if call fails")
                    .send(Err(fta::WakeAlarmsError::Internal))
                    .unwrap();
            }
        }
        fta::WakeAlarmsRequest::Cancel { alarm_id, .. } => {
            // TODO: b/383062441 - make this into an async task so that we wait
            // less to schedule the next alarm.
            log_long_op!(handle_cancel(alarm_id, cid, &mut cmd));
        }
        fta::WakeAlarmsRequest::Set { .. } => {
            // TODO(https://fxbug.dev/424009669): Finish implementation
        }
        fta::WakeAlarmsRequest::_UnknownMethod { .. } => {}
    };
}

/// Represents a single alarm event processing loop.
///
/// One instance is created per each alarm-capable low-level device.
pub struct Loop {
    // Given to any clients that need to send messages to `_task`
    // via [get_sender].
    snd: mpsc::Sender<Cmd>,
}

impl Loop {
    /// Creates a new instance of [Loop].
    ///
    /// `device_proxy` is a connection to a low-level timer device.
    pub fn new(
        scope: fasync::ScopeHandle,
        device_proxy: ffhh::DeviceProxy,
        inspect: finspect::Node,
    ) -> Self {
        let hw_device_timer_ops = HardwareTimerOps::new(device_proxy);
        Loop::new_internal(scope, hw_device_timer_ops, inspect)
    }

    // Creates a new instance of [Loop] with emulated wake alarms.
    pub fn new_emulated(scope: fasync::ScopeHandle, inspect: finspect::Node) -> Self {
        let timer_ops = Box::new(EmulationTimerOps::new());
        Loop::new_internal(scope, timer_ops, inspect)
    }

    fn new_internal(
        scope: fasync::ScopeHandle,
        timer_ops: Box<dyn TimerOps>,
        inspect: finspect::Node,
    ) -> Self {
        let (snd, rcv) = mpsc::channel(CHANNEL_SIZE);
        let snd_clone = snd.clone();
        let loop_scope = scope.clone();
        scope.spawn_local(wake_timer_loop(loop_scope, snd_clone, rcv, timer_ops, inspect));
        Self { snd }
    }

    /// Gets a copy of a channel through which async commands may be sent to
    /// the [Loop].
    fn get_sender(&self) -> mpsc::Sender<Cmd> {
        self.snd.clone()
    }
}

/// A representation of the state of a single Timer.
#[derive(Debug)]
struct TimerNode {
    /// The deadline at which the timer expires.
    deadline: fasync::BootInstant,
    /// The unique alarm ID associated with this timer.
    alarm_id: String,
    /// The unique connection ID that this timer belongs to.  Multiple timers
    /// may share the same `cid`.
    cid: zx::Koid,
    /// The responder that is blocked until the timer expires.  Used to notify
    /// the alarms subsystem client when this alarm expires.
    responder: Option<fta::WakeAlarmsSetAndWaitResponder>,
}

impl TimerNode {
    fn new(
        deadline: fasync::BootInstant,
        alarm_id: String,
        cid: zx::Koid,
        responder: fta::WakeAlarmsSetAndWaitResponder,
    ) -> Self {
        Self { deadline, alarm_id, cid, responder: Some(responder) }
    }

    fn get_alarm_id(&self) -> &str {
        &self.alarm_id[..]
    }

    fn get_cid(&self) -> &zx::Koid {
        &self.cid
    }

    fn get_id(&self) -> TimerId {
        TimerId { alarm_id: self.alarm_id.clone(), cid: self.cid.clone() }
    }

    fn get_deadline(&self) -> &fasync::BootInstant {
        &self.deadline
    }

    fn take_responder(&mut self) -> Option<fta::WakeAlarmsSetAndWaitResponder> {
        self.responder.take()
    }
}

impl Drop for TimerNode {
    // If the TimerNode was evicted without having expired, notify the other
    // end that the timer has been canceled.
    fn drop(&mut self) {
        let responder = self.take_responder();
        responder.map(|r| {
            // If the TimerNode is dropped, notify the client that may have
            // been waiting. We can not drop a responder, because that kills
            // the FIDL connection.
            r.send(Err(fta::WakeAlarmsError::Dropped))
                .map_err(|e| error!("could not drop responder: {:?}", e))
        });
    }
}

/// This and other comparison trait implementation are needed to establish
/// a total ordering of TimerNodes.
impl std::cmp::Eq for TimerNode {}

impl std::cmp::PartialEq for TimerNode {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.alarm_id == other.alarm_id && self.cid == other.cid
    }
}

impl std::cmp::PartialOrd for TimerNode {
    /// Order by deadline first, but timers with same deadline are ordered
    /// by respective IDs to avoid ordering nondeterminism.
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerNode {
    /// Compares two [TimerNode]s, by "which is sooner".
    ///
    /// Ties are broken by alarm ID, then by connection ID.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let ordering = other.deadline.cmp(&self.deadline);
        if ordering == std::cmp::Ordering::Equal {
            let ordering = self.alarm_id.cmp(&self.alarm_id);
            if ordering == std::cmp::Ordering::Equal {
                self.cid.cmp(&other.cid)
            } else {
                ordering
            }
        } else {
            ordering
        }
    }
}

/// A full timer identifier.
#[derive(Debug, PartialEq, Eq, Hash)]
struct TimerId {
    /// Connection-unique alarm ID.
    alarm_id: String,
    /// Connection identifier, unique per each client connection.
    cid: zx::Koid,
}

// Compute a trace ID for a given alarm ID. This identifier is used across
// processes for tracking the alarm's lifetime.
fn as_trace_id(alarm_id: &str) -> trace::Id {
    if let Some(rest) = alarm_id.strip_prefix("starnix:Koid(") {
        if let Some((koid_str, _)) = rest.split_once(')') {
            if let Ok(trace_id) = koid_str.parse::<u64>() {
                return trace_id.into();
            }
        }
    }

    // For now, other components don't have a specific way to get the trace id.
    0.into()
}

impl std::fmt::Display for TimerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TimerId[alarm_id:{},cid:{:?}]", self.alarm_id, self.cid)
    }
}

/// Contains all the timers known by the alarms subsystem.
///
/// [Timers] can efficiently find a timer with the earliest deadline,
/// and given a cutoff can expire one timer for which the deadline has
/// passed.
struct Timers {
    timers: BinaryHeap<TimerNode>,
    deadline_by_id: HashMap<TimerId, fasync::BootInstant>,
}

impl std::fmt::Display for Timers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let now = fasync::BootInstant::now();
        let sorted = self
            .timers
            .iter()
            .map(|n| (n.deadline, n.alarm_id.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(k, v)| {
                let remaining = k - now;
                format!(
                    "Timeout: {} => timer_id: {}, remaining: {}",
                    format_timer(k.into()),
                    v,
                    format_duration(remaining.into())
                )
            })
            .collect::<Vec<_>>();
        let joined = sorted.join("\n\t");
        write!(f, "\n\t{}", joined)
    }
}

impl Timers {
    /// Creates an empty [AllTimers].
    fn new() -> Self {
        Self { timers: BinaryHeap::new(), deadline_by_id: HashMap::new() }
    }

    /// Adds a [TimerNode] to [Timers].
    ///
    /// If the inserted node is identical to an already existing node, then
    /// nothing is changed.  If the deadline is different, then the timer node
    /// is replaced.
    fn push(&mut self, n: TimerNode) {
        let new_id = n.get_id();
        if let Some(deadline) = self.deadline_by_id.get(&new_id) {
            // There already is a deadline for this timer.
            if n.deadline == *deadline {
                return;
            }
            // Else replace. The deadline may be pushed out or pulled in.
            self.deadline_by_id.insert(new_id, n.deadline.clone());
            self.timers.retain(|t| t.get_id() != n.get_id());
            self.timers.push(n);
        } else {
            // New timer node.
            self.deadline_by_id.insert(new_id, n.deadline);
            self.timers.push(n);
        }
    }

    /// Returns a reference to the stored timer with the earliest deadline.
    fn peek(&self) -> Option<&TimerNode> {
        self.timers.peek()
    }

    /// Returns the deadline of the proximate timer in [Timers].
    fn peek_deadline(&self) -> Option<fasync::BootInstant> {
        self.peek().map(|t| t.deadline)
    }

    fn peek_id(&self) -> Option<TimerId> {
        self.peek().map(|t| TimerId { alarm_id: t.alarm_id.clone(), cid: t.cid })
    }

    /// Args:
    /// - `now` is the current time.
    /// - `deadline` is the timer deadline to check for expiry.
    fn expired(now: fasync::BootInstant, deadline: fasync::BootInstant) -> bool {
        deadline <= now
    }

    /// Returns true if there are no known timers.
    fn is_empty(&self) -> bool {
        let empty1 = self.timers.is_empty();
        let empty2 = self.deadline_by_id.is_empty();
        assert!(empty1 == empty2, "broken invariant: empty1: {} empty2:{}", empty1, empty2);
        empty1
    }

    /// Attempts to expire the earliest timer.
    ///
    /// If a timer is expired, it is removed from [Timers] and returned to the caller. Note that
    /// there may be more timers that need expiring at the provided `reference instant`. To drain
    /// [Timers] of all expired timers, one must repeat the call to this method with the same
    /// value of `reference_instant` until it returns `None`.
    ///
    /// Args:
    /// - `now`: the time instant to compare the stored timers against.  Timers for
    ///   which the deadline has been reached or surpassed are eligible for expiry.
    fn maybe_expire_earliest(&mut self, now: fasync::BootInstant) -> Option<TimerNode> {
        self.peek_deadline()
            .map(|d| {
                if Timers::expired(now, d) {
                    self.timers.pop().map(|e| {
                        self.deadline_by_id.remove(&e.get_id());
                        e
                    })
                } else {
                    None
                }
            })
            .flatten()
    }

    /// Removes an alarm by ID.  If the earliest alarm is the alarm to be removed,
    /// it is returned.
    fn remove_by_id(&mut self, timer_id: &TimerId) -> Option<TimerNode> {
        let ret = if let Some(t) = self.peek_id() {
            if t == *timer_id {
                self.timers.pop()
            } else {
                None
            }
        } else {
            None
        };

        self.timers.retain(|t| t.alarm_id != timer_id.alarm_id || t.cid != timer_id.cid);
        self.deadline_by_id.remove(timer_id);
        ret
    }

    /// Returns the number of currently pending timers.
    fn timer_count(&self) -> usize {
        let count1 = self.timers.len();
        let count2 = self.deadline_by_id.len();
        assert!(count1 == count2, "broken invariant: count1: {}, count2: {}", count1, count2);
        count1
    }
}

// Clones a handle. Needed for 1:N notifications.
pub(crate) fn clone_handle<H: HandleBased>(handle: &H) -> H {
    handle.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("infallible")
}

async fn wait_signaled<H: HandleBased>(handle: &H) {
    fasync::OnSignals::new(handle, zx::Signals::EVENT_SIGNALED).await.expect("infallible");
}

pub(crate) fn signal<H: HandleBased>(handle: &H) {
    handle.signal_handle(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED).expect("infallible");
}

/// A [TimerDuration] represents a duration of time that can be expressed by
/// a discrete timer register.
///
/// This is a low-level representation of time duration, used in interaction with
/// hardware devices. It is therefore necessarily discretized, with adaptive
/// resolution, depending on the physical characteristics of the underlying
/// hardware timer that it models.
#[derive(Debug, Clone, Copy)]
struct TimerDuration {
    // The resolution of each one of the `ticks` below.
    resolution: zx::BootDuration,
    // The number of ticks that encodes time duration. Each "tick" represents
    // one unit of `resolution` above.
    ticks: u64,
}

/// This and the comparison traits below are used to allow TimerDuration
/// calculations in a compact form.
impl Eq for TimerDuration {}

impl std::cmp::PartialOrd for TimerDuration {
    fn partial_cmp(&self, other: &TimerDuration) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::PartialEq for TimerDuration {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl std::cmp::Ord for TimerDuration {
    /// Two [TimerDuration]s compare equal if they model exactly the same duration of time,
    /// no matter the resolutions.
    fn cmp(&self, other: &TimerDuration) -> std::cmp::Ordering {
        let self_ticks_128: i128 = self.ticks as i128;
        let self_resolution: i128 = self.resolution_as_nanos() as i128;
        let self_nanos = self_resolution * self_ticks_128;

        let other_ticks_128: i128 = other.ticks as i128;
        let other_resolution: i128 = other.resolution_as_nanos() as i128;
        let other_nanos = other_resolution * other_ticks_128;

        self_nanos.cmp(&other_nanos)
    }
}

impl std::fmt::Display for TimerDuration {
    /// Human readable TimerDuration exposes both the tick count and the resolution,
    /// in the format of "ticks x resolution", with an end result of
    /// `10x5ms` for example.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ticks = self.ticks;
        let resolution = self.resolution();
        // Example: 10x1ms
        write!(f, "{}x{}", ticks, format_duration(resolution),)
    }
}

impl TimerDuration {
    /// The maximum representable TimerDuration that we allow.
    fn max() -> Self {
        TimerDuration::new(zx::BootDuration::from_nanos(1), *I64_MAX_AS_U64)
    }

    /// The zero [TimerDuration].
    fn zero() -> Self {
        TimerDuration::new(zx::BootDuration::from_nanos(1), 0)
    }

    /// Creates a new timer duration with the given parameters.
    fn new(resolution: zx::BootDuration, ticks: u64) -> Self {
        Self { resolution, ticks }
    }

    /// Creates a new timer duration using the resolution from `res_source` and
    /// a specified number of ticks.
    fn new_with_resolution(res_source: &TimerDuration, ticks: u64) -> Self {
        Self::new(res_source.resolution, ticks)
    }

    /// Returns the time duration represented by this TimerDuration.
    ///
    /// Due to the way duration is expressed, the same time duration
    /// can be represented in multiple ways.
    fn duration(&self) -> zx::BootDuration {
        let duration_as_nanos = self.resolution_as_nanos() * self.ticks;
        let clamp_duration = std::cmp::min(*I32_MAX_AS_U64, duration_as_nanos);
        zx::BootDuration::from_nanos(clamp_duration.try_into().expect("result was clamped"))
    }

    /// The resolution of this TimerDuration
    fn resolution(&self) -> zx::BootDuration {
        self.resolution
    }

    fn resolution_as_nanos(&self) -> u64 {
        self.resolution().into_nanos().try_into().expect("resolution is never negative")
    }

    /// The number of ticks of this [TimerDuration].
    fn ticks(&self) -> u64 {
        self.ticks
    }
}

impl From<zx::BootDuration> for TimerDuration {
    fn from(d: zx::BootDuration) -> TimerDuration {
        let nanos = d.into_nanos();
        assert!(nanos >= 0);
        let nanos_u64 = nanos.try_into().expect("guarded by assert");
        TimerDuration::new(zx::BootDuration::from_nanos(1), nanos_u64)
    }
}

impl std::ops::Div for TimerDuration {
    type Output = u64;
    fn div(self, rhs: Self) -> Self::Output {
        let self_nanos = self.resolution_as_nanos() * self.ticks;
        let rhs_nanos = rhs.resolution_as_nanos() * rhs.ticks;
        self_nanos / rhs_nanos
    }
}

impl std::ops::Mul<u64> for TimerDuration {
    type Output = Self;
    fn mul(self, rhs: u64) -> Self::Output {
        Self::new(self.resolution, self.ticks * rhs)
    }
}

/// Contains the configuration of a specific timer.
#[derive(Debug)]
pub(crate) struct TimerConfig {
    /// The resolutions supported by this timer. Each entry is one possible
    /// duration for on timer "tick".  The resolution is picked when a timer
    /// request is sent.
    resolutions: Vec<zx::BootDuration>,
    /// The maximum count of "ticks" that the timer supports. The timer usually
    /// has a register that counts up or down based on a clock signal with
    /// the period specified by `resolutions`.  This is the maximum value that
    /// the counter can count to without overflowing.
    max_ticks: u64,
    /// The stable ID of the timer with the above configuration.
    id: u64,
}

impl TimerConfig {
    /// Creates a new timer config with supported timer resolutions and the max
    /// ticks value for the timer's counter.
    fn new_from_data(timer_id: u64, resolutions: &[zx::BootDuration], max_ticks: u64) -> Self {
        debug!(
            "TimerConfig: resolutions: {:?}, max_ticks: {}, timer_id: {}",
            resolutions.iter().map(|r| format_duration(*r)).collect::<Vec<_>>(),
            max_ticks,
            timer_id
        );
        let resolutions = resolutions.iter().map(|d| *d).collect::<Vec<zx::BootDuration>>();
        TimerConfig { resolutions, max_ticks, id: timer_id }
    }

    fn new_empty() -> Self {
        error!("TimerConfig::new_empty() called, this is not OK.");
        TimerConfig { resolutions: vec![], max_ticks: 0, id: 0 }
    }

    // Picks the most appropriate timer setting for it to fire as close as possible
    // when `duration` expires.
    //
    // If duration is too far in the future for what the timer supports,
    // return a smaller value, to allow the timer to be reprogrammed multiple
    // times.
    //
    // If the available menu of resolutions is such that we can wake only after
    // the intended deadline, begrudgingly return that option.
    fn pick_setting(&self, duration: zx::BootDuration) -> TimerDuration {
        //  0         |-------------->|<---------------|
        //  |---------+---------------+----------------+---->
        //  |---------^               |                |
        //  | best positive slack     |                |
        //  |-------------------------^ duration       |
        //  |------------------------------------------^ best negative slack.
        let mut best_positive_slack = TimerDuration::zero();
        let mut best_negative_slack = TimerDuration::max();

        if self.max_ticks == 0 {
            return TimerDuration::new(zx::BootDuration::from_millis(1), 0);
        }
        let duration_slack: TimerDuration = duration.into();

        for res1 in self.resolutions.iter() {
            let smallest_unit = TimerDuration::new(*res1, 1);
            let max_tick_at_res = TimerDuration::new(*res1, self.max_ticks);

            let smallest_slack_larger_than_duration = smallest_unit > duration_slack;
            let largest_slack_smaller_than_duration = max_tick_at_res < duration_slack;

            if smallest_slack_larger_than_duration {
                if duration_slack == TimerDuration::zero() {
                    best_negative_slack = TimerDuration::zero();
                } else if smallest_unit < best_negative_slack {
                    best_negative_slack = smallest_unit;
                }
            }
            if largest_slack_smaller_than_duration {
                if max_tick_at_res > best_positive_slack
                    || best_positive_slack == TimerDuration::zero()
                {
                    best_positive_slack = max_tick_at_res;
                }
            }

            // "Regular" case.
            if !smallest_slack_larger_than_duration && !largest_slack_smaller_than_duration {
                // Check whether duration divides evenly into the available slack options
                // for this resolution.  If it does, then that is the slack we're looking for.
                let q = duration_slack / smallest_unit;
                let d = smallest_unit * q;
                if d == duration_slack {
                    // Exact match, we can return right now.
                    return d;
                } else {
                    // Not an exact match, so q ticks is before, but q+1 is after.
                    if d > best_positive_slack {
                        best_positive_slack = TimerDuration::new_with_resolution(&smallest_unit, q);
                    }
                    let d_plus = TimerDuration::new_with_resolution(&smallest_unit, q + 1);
                    if d_plus < best_negative_slack {
                        best_negative_slack = d_plus;
                    }
                }
            }
        }

        let p_slack = duration - best_positive_slack.duration();
        let n_slack = best_negative_slack.duration() - duration;

        // If the closest approximation is 0ns, then we can not advance time, so we reject it.
        // Otherwise pick the smallest slack.  Note that when we pick the best positive slack,
        // we will wake *before* the actual deadline.  In multi-resolution counters, this enables
        // us to pick a finer count in the next go.
        let ret = if p_slack < n_slack && best_positive_slack.duration().into_nanos() > 0 {
            best_positive_slack
        } else {
            best_negative_slack
        };
        debug!("TimerConfig: picked slack: {} for duration: {}", ret, format_duration(duration));
        assert!(
            ret.duration().into_nanos() >= 0,
            "ret: {}, p_slack: {}, n_slack: {}, orig.duration: {}\n\tbest_p_slack: {}\n\tbest_n_slack: {}\n\ttarget: {}\n\t 1: {} 2: {:?}, 3: {:?}",
            ret,
            format_duration(p_slack),
            format_duration(n_slack),
            format_duration(duration),
            best_positive_slack,
            best_negative_slack,
            duration_slack,
            p_slack != zx::BootDuration::ZERO,
            p_slack,
            zx::BootDuration::ZERO,
        );
        ret
    }
}

async fn get_timer_properties(hrtimer: &Box<dyn TimerOps>) -> TimerConfig {
    debug!("get_timer_properties: requesting timer properties.");
    hrtimer.get_timer_properties().await
}

/// The state of a single hardware timer that we must bookkeep.
struct TimerState {
    // The task waiting for the proximate timer to expire.
    task: fasync::Task<()>,
    // The deadline that the above task is waiting for.
    deadline: fasync::BootInstant,
}

/// The command loop for timer interaction.  All changes to the wake alarm device programming
/// come in form of commands through `cmd`.
///
/// Args:
/// - `snd`: the send end of `cmd` below, a clone is given to each spawned sub-task.
/// - `cmds``: the input queue of alarm related commands.
/// - `timer_proxy`: the FIDL API proxy for interacting with the hardware device.
/// - `inspect`: the inspect node to record loop info into.
async fn wake_timer_loop(
    scope: fasync::ScopeHandle,
    snd: mpsc::Sender<Cmd>,
    mut cmds: mpsc::Receiver<Cmd>,
    timer_proxy: Box<dyn TimerOps>,
    inspect: finspect::Node,
) {
    debug!("wake_timer_loop: started");

    let mut timers = Timers::new();
    let timer_config = get_timer_properties(&timer_proxy).await;

    // Keeps the currently executing HrTimer closure.  This is not read from, but
    // keeps the timer task active.
    #[allow(clippy::collection_is_never_read)]
    let mut hrtimer_status: Option<TimerState> = None;

    // Initialize inspect properties. This must be done only once.
    //
    // Take note that these properties are updated when the `cmds` loop runs.
    // This means that repeated reads while no `cmds` activity occurs will return
    // old readings.  This is to ensure a consistent ability to replay the last
    // loop run if needed.
    let now_prop = inspect.create_int("now_ns", 0);
    let now_formatted_prop = inspect.create_string("now_formatted", "");
    let pending_timers_count_prop = inspect.create_uint("pending_timers_count", 0);
    let pending_timers_prop = inspect.create_string("pending_timers", "");
    let deadline_histogram_prop = inspect.create_int_exponential_histogram(
        "requested_deadlines_ns",
        finspect::ExponentialHistogramParams {
            floor: 0,
            initial_step: zx::BootDuration::from_micros(1).into_nanos(),
            // Allows capturing deadlines up to dozens of days.
            step_multiplier: 10,
            buckets: 16,
        },
    );
    let slack_histogram_prop = inspect.create_int_exponential_histogram(
        "slack_ns",
        finspect::ExponentialHistogramParams {
            floor: 0,
            initial_step: zx::BootDuration::from_micros(1).into_nanos(),
            step_multiplier: 10,
            buckets: 16,
        },
    );
    let schedule_delay_prop = inspect.create_int_exponential_histogram(
        "schedule_delay_ns",
        finspect::ExponentialHistogramParams {
            floor: 0,
            initial_step: zx::BootDuration::from_micros(1).into_nanos(),
            step_multiplier: 10,
            buckets: 16,
        },
    );
    // Internals of what was programmed into the wake alarms hardware.
    let hw_node = inspect.create_child("hardware");
    let current_hw_deadline_prop = hw_node.create_string("current_deadline", "");
    let remaining_until_alarm_prop = hw_node.create_string("remaining_until_alarm", "");

    while let Some(cmd) = cmds.next().await {
        trace::duration!(c"alarms", c"Cmd");
        // Use a consistent notion of "now" across commands.
        let now = fasync::BootInstant::now();
        now_prop.set(now.into_nanos());
        trace::instant!(c"alarms", c"wake_timer_loop", trace::Scope::Process, "now" => now.into_nanos());
        match cmd {
            Cmd::Start { cid, deadline, mode, alarm_id, responder } => {
                trace::duration!(c"alarms", c"Cmd::Start");
                fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", as_trace_id(&alarm_id));
                let responder = responder.borrow_mut().take().expect("responder is always present");
                // NOTE: hold keep_alive until all work is done.
                debug!(
                    "wake_timer_loop: START alarm_id: \"{}\", cid: {:?}\n\tdeadline: {}\n\tnow:      {}",
                    alarm_id,
                    cid,
                    format_timer(deadline.into()),
                    format_timer(now.into()),
                );

                defer! {
                    // This is the only option that requires further action.
                    if let fta::SetMode::NotifySetupDone(setup_done) = mode {
                        // Must signal once the setup is completed.
                        signal(&setup_done);
                        debug!("wake_timer_loop: START: setup_done signaled");
                    };
                }
                deadline_histogram_prop.insert((deadline - now).into_nanos());
                if Timers::expired(now, deadline) {
                    trace::duration!(c"alarms", c"Cmd::Start:immediate");
                    fuchsia_trace::flow_step!(
                        c"alarms",
                        c"hrtimer_lifecycle",
                        as_trace_id(&alarm_id)
                    );
                    // A timer set into now or the past expires right away.
                    let (_lease, keep_alive) = zx::EventPair::create();
                    debug!(
                        "[{}] wake_timer_loop: bogus lease {:?}",
                        line!(),
                        &keep_alive.get_koid().unwrap()
                    );
                    responder
                        .send(Ok(keep_alive))
                        .map(|_| {
                            debug!(
                                concat!(
                                    "wake_timer_loop: cid: {:?}, alarm: {}: EXPIRED IMMEDIATELY\n\t",
                                    "deadline({}) <= now({})"
                                ),
                                cid,
                                alarm_id,
                                format_timer(deadline.into()),
                                format_timer(now.into())
                            )
                        })
                        .map_err(|e| {
                            error!(
                            "wake_timer_loop: cid: {:?}, alarm: {}: could not notify, dropping: {}",
                                cid, alarm_id, e)
                        })
                        .unwrap_or(());
                } else {
                    trace::duration!(c"alarms", c"Cmd::Start:regular");
                    fuchsia_trace::flow_step!(
                        c"alarms",
                        c"hrtimer_lifecycle",
                        as_trace_id(&alarm_id)
                    );
                    // A timer scheduled for the future gets inserted into the timer heap.
                    let was_empty = timers.is_empty();

                    let deadline_before = timers.peek_deadline();
                    timers.push(TimerNode::new(deadline, alarm_id, cid, responder));
                    let deadline_after = timers.peek_deadline();

                    let deadline_changed = is_deadline_changed(deadline_before, deadline_after);
                    let needs_cancel = !was_empty && deadline_changed;
                    let needs_reschedule = was_empty || deadline_changed;

                    if needs_reschedule {
                        // Always schedule the proximate deadline.
                        let schedulable_deadline = deadline_after.unwrap_or(deadline);
                        if needs_cancel {
                            log_long_op!(stop_hrtimer(&timer_proxy, &timer_config));
                        }
                        hrtimer_status = Some(
                            schedule_hrtimer(
                                scope.clone(),
                                now,
                                &timer_proxy,
                                schedulable_deadline,
                                snd.clone(),
                                &timer_config,
                                &schedule_delay_prop,
                            )
                            .await,
                        );
                    }
                }
            }
            Cmd::StopById { timer_id, done } => {
                trace::duration!(c"alarms", c"Cmd::StopById", "alarm_id" => &timer_id.alarm_id[..]);
                fuchsia_trace::flow_step!(
                    c"alarms",
                    c"hrtimer_lifecycle",
                    as_trace_id(&timer_id.alarm_id)
                );
                debug!("wake_timer_loop: STOP timer: {}", timer_id);
                let deadline_before = timers.peek_deadline();

                if let Some(mut timer_node) = timers.remove_by_id(&timer_id) {
                    let deadline_after = timers.peek_deadline();

                    if let Some(responder) = timer_node.take_responder() {
                        // We must reply to the responder to keep the connection open.
                        responder.send(Err(fta::WakeAlarmsError::Dropped)).expect("infallible");
                    }
                    if is_deadline_changed(deadline_before, deadline_after) {
                        log_long_op!(stop_hrtimer(&timer_proxy, &timer_config));
                    }
                    if let Some(deadline) = deadline_after {
                        // Reschedule the hardware timer if the removed timer is the earliest one,
                        // and another one exists.
                        let new_timer_state = schedule_hrtimer(
                            scope.clone(),
                            now,
                            &timer_proxy,
                            deadline,
                            snd.clone(),
                            &timer_config,
                            &schedule_delay_prop,
                        )
                        .await;
                        let old_hrtimer_status = hrtimer_status.replace(new_timer_state);
                        if let Some(task) = old_hrtimer_status.map(|ev| ev.task) {
                            // Allow the task to complete, I suppose.
                            task.await;
                        }
                    } else {
                        // No next timer, clean up the hrtimer status.
                        hrtimer_status = None;
                    }
                } else {
                    debug!("wake_timer_loop: STOP: no active timer to stop: {}", timer_id);
                }
                signal(&done);
            }
            Cmd::Alarm { expired_deadline, keep_alive } => {
                trace::duration!(c"alarms", c"Cmd::Alarm");
                // Expire all eligible timers, based on "now".  This is because
                // we may have woken up earlier than the actual deadline. This
                // happens for example if the timer can not make the actual
                // deadline and needs to be re-programmed.
                debug!(
                    "wake_timer_loop: ALARM!!! reached deadline: {}, wakey-wakey! {:?}",
                    format_timer(expired_deadline.into()),
                    keep_alive.get_koid().unwrap(),
                );
                let expired_count =
                    notify_all(&mut timers, &keep_alive, now, &slack_histogram_prop)
                        .expect("notification succeeds");
                if expired_count == 0 {
                    // This could be a resolution switch, or a straggler notification.
                    // Either way, the hardware timer is still ticking, cancel it.
                    debug!("wake_timer_loop: no expired alarms, reset hrtimer state");
                    log_long_op!(stop_hrtimer(&timer_proxy, &timer_config));
                }
                // There is a timer to reschedule, do that now.
                hrtimer_status = match timers.peek_deadline() {
                    None => None,
                    Some(deadline) => Some(
                        schedule_hrtimer(
                            scope.clone(),
                            now,
                            &timer_proxy,
                            deadline,
                            snd.clone(),
                            &timer_config,
                            &schedule_delay_prop,
                        )
                        .await,
                    ),
                }
            }
            Cmd::AlarmFidlError { expired_deadline, error } => {
                trace::duration!(c"alarms", c"Cmd::AlarmFidlError");
                // We do not have a wake lease, so the system may sleep before
                // we get to schedule a new timer. We have no way to avoid it
                // today.
                warn!(
                    "wake_timer_loop: FIDL error: {:?}, deadline: {}, now: {}",
                    error,
                    format_timer(expired_deadline.into()),
                    format_timer(now.into()),
                );
                // Manufacture a fake lease to make the code below work.
                // Maybe use Option instead?
                let (_dummy_lease, peer) = zx::EventPair::create();
                debug!("XXX: [{}] bogus lease: 1 {:?}", line!(), &peer.get_koid().unwrap());
                notify_all(&mut timers, &peer, now, &slack_histogram_prop)
                    .expect("notification succeeds");
                hrtimer_status = match timers.peek_deadline() {
                    None => None, // No remaining timers, nothing to schedule.
                    Some(deadline) => Some(
                        schedule_hrtimer(
                            scope.clone(),
                            now,
                            &timer_proxy,
                            deadline,
                            snd.clone(),
                            &timer_config,
                            &schedule_delay_prop,
                        )
                        .await,
                    ),
                }
            }
            Cmd::AlarmDriverError { expired_deadline, error } => {
                trace::duration!(c"alarms", c"Cmd::AlarmDriverError");
                let (_dummy_lease, peer) = zx::EventPair::create();
                debug!("XXX: [{}] bogus lease: {:?}", line!(), &peer.get_koid().unwrap());
                notify_all(&mut timers, &peer, now, &slack_histogram_prop)
                    .expect("notification succeeds");
                match error {
                    fidl_fuchsia_hardware_hrtimer::DriverError::Canceled => {
                        // Nothing to do here, cancelation is handled in Stop code.
                        debug!(
                            "wake_timer_loop: CANCELED timer at deadline: {}",
                            format_timer(expired_deadline.into())
                        );
                    }
                    _ => {
                        error!(
                            "wake_timer_loop: DRIVER SAYS: {:?}, deadline: {}, now: {}",
                            error,
                            format_timer(expired_deadline.into()),
                            format_timer(now.into()),
                        );
                        // We do not have a wake lease, so the system may sleep before
                        // we get to schedule a new timer. We have no way to avoid it
                        // today.
                        hrtimer_status = match timers.peek_deadline() {
                            None => None,
                            Some(deadline) => Some(
                                schedule_hrtimer(
                                    scope.clone(),
                                    now,
                                    &timer_proxy,
                                    deadline,
                                    snd.clone(),
                                    &timer_config,
                                    &schedule_delay_prop,
                                )
                                .await,
                            ),
                        }
                    }
                }
            }
        }

        {
            // Print and record diagnostics after each iteration, record the
            // duration for performance awareness.  Note that iterations happen
            // only occasionally, so these stats can remain unchanged for a long
            // time.
            trace::duration!(c"timekeeper", c"inspect");
            let now_formatted = format_timer(now.into());
            debug!("wake_timer_loop: now:                             {}", &now_formatted);
            now_formatted_prop.set(&now_formatted);

            let pending_timers_count: u64 =
                timers.timer_count().try_into().expect("always convertible");
            debug!("wake_timer_loop: currently pending timer count:   {}", pending_timers_count);
            pending_timers_count_prop.set(pending_timers_count);

            let pending_timers = format!("{}", timers);
            debug!("wake_timer_loop: currently pending timers:        {}", &timers);
            pending_timers_prop.set(&pending_timers);

            let current_deadline: String = hrtimer_status
                .as_ref()
                .map(|s| format!("{}", format_timer(s.deadline.into())))
                .unwrap_or_else(|| "(none)".into());
            debug!("wake_timer_loop: current hardware timer deadline: {:?}", current_deadline);
            current_hw_deadline_prop.set(&current_deadline);

            let remaining_duration_until_alarm = hrtimer_status
                .as_ref()
                .map(|s| format!("{}", format_duration((s.deadline - now).into())))
                .unwrap_or_else(|| "(none)".into());
            debug!(
                "wake_timer_loop: remaining duration until alarm:  {}",
                &remaining_duration_until_alarm
            );
            remaining_until_alarm_prop.set(&remaining_duration_until_alarm);
            debug!("---");
        }
    }

    debug!("wake_timer_loop: exiting. This is unlikely in prod code.");
}

/// Schedules a wake alarm.
///
/// Args:
/// - `scope`: used to spawn async tasks.
/// - `now`: the time instant used as the value of current instant.
/// - `hrtimer`: the proxy for the hrtimer device driver.
/// - `deadline`: the time instant in the future at which the alarm should fire.
/// - `command_send`: the sender channel to use when the timer expires.
/// - `timer_config`: a configuration of the hardware timer showing supported resolutions and
///   max tick value.
/// - `needs_cancel`: if set, we must first cancel a hrtimer before scheduling a new one.
async fn schedule_hrtimer(
    scope: fasync::ScopeHandle,
    now: fasync::BootInstant,
    hrtimer: &Box<dyn TimerOps>,
    deadline: fasync::BootInstant,
    mut command_send: mpsc::Sender<Cmd>,
    timer_config: &TimerConfig,
    schedule_delay_histogram: &finspect::IntExponentialHistogramProperty,
) -> TimerState {
    let timeout = std::cmp::max(zx::BootDuration::ZERO, deadline - now);
    trace::duration!(c"alarms", c"schedule_hrtimer", "timeout" => timeout.into_nanos());
    // When signaled, the hrtimer has been scheduled.
    let hrtimer_scheduled = zx::Event::create();

    debug!(
        "schedule_hrtimer:\n\tnow: {}\n\tdeadline: {}\n\ttimeout: {}",
        format_timer(now.into()),
        format_timer(deadline.into()),
        format_duration(timeout),
    );

    let slack = timer_config.pick_setting(timeout);

    let resolution_nanos = slack.resolution.into_nanos();
    let ticks = slack.ticks();
    trace::instant!(c"alarms", c"hrtimer:programmed",
        trace::Scope::Process,
        "resolution_ns" => resolution_nanos,
        "ticks" => ticks
    );
    let start_and_wait_fut = hrtimer.start_and_wait(
        timer_config.id,
        &ffhh::Resolution::Duration(resolution_nanos),
        ticks,
        clone_handle(&hrtimer_scheduled),
    );
    let hrtimer_scheduled_if_error = clone_handle(&hrtimer_scheduled);
    let hrtimer_task = scope.spawn_local(async move {
        debug!("hrtimer_task: waiting for hrtimer driver response");
        trace::instant!(c"alarms", c"hrtimer:started", trace::Scope::Process);
        let response = start_and_wait_fut.await;
        trace::instant!(c"alarms", c"hrtimer:response", trace::Scope::Process);
        match response {
            Err(TimerOpsError::Fidl(e)) => {
                defer! {
                    // Allow hrtimer_scheduled to proceed anyways.
                    signal(&hrtimer_scheduled_if_error);
                }
                trace::instant!(c"alarms", c"hrtimer:response:fidl_error", trace::Scope::Process);
                warn!("hrtimer_task: hrtimer FIDL error: {:?}", e);
                command_send
                    .start_send(Cmd::AlarmFidlError { expired_deadline: now, error: e })
                    .unwrap();
                // BAD: no way to keep alive.
            }
            Err(TimerOpsError::Driver(e)) => {
                defer! {
                    // This should be idempotent if the error occurs after
                    // the timer was scheduled.
                    signal(&hrtimer_scheduled_if_error);
                }
                let driver_error_str = format!("{:?}", e);
                trace::instant!(c"alarms", c"hrtimer:response:driver_error", trace::Scope::Process, "error" => &driver_error_str[..]);
                warn!("schedule_hrtimer: hrtimer driver error: {:?}", e);
                command_send
                    .start_send(Cmd::AlarmDriverError { expired_deadline: now, error: e })
                    .unwrap();
                // BAD: no way to keep alive.
            }
            Ok(keep_alive) => {
                trace::instant!(c"alarms", c"hrtimer:response:alarm", trace::Scope::Process);
                debug!("hrtimer: got alarm response: {:?}", keep_alive);
                // May trigger sooner than the deadline.
                command_send
                    .start_send(Cmd::Alarm { expired_deadline: deadline, keep_alive })
                    .unwrap();
            }
        }
        debug!("hrtimer_task: exiting task.");
        trace::instant!(c"alarms", c"hrtimer:task_exit", trace::Scope::Process);
    }).into();
    debug!("schedule_hrtimer: waiting for event to be signaled");

    // We must wait here to ensure that the wake alarm has been scheduled.
    log_long_op!(wait_signaled(&hrtimer_scheduled));

    let now_after_signaled = fasync::BootInstant::now();
    let duration_until_scheduled: zx::BootDuration = (now_after_signaled - now).into();
    if duration_until_scheduled > zx::BootDuration::from_nanos(LONG_DELAY_NANOS) {
        trace::duration!(c"alarms", c"schedule_hrtimer:unusual_duration",
            "duration" => duration_until_scheduled.into_nanos());
        warn!(
            "unusual duration until hrtimer scheduled: {}",
            format_duration(duration_until_scheduled)
        );
    }
    schedule_delay_histogram.insert(duration_until_scheduled.into_nanos());
    debug!("schedule_hrtimer: hrtimer wake alarm has been scheduled.");
    TimerState { task: hrtimer_task, deadline }
}

/// Notify all `timers` that `reference_instant` has been reached.
///
/// The notified `timers` are removed from the list of timers to notify.
///
/// Args:
/// - `timers`: the collection of currently available timers.
/// - `lease_prototype`: an EventPair used as a wake lease.
/// - `reference_instant`: the time instant used as a reference for alarm notification.
///   All timers
fn notify_all(
    timers: &mut Timers,
    lease_prototype: &zx::EventPair,
    reference_instant: fasync::BootInstant,
    unusual_slack_histogram: &finspect::IntExponentialHistogramProperty,
) -> Result<usize> {
    trace::duration!(c"alarms", c"notify_all");
    let now = fasync::BootInstant::now();
    let mut expired = 0;
    while let Some(mut timer_node) = timers.maybe_expire_earliest(reference_instant) {
        expired += 1;
        // How much later than requested did the notification happen.
        let deadline = *timer_node.get_deadline();
        let alarm_id = timer_node.get_alarm_id().to_string();
        trace::duration!(c"alarms", c"notify_all:notified", "alarm_id" => &*alarm_id);
        fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", as_trace_id(&alarm_id));
        let cid = timer_node.get_cid().clone();
        let slack: zx::BootDuration = deadline - now;
        if slack < zx::BootDuration::from_nanos(-LONG_DELAY_NANOS) {
            trace::duration!(c"alarms", c"schedule_hrtimer:unusual_slack", "slack" => slack.into_nanos());
            // This alarm triggered noticeably later than it should have.
            warn!(
                "alarm id: {} had an unusually large slack: {}",
                alarm_id,
                format_duration(slack)
            );
        }
        if slack < zx::BootDuration::ZERO {
            unusual_slack_histogram.insert(-slack.into_nanos());
        }
        debug!(
            concat!(
                "wake_alarm_loop: ALARM alarm_id: \"{}\"\n\tdeadline: {},\n\tcid: {:?},\n\t",
                "reference_instant: {},\n\tnow: {},\n\tslack: {}",
            ),
            alarm_id,
            format_timer(deadline.into()),
            cid,
            format_timer(reference_instant.into()),
            format_timer(now.into()),
            format_duration(slack),
        );
        let lease = clone_handle(lease_prototype);
        trace::instant!(c"alarms", c"notify", trace::Scope::Process, "alarm_id" => &alarm_id[..], "cid" => cid);
        let _ = timer_node
            .take_responder()
            .map(|r| r.send(Ok(lease)))
            .map_or_else(|| Ok(()), |res| res)
            .map_err(|e| error!("could not signal responder: {:?}", e));
        trace::instant!(c"alarms", c"notified", trace::Scope::Process);
    }
    trace::instant!(c"alarms", c"notify", trace::Scope::Process, "expired_count" => expired);
    debug!("notify_all: expired count: {}", expired);
    Ok(expired)
    // A new timer is not scheduled yet here.
}

/// The hrtimer driver service directory.  hrtimer driver APIs appear as randomly
/// named files in this directory. They are expected to come and go.
const HRTIMER_DIRECTORY: &str = "/dev/class/hrtimer";

/// Connects to the high resolution timer device driver.
pub async fn connect_to_hrtimer_async() -> Result<ffhh::DeviceProxy> {
    debug!("connect_to_hrtimer: trying directory: {}", HRTIMER_DIRECTORY);
    let dir =
        fuchsia_fs::directory::open_in_namespace(HRTIMER_DIRECTORY, fidl_fuchsia_io::PERM_READABLE)
            .with_context(|| format!("Opening {}", HRTIMER_DIRECTORY))?;
    let path = device_watcher::watch_for_files(&dir)
        .await
        .with_context(|| format!("Watching for files in {}", HRTIMER_DIRECTORY))?
        .try_next()
        .await
        .with_context(|| format!("Getting a file from {}", HRTIMER_DIRECTORY))?;
    let path = path.ok_or_else(|| anyhow!("Could not find {}", HRTIMER_DIRECTORY))?;
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("Could not find a valid str for {}", HRTIMER_DIRECTORY))?;
    connect_to_named_protocol_at_dir_root::<ffhh::DeviceMarker>(&dir, path)
        .context("Failed to connect built-in service")
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fuchsia_async::TestExecutor;
    use futures::select;
    use test_case::test_case;
    use test_util::{assert_gt, assert_lt};

    fn fake_wake_lease() -> fidl_fuchsia_power_system::LeaseToken {
        let (_lease, peer) = zx::EventPair::create();
        peer
    }

    #[test]
    fn timer_duration_no_overflow() {
        let duration1 = TimerDuration {
            resolution: zx::BootDuration::from_seconds(100_000_000),
            ticks: u64::MAX,
        };
        let duration2 = TimerDuration {
            resolution: zx::BootDuration::from_seconds(110_000_000),
            ticks: u64::MAX,
        };
        assert_eq!(duration1, duration1);
        assert_eq!(duration2, duration2);

        assert_lt!(duration1, duration2);
        assert_gt!(duration2, duration1);
    }

    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1)
    )]
    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(1), 10),
        TimerDuration::new(zx::BootDuration::from_nanos(10), 1)
    )]
    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(10), 1),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 10)
    )]
    #[test_case(
        TimerDuration::new(zx::BootDuration::from_micros(1), 1),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1000)
    )]
    fn test_slack_eq(one: TimerDuration, other: TimerDuration) {
        assert_eq!(one, other);
    }

    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 2)
    )]
    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1),
        TimerDuration::new(zx::BootDuration::from_nanos(10), 1)
    )]
    fn test_slack_lt(one: TimerDuration, other: TimerDuration) {
        assert_lt!(one, other);
    }

    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(1), 2),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1)
    )]
    #[test_case(
        TimerDuration::new(zx::BootDuration::from_nanos(10), 1),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 1)
    )]
    fn test_slack_gt(one: TimerDuration, other: TimerDuration) {
        assert_gt!(one, other);
    }

    #[test_case(
        vec![zx::BootDuration::from_nanos(1)],
        100,
        zx::BootDuration::from_nanos(0),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 0) ; "Exact at 0x1ns"
    )]
    #[test_case(
        vec![zx::BootDuration::from_nanos(1)],
        100,
        zx::BootDuration::from_nanos(50),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 50) ; "Exact at 50x1ns"
    )]
    #[test_case(
        vec![zx::BootDuration::from_nanos(2)],
        100,
        zx::BootDuration::from_nanos(50),
        TimerDuration::new(zx::BootDuration::from_nanos(2), 25) ; "Exact at 25x2ns"
    )]
    #[test_case(
        vec![zx::BootDuration::from_nanos(3)],
        100,
        zx::BootDuration::from_nanos(50),
        // The closest duration is 51ns.
        TimerDuration::new(zx::BootDuration::from_nanos(3), 17) ; "Inexact at 51ns"
    )]
    #[test_case(
        vec![
            zx::BootDuration::from_nanos(3),
            zx::BootDuration::from_nanos(4)
        ],
        100,
        zx::BootDuration::from_nanos(50),
        TimerDuration::new(zx::BootDuration::from_nanos(3), 17) ; "3ns is a better resolution"
    )]
    #[test_case(
        vec![
            zx::BootDuration::from_nanos(1000),
        ],
        100,
        zx::BootDuration::from_nanos(50),
        TimerDuration::new(zx::BootDuration::from_nanos(1000), 1) ;
        "950ns negative slack is the best we can do"
    )]
    #[test_case(
        vec![
            zx::BootDuration::from_nanos(1),
        ],
        10,
        zx::BootDuration::from_nanos(50),
        TimerDuration::new(zx::BootDuration::from_nanos(1), 10) ;
        "10ns positive slack is the best we can do"
    )]
    #[test_case(
        vec![
            zx::BootDuration::from_millis(1),
            zx::BootDuration::from_micros(100),
            zx::BootDuration::from_micros(10),
            zx::BootDuration::from_micros(1),
        ],
        20,  // Make only one of the resolutions above match.
        zx::BootDuration::from_micros(150),
        TimerDuration::new(zx::BootDuration::from_micros(10), 15) ;
        "Realistic case with resolutions from driver, should be 15us"
    )]
    #[test_case(
        vec![
            zx::BootDuration::from_millis(1),
            zx::BootDuration::from_micros(100),
            zx::BootDuration::from_micros(10),
            zx::BootDuration::from_micros(1),
        ],
        2000,  // Make only one of the resolutions above match.
        zx::BootDuration::from_micros(6000),
        TimerDuration::new(zx::BootDuration::from_millis(1), 6) ;
        "Coarser exact unit wins"
    )]
    fn test_pick_setting(
        resolutions: Vec<zx::BootDuration>,
        max_ticks: u64,
        duration: zx::BootDuration,
        expected: TimerDuration,
    ) {
        let config = TimerConfig::new_from_data(MAIN_TIMER_ID as u64, &resolutions[..], max_ticks);
        let actual = config.pick_setting(duration);

        // .eq() does not work here, since we do not just require that the values
        // be equal, but also that the same resolution is used in both.
        assert_slack_eq(expected, actual);
    }

    // TimerDuration assertion with human-friendly output in case of an error.
    fn assert_slack_eq(expected: TimerDuration, actual: TimerDuration) {
        let slack = expected.duration() - actual.duration();
        assert_eq!(
            actual.resolution(),
            expected.resolution(),
            "\n\texpected: {} ({})\n\tactual  : {} ({})\n\tslack: expected-actual={}",
            expected,
            format_duration(expected.duration()),
            actual,
            format_duration(actual.duration()),
            format_duration(slack)
        );
        assert_eq!(
            actual.ticks(),
            expected.ticks(),
            "\n\texpected: {} ({})\n\tactual  : {} ({})\n\tslack: expected-actual={}",
            expected,
            format_duration(expected.duration()),
            actual,
            format_duration(actual.duration()),
            format_duration(slack)
        );
    }

    #[derive(Debug)]
    enum FakeCmd {
        SetProperties {
            resolutions: Vec<zx::BootDuration>,
            max_ticks: i64,
            keep_alive: zx::EventPair,
            done: zx::Event,
        },
    }

    use std::cell::RefCell;
    use std::rc::Rc;

    // A fake that emulates some aspects of the hrtimer driver.
    //
    // Specifically it can be configured with different resolutions, and will
    // bomb out if any waiting methods are called twice in a succession, without
    // canceling the timer in between.
    fn fake_hrtimer_connection(
        scope: fasync::ScopeHandle,
        rcv: mpsc::Receiver<FakeCmd>,
    ) -> ffhh::DeviceProxy {
        debug!("fake_hrtimer_connection: entry.");
        let (hrtimer, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<ffhh::DeviceMarker>();
        scope.clone().spawn_local(async move {
            let mut rcv = rcv.fuse();
            let timer_properties = Rc::new(RefCell::new(None));
            let wake_lease = Rc::new(RefCell::new(None));

            // Set to true when the hardware timer is supposed to be running.
            // Hardware timer may not be reprogrammed without canceling it first,
            // make sure the tests fail the same way as production would.
            let timer_running = Rc::new(RefCell::new(false));

            loop {
                let timer_properties = timer_properties.clone();
                let wake_lease = wake_lease.clone();
                select! {
                    cmd = rcv.next() => {
                        debug!("fake_hrtimer_connection: cmd: {:?}", cmd);
                        match cmd {
                            Some(FakeCmd::SetProperties{ resolutions, max_ticks, keep_alive, done}) => {
                                let mut timer_props = vec![];
                                for v in 0..10 {
                                    timer_props.push(ffhh::TimerProperties {
                                        supported_resolutions: Some(
                                            resolutions.iter()
                                                .map(|d| ffhh::Resolution::Duration(d.into_nanos())).collect()),
                                        max_ticks: Some(max_ticks.try_into().unwrap()),
                                        // start_and_wait method works.
                                        supports_wait: Some(true),
                                        id: Some(v),
                                        ..Default::default()
                                        },
                                    );
                                }
                                *timer_properties.borrow_mut() = Some(timer_props);
                                *wake_lease.borrow_mut() = Some(keep_alive);
                                debug!("set timer properties to: {:?}", timer_properties);
                                signal(&done);
                            }
                            e => {
                                panic!("unrecognized command: {:?}", e);
                            }
                        }
                        // Set some responses if we have them.
                    },
                    event = stream.next() => {
                        debug!("fake_hrtimer_connection: event: {:?}", event);
                        if let Some(Ok(event)) = event {
                            match event {
                                ffhh::DeviceRequest::Start { responder, .. } => {
                                    assert!(!*timer_running.borrow(), "invariant broken: timer may not be running here");
                                    *timer_running.borrow_mut() = true;
                                    responder.send(Ok(())).expect("");
                                }
                                ffhh::DeviceRequest::Stop { responder, .. } => {
                                    *timer_running.borrow_mut() = false;
                                    responder.send(Ok(())).expect("");
                                }
                                ffhh::DeviceRequest::GetTicksLeft { responder, .. } => {
                                    responder.send(Ok(1)).expect("");
                                }
                                ffhh::DeviceRequest::SetEvent { responder, .. } => {
                                    responder.send(Ok(())).expect("");
                                }
                                ffhh::DeviceRequest::StartAndWait { id, resolution, ticks, setup_event, responder, .. } => {
                                    assert!(!*timer_running.borrow(), "invariant broken: timer may not be running here");
                                    *timer_running.borrow_mut() = true;
                                    debug!("fake_hrtimer_connection: starting timer: \"{}\", resolution: {:?}, ticks: {}", id, resolution, ticks);
                                    let ticks: i64 = ticks.try_into().unwrap();
                                    let sleep_duration  = zx::BootDuration::from_nanos(ticks * match resolution {
                                        ffhh::Resolution::Duration(e) => e,
                                        _ => {
                                            error!("resolution has an unexpected value");
                                            1
                                        }
                                    });
                                    let timer_running_clone = timer_running.clone();
                                    scope.spawn_local(async move {
                                        // Signaling the setup event allows the client to proceed
                                        // with post-scheduling work.
                                        signal(&setup_event);

                                        // Respond after the requested sleep time. In tests this will
                                        // be sleeping in fake time.
                                        fasync::Timer::new(sleep_duration).await;
                                        *timer_running_clone.borrow_mut() = false;
                                        responder.send(Ok(clone_handle(wake_lease.borrow().as_ref().unwrap()))).unwrap();
                                    });
                                }
                                ffhh::DeviceRequest::StartAndWait2 { responder, .. } => {
                                    assert!(!*timer_running.borrow(), "invariant broken: timer may not be running here");
                                    *timer_running.borrow_mut() = true;
                                    responder.send(Err(ffhh::DriverError::InternalError)).expect("");
                                }
                                ffhh::DeviceRequest::GetProperties { responder, .. } => {
                                    if (*timer_properties).borrow().is_none() {
                                        error!("timer_properties is empty, this is not what you want!");
                                    }
                                    responder
                                        .send(ffhh::Properties {
                                            timers_properties: (*timer_properties).borrow().clone(),
                                            ..Default::default()
                                        })
                                        .expect("");
                                }
                                ffhh::DeviceRequest::ReadTimer { responder, .. } => {
                                    responder.send(Err(ffhh::DriverError::NotSupported)).expect("");
                                }
                                ffhh::DeviceRequest::ReadClock { responder, .. } => {
                                    responder.send(Err(ffhh::DriverError::NotSupported)).expect("");
                                }
                                ffhh::DeviceRequest::_UnknownMethod { .. } => todo!(),
                            }
                        }
                    },
                }
            }
        });
        hrtimer
    }

    struct TestContext {
        wake_proxy: fta::WakeAlarmsProxy,
        _scope: fasync::Scope,
        _cmd_tx: mpsc::Sender<FakeCmd>,
    }

    impl TestContext {
        async fn new() -> Self {
            TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(0)).await;

            let scope = fasync::Scope::new();
            let (mut cmd_tx, wake_proxy) = {
                let (tx, rx) = mpsc::channel::<FakeCmd>(0);
                let hrtimer_proxy = fake_hrtimer_connection(scope.to_handle(), rx);

                let inspector = finspect::component::inspector();
                let alarms = Rc::new(Loop::new(
                    scope.to_handle(),
                    hrtimer_proxy,
                    inspector.root().create_child("test"),
                ));

                let (proxy, stream) =
                    fidl::endpoints::create_proxy_and_stream::<fta::WakeAlarmsMarker>();
                scope.spawn_local(async move {
                    serve(alarms, stream).await;
                });
                (tx, proxy)
            };

            let (_wake_lease, peer) = zx::EventPair::create();
            let done = zx::Event::create();
            cmd_tx
                .start_send(FakeCmd::SetProperties {
                    resolutions: vec![zx::Duration::from_nanos(1)],
                    max_ticks: 100,
                    keep_alive: peer,
                    done: done.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                })
                .unwrap();

            // Wait until hrtimer configuration has completed.
            assert_matches!(fasync::OnSignals::new(done, zx::Signals::EVENT_SIGNALED).await, Ok(_));

            Self { wake_proxy, _scope: scope, _cmd_tx: cmd_tx }
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            assert_matches!(TestExecutor::next_timer(), None, "Unexpected lingering timers");
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_basic_timed_wait() {
        let ctx = TestContext::new().await;

        let deadline = zx::BootInstant::from_nanos(100);
        let setup_done = zx::Event::create();
        let mut set_task = ctx.wake_proxy.set_and_wait(
            deadline.into(),
            fta::SetMode::NotifySetupDone(
                setup_done.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
            ),
            "Hello".into(),
        );

        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task).await, Poll::Pending);

        let mut setup_done_task = fasync::OnSignals::new(setup_done, zx::Signals::EVENT_SIGNALED);
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut setup_done_task).await,
            Poll::Ready(Ok(_)),
            "Setup event not triggered after scheduling an alarm"
        );

        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(100)).await;
        assert_matches!(TestExecutor::poll_until_stalled(set_task).await, Poll::Ready(Ok(Ok(_))));
    }

    #[test_case(100, 200 ; "push out")]
    #[test_case(200, 100 ; "pull in")]
    #[fuchsia::test(allow_stalls = false)]
    async fn test_two_alarms_different(
        // One timer scheduled at this instant (fake time starts from zero).
        first_deadline_nanos: i64,
        // Another timer scheduled at this instant.
        second_deadline_nanos: i64,
    ) {
        let ctx = TestContext::new().await;

        let mut set_task_1 = ctx.wake_proxy.set_and_wait(
            fidl::BootInstant::from_nanos(first_deadline_nanos),
            fta::SetMode::KeepAlive(fake_wake_lease()),
            "Hello1".into(),
        );
        let mut set_task_2 = ctx.wake_proxy.set_and_wait(
            fidl::BootInstant::from_nanos(second_deadline_nanos),
            fta::SetMode::KeepAlive(fake_wake_lease()),
            "Hello2".into(),
        );

        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_1).await, Poll::Pending);
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_2).await, Poll::Pending);

        // Sort alarms by their deadlines.
        let mut tasks = [(first_deadline_nanos, set_task_1), (second_deadline_nanos, set_task_2)];
        tasks.sort_by(|a, b| a.0.cmp(&b.0));
        let [mut first_task, mut second_task] = tasks;

        // Alarms should fire in order of deadlines.
        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(first_task.0)).await;
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut first_task.1).await,
            Poll::Ready(Ok(Ok(_)))
        );
        assert_matches!(TestExecutor::poll_until_stalled(&mut second_task.1).await, Poll::Pending);

        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(second_task.0)).await;
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut second_task.1).await,
            Poll::Ready(Ok(Ok(_)))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_two_alarms_same() {
        const DEADLINE_NANOS: i64 = 100;

        let ctx = TestContext::new().await;

        let mut set_task_1 = ctx.wake_proxy.set_and_wait(
            fidl::BootInstant::from_nanos(DEADLINE_NANOS),
            fta::SetMode::KeepAlive(fake_wake_lease()),
            "Hello1".into(),
        );
        let mut set_task_2 = ctx.wake_proxy.set_and_wait(
            fidl::BootInstant::from_nanos(DEADLINE_NANOS),
            fta::SetMode::KeepAlive(fake_wake_lease()),
            "Hello2".into(),
        );

        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_1).await, Poll::Pending);
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_2).await, Poll::Pending);

        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(DEADLINE_NANOS)).await;

        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task_1).await,
            Poll::Ready(Ok(Ok(_)))
        );
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task_2).await,
            Poll::Ready(Ok(Ok(_)))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_alarm_immediate() {
        let ctx = TestContext::new().await;
        let mut set_task = ctx.wake_proxy.set_and_wait(
            fidl::BootInstant::INFINITE_PAST,
            fta::SetMode::KeepAlive(fake_wake_lease()),
            "Hello1".into(),
        );
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task).await,
            Poll::Ready(Ok(Ok(_)))
        );
    }

    // Rescheduling a timer will cancel the earlier call and use the new
    // deadline for the later call.
    #[test_case(200, 100 ; "pull in")]
    #[test_case(100, 200 ; "push out")]
    #[fuchsia::test(allow_stalls = false)]
    async fn test_reschedule(initial_deadline_nanos: i64, override_deadline_nanos: i64) {
        const ALARM_ID: &str = "Hello";

        let ctx = TestContext::new().await;

        let schedule = |deadline_nanos: i64| {
            let setup_done = zx::Event::create();
            let task = ctx.wake_proxy.set_and_wait(
                fidl::BootInstant::from_nanos(deadline_nanos),
                fta::SetMode::NotifySetupDone(
                    setup_done.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                ),
                ALARM_ID.into(),
            );
            (task, setup_done)
        };

        // Schedule timer with a long timeout first. Let it wait, then
        // try to reschedule the same timer
        let (mut set_task_1, setup_done_1) = schedule(initial_deadline_nanos);
        fasync::OnSignals::new(setup_done_1, zx::Signals::EVENT_SIGNALED).await.unwrap();
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_1).await, Poll::Pending);

        // Schedule the same timer as above, but with a shorter deadline. This
        // should cancel the earlier call.
        let (mut set_task_2, setup_done_2) = schedule(override_deadline_nanos);
        fasync::OnSignals::new(setup_done_2, zx::Signals::EVENT_SIGNALED).await.unwrap();
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task_1).await,
            Poll::Ready(Ok(Err(fta::WakeAlarmsError::Dropped)))
        );
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_2).await, Poll::Pending);

        // The later call will be fired exactly on the new shorter deadline.
        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(override_deadline_nanos - 1))
            .await;
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_2).await, Poll::Pending);
        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(override_deadline_nanos))
            .await;
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task_2).await,
            Poll::Ready(Ok(Ok(_)))
        );

        // The values in the inspector tree are fixed because the test
        // runs fully deterministically in fake time.
        assert_data_tree!(finspect::component::inspector(), root: {
            test: {
                hardware: {
                    // All alarms fired, so this should be "none".
                    current_deadline: "(none)",
                    remaining_until_alarm: "(none)",
                },
                now_formatted: format!("{override_deadline_nanos}ns ({override_deadline_nanos})"),
                now_ns: override_deadline_nanos,
                pending_timers: "\n\t",
                pending_timers_count: 0u64,
                requested_deadlines_ns: AnyProperty,
                schedule_delay_ns: AnyProperty,
                slack_ns: AnyProperty,
            },
        });
    }

    // Rescheduling a timer with the same deadline will fail with WakeAlarmsError::Dropped.
    //
    // TODO(http://b/430638703): Consider changing this behavior.
    #[fuchsia::test(allow_stalls = false)]
    async fn test_reschedule_same() {
        const ALARM_ID: &str = "Hello";
        const DEADLINE_NANOS: i64 = 100;

        let ctx = TestContext::new().await;

        let schedule = |deadline_nanos: i64| {
            let setup_done = zx::Event::create();
            let task = ctx.wake_proxy.set_and_wait(
                fidl::BootInstant::from_nanos(deadline_nanos),
                fta::SetMode::NotifySetupDone(
                    setup_done.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                ),
                ALARM_ID.into(),
            );
            (task, setup_done)
        };

        // Schedule timer with a long timeout first. Let it wait, then
        // try to reschedule the same timer
        let (mut set_task_1, setup_done_1) = schedule(DEADLINE_NANOS);
        fasync::OnSignals::new(setup_done_1, zx::Signals::EVENT_SIGNALED).await.unwrap();
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_1).await, Poll::Pending);

        // Schedule the same timer as above, but with a shorter deadline. This
        // should cancel the earlier call.
        let (mut set_task_2, setup_done_2) = schedule(DEADLINE_NANOS);
        fasync::OnSignals::new(setup_done_2, zx::Signals::EVENT_SIGNALED).await.unwrap();
        assert_matches!(TestExecutor::poll_until_stalled(&mut set_task_1).await, Poll::Pending);
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task_2).await,
            Poll::Ready(Ok(Err(fta::WakeAlarmsError::Dropped)))
        );

        // The later call will be fired exactly on the new shorter deadline.
        TestExecutor::advance_to(fasync::MonotonicInstant::from_nanos(DEADLINE_NANOS)).await;
        assert_matches!(
            TestExecutor::poll_until_stalled(&mut set_task_1).await,
            Poll::Ready(Ok(Ok(_)))
        );
    }

    // If we get two scheduling FIDL errors one after another, the wake alarm
    // manager must not lock up.
    #[fuchsia::test(allow_stalls = false)]
    async fn test_fidl_error_on_reschedule() {
        const DEADLINE_NANOS: i64 = 100;

        let (wake_proxy, _stream) =
            fidl::endpoints::create_proxy_and_stream::<fta::WakeAlarmsMarker>();
        drop(_stream);

        assert_matches!(
            wake_proxy
                .set_and_wait(
                    zx::BootInstant::from_nanos(DEADLINE_NANOS).into(),
                    fta::SetMode::KeepAlive(fake_wake_lease()),
                    "hello1".into(),
                )
                .await,
            Err(fidl::Error::ClientChannelClosed { .. })
        );

        assert_matches!(
            wake_proxy
                .set_and_wait(
                    zx::BootInstant::from_nanos(DEADLINE_NANOS).into(),
                    fta::SetMode::KeepAlive(fake_wake_lease()),
                    "hello2".into(),
                )
                .await,
            Err(fidl::Error::ClientChannelClosed { .. })
        );
    }
}
