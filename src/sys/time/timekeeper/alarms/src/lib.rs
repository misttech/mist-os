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

use anyhow::{anyhow, Result};
use fidl::encoding::ProxyChannelBox;
use fidl::endpoints::RequestStream;
use fidl::HandleBased;
use fuchsia_inspect::{HistogramProperty, Property};
use futures::channel::mpsc;
use futures::sink::SinkExt;
use futures::StreamExt;
use log::{debug, error, warn};
use scopeguard::defer;
use std::cell::RefCell;
use std::cmp;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::rc::Rc;
use std::sync::LazyLock;
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

/// TODO(b/383062441): remove this special casing once Starnix hrtimer is fully
/// migrated to multiplexed timer.
/// A special-cased Starnix timer ID, used to allow cross-connection setup
/// for Starnix only.
const TEMPORARY_STARNIX_TIMER_ID: &str = "starnix-hrtimer";
static TEMPORARY_STARNIX_CID: LazyLock<zx::Event> = LazyLock::new(|| zx::Event::create());

// This may be already handled by something, but I don't want new deps.
const USEC_IN_NANOS: i64 = 1000;
const MSEC_IN_NANOS: i64 = 1000 * USEC_IN_NANOS;
const SEC_IN_NANOS: i64 = 1000 * MSEC_IN_NANOS;
const MIN_IN_NANOS: i64 = SEC_IN_NANOS * 60;
const HOUR_IN_NANOS: i64 = MIN_IN_NANOS * 60;
const DAY_IN_NANOS: i64 = HOUR_IN_NANOS * 24;
const WEEK_IN_NANOS: i64 = DAY_IN_NANOS * 7;
const YEAR_IN_NANOS: i64 = DAY_IN_NANOS * 365; // Approximate.

static UNITS: LazyLock<Vec<(i64, &'static str)>> = LazyLock::new(|| {
    vec![
        (YEAR_IN_NANOS, "year(s)"),
        (WEEK_IN_NANOS, "week(s)"),
        (DAY_IN_NANOS, "day(s)"),
        (HOUR_IN_NANOS, "h"),
        (MIN_IN_NANOS, "min"),
        (SEC_IN_NANOS, "s"),
        (MSEC_IN_NANOS, "ms"),
        (USEC_IN_NANOS, "μs"),
        (1, "ns"),
    ]
});

// Formats a time value into a simplistic human-readable string.  This is meant
// to be a human-friendly, but not an impeccable format.
fn format_common(mut value: i64) -> String {
    let value_copy = value;
    let mut repr: Vec<String> = vec![];
    for (unit_value, unit_str) in UNITS.iter() {
        if value == 0 {
            break;
        }
        let num_units = value / unit_value;
        if num_units.abs() > 0 {
            repr.push(format!("{}{}", num_units, unit_str));
            value = value % unit_value;
        }
    }
    if repr.len() == 0 {
        repr.push("0ns".to_string());
    }
    // 1year(s)_3week(s)_4day(s)_1h_2m_340ms. Not ideal but user-friendly enough.
    let repr = repr.join("_");

    let mut ret = vec![];
    ret.push(repr);
    // Also add the full nanosecond value too.
    ret.push(format!("({})", value_copy));
    ret.join(" ")
}

// Pretty prints a timer value into a simplistic format.
fn format_timer<T: zx::Timeline>(timer: zx::Instant<T>) -> String {
    format_common(timer.into_nanos())
}

// Pretty prints a duration into a simplistic format.
fn format_duration<T: zx::Timeline>(duration: zx::Duration<T>) -> String {
    format_common(duration.into_nanos())
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

/// Stops a currently running hardware timer.
async fn stop_hrtimer(hrtimer: &ffhh::DeviceProxy) {
    trace::duration!(c"alarms", c"hrtimer:stop");
    debug!("stop_hrtimer: stopping hardware timer");
    let _ = hrtimer
        .stop(MAIN_TIMER_ID.try_into().expect("infallible"))
        .await
        .map(|result| {
            let _ = result.map_err(|e| warn!("stop_hrtimer: driver error: {:?}", e));
        })
        .map_err(|e| warn!("stop_hrtimer: could not stop prior timer: {}", e));
    debug!("stop_hrtimer: stopped  hardware timer");
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
        /// A wake lease token. Hold onto this value while we must prevent the
        /// system from going to sleep.
        ///
        /// This is important so that wake alarms can be scheduled before we
        /// allow the system to go to sleep.
        setup_done: zx::Event,
        /// An alarm identifier, chosen by the caller.
        alarm_id: String,
        /// A responder that will be called when the timer expires. The
        /// client end of the connection will block until we send something
        /// on this responder.
        ///
        /// This is packaged into a Rc... only because both the "happy path"
        /// and the error path must consume the responder.  This allows them
        /// to be consumed, without the responder needing to implement Default.
        responder: Rc<RefCell<Option<fta::WakeSetAndWaitResponder>>>,
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

/// Extracts a KOID from the undelrying channel of the provided [stream].
///
/// # Returns
/// - zx::Koid: the KOID you wanted.
/// - fta::WakeRequestStream: the stream; we had to deconstruct it briefly,
///   so this gives it back to you.
pub fn get_stream_koid(stream: fta::WakeRequestStream) -> (zx::Koid, fta::WakeRequestStream) {
    let (inner, is_terminated) = stream.into_inner();
    let koid = inner.channel().as_channel().get_koid().expect("infallible");
    let stream = fta::WakeRequestStream::from_inner(inner, is_terminated);
    (koid, stream)
}

/// Serves a single Wake API client.
pub async fn serve(timer_loop: Rc<Loop>, requests: fta::WakeRequestStream) {
    // Compute the request ID somehow.
    fasync::Task::local(async move {
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
    })
    .detach();
}

// Inject a constant KOID as connection ID (cid) if the singular alarm ID corresponds to a Starnix
// alarm.
// TODO(b/383062441): remove this special casing.
fn compute_cid(cid: zx::Koid, alarm_id: &str) -> zx::Koid {
    if alarm_id == TEMPORARY_STARNIX_TIMER_ID {
        // Temporarily, the Starnix timer is a singleton and always gets the
        // same CID.
        TEMPORARY_STARNIX_CID.as_handle_ref().get_koid().expect("infallible")
    } else {
        cid
    }
}

async fn handle_cancel(alarm_id: String, cid: zx::Koid, cmd: &mut mpsc::Sender<Cmd>) {
    let done = zx::Event::create();
    let cid = compute_cid(cid, &alarm_id);
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
async fn handle_request(cid: zx::Koid, mut cmd: mpsc::Sender<Cmd>, request: fta::WakeRequest) {
    match request {
        fta::WakeRequest::SetAndWait { deadline, setup_done, alarm_id, responder } => {
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
            let cid = compute_cid(cid, &alarm_id);

            // Alarm is not scheduled yet!
            debug!(
                "handle_request: scheduling alarm_id: \"{}\"\n\tcid: {:?}\n\tdeadline: {}",
                alarm_id,
                cid,
                format_timer(deadline.into())
            );
            // Expected to return quickly.
            if let Err(e) = cmd
                .send(Cmd::Start {
                    cid,
                    deadline: deadline.into(),
                    setup_done,
                    alarm_id: alarm_id.clone(),
                    responder: responder.clone(),
                })
                .await
            {
                warn!("handle_request: error while trying to schedule `{}`: {:?}", alarm_id, e);
                responder
                    .borrow_mut()
                    .take()
                    .expect("always present if call fails")
                    .send(Err(fta::WakeError::Internal))
                    .unwrap();
            }
        }
        fta::WakeRequest::Cancel { alarm_id, .. } => {
            // TODO: b/383062441 - make this into an async task so that we wait
            // less to schedule the next alarm.
            handle_cancel(alarm_id, cid, &mut cmd).await;
        }
        // Similar to above, but wait for the cancel to complete.
        fta::WakeRequest::CancelSync { alarm_id, responder, .. } => {
            handle_cancel(alarm_id, cid, &mut cmd).await;
            responder.send(Ok(())).expect("infallible");
        }
        fta::WakeRequest::GetProperties { responder, .. } => {
            let response =
                fta::WakeGetPropertiesResponse { is_supported: Some(true), ..Default::default() };
            debug!("sending: Wake.GetProperties: {:?}", &response);
            responder.send(&response).expect("send success");
        }
        fta::WakeRequest::_UnknownMethod { .. } => {}
    };
}

/// Represents a single alarm event processing loop.
///
/// One instance is created per each alarm-capable low-level device.
pub struct Loop {
    // The task executing the alarm event processing [Loop].
    _task: fasync::Task<()>,
    // Given to any clients that need to send messages to `_task`
    // via [get_sender].
    snd_cloneable: mpsc::Sender<Cmd>,
}

impl Loop {
    /// Creates a new instance of [Loop].
    ///
    /// `device_proxy` is a connection to a low-level timer device.
    pub fn new(device_proxy: ffhh::DeviceProxy, inspect: finspect::Node) -> Self {
        let (snd, rcv) = mpsc::channel(CHANNEL_SIZE);
        let snd_clone = snd.clone();
        let _task = fasync::Task::local(async move {
            wake_timer_loop(snd_clone, rcv, device_proxy, inspect).await
        });
        Self { _task, snd_cloneable: snd }
    }

    /// Gets a copy of a channel through which async commands may be sent to
    /// the [Loop].
    fn get_sender(&self) -> mpsc::Sender<Cmd> {
        self.snd_cloneable.clone()
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
    responder: Option<fta::WakeSetAndWaitResponder>,
}

impl TimerNode {
    fn new(
        deadline: fasync::BootInstant,
        alarm_id: String,
        cid: zx::Koid,
        responder: fta::WakeSetAndWaitResponder,
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

    fn take_responder(&mut self) -> Option<fta::WakeSetAndWaitResponder> {
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
            r.send(Err(fta::WakeError::Dropped))
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
fn clone_handle<H: HandleBased>(handle: &H) -> H {
    handle.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("infallible")
}

async fn wait_signaled<H: HandleBased>(handle: &H) {
    fasync::OnSignals::new(handle, zx::Signals::EVENT_SIGNALED).await.expect("infallible");
}

fn signal<H: HandleBased>(handle: &H) {
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
        let self_nanos = self.resolution_as_nanos() * self.ticks;
        let other_nanos = other.resolution_as_nanos() * other.ticks;
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
struct TimerConfig {
    /// The resolutions supported by this timer. Each entry is one possible
    /// duration for on timer "tick".  The resolution is picked when a timer
    /// request is sent.
    resolutions: Vec<zx::BootDuration>,
    /// The maximum count of "ticks" that the timer supports. The timer usually
    /// has a register that counts up or down based on a clock signal with
    /// the period specified by `resolutions`.  This is the maximum value that
    /// the counter can count to without overflowing.
    max_ticks: u64,
}

impl TimerConfig {
    /// Creates a new timer config with supported timer resolutions and the max
    /// ticks value for the timer's counter.
    fn new_from_data(resolutions: &[zx::BootDuration], max_ticks: u64) -> Self {
        debug!(
            "TimerConfig: resolutions: {:?}, max_ticks: {}",
            resolutions.iter().map(|r| format_duration(*r)).collect::<Vec<_>>(),
            max_ticks
        );
        let resolutions = resolutions.iter().map(|d| *d).collect::<Vec<zx::BootDuration>>();
        TimerConfig { resolutions, max_ticks }
    }

    fn new_empty() -> Self {
        error!("TimerConfig::new_empty() called, this is not OK.");
        TimerConfig { resolutions: vec![], max_ticks: 0 }
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
                if smallest_unit < best_negative_slack {
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
            ret.duration().into_nanos() > 0,
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

async fn get_timer_properties(hrtimer: &ffhh::DeviceProxy) -> TimerConfig {
    debug!("get_timer_properties: requesting timer properties.");
    match hrtimer.get_properties().await {
        Ok(p) => {
            let timers_properties = &p.timers_properties.expect("timers_properties must exist");
            let main_timer_properties = &timers_properties[MAIN_TIMER_ID];
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
            TimerConfig::new_from_data(resolutions, max_ticks)
        }
        Err(e) => {
            error!("could not get timer properties: {:?}", e);
            TimerConfig::new_empty()
        }
    }
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
    snd: mpsc::Sender<Cmd>,
    mut cmds: mpsc::Receiver<Cmd>,
    timer_proxy: ffhh::DeviceProxy,
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
            Cmd::Start { cid, deadline, setup_done, alarm_id, responder } => {
                trace::duration!(c"alarms", c"Cmd::Start");
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
                    // Must signal once the setup is completed.
                    signal(&setup_done);
                    debug!("wake_timer_loop: START: setup_done signaled");
                };
                deadline_histogram_prop.insert((deadline - now).into_nanos());
                if Timers::expired(now, deadline) {
                    trace::duration!(c"alarms", c"Cmd::Start:immediate");
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
                            stop_hrtimer(&timer_proxy).await;
                        }
                        hrtimer_status = Some(
                            schedule_hrtimer(
                                now,
                                &timer_proxy,
                                schedulable_deadline,
                                snd.clone(),
                                &timer_config,
                            )
                            .await,
                        );
                    }
                }
            }
            Cmd::StopById { timer_id, done } => {
                trace::duration!(c"alarms", c"Cmd::StopById", "alarm_id" => &timer_id.alarm_id[..]);
                debug!("wake_timer_loop: STOP timer: {}", timer_id);
                let deadline_before = timers.peek_deadline();

                if let Some(mut timer_node) = timers.remove_by_id(&timer_id) {
                    let deadline_after = timers.peek_deadline();

                    if let Some(responder) = timer_node.take_responder() {
                        // We must reply to the responder to keep the connection open.
                        responder.send(Err(fta::WakeError::Dropped)).expect("infallible");
                    }
                    if is_deadline_changed(deadline_before, deadline_after) {
                        stop_hrtimer(&timer_proxy).await;
                    }
                    if let Some(deadline) = deadline_after {
                        // Reschedule the hardware timer if the removed timer is the earliest one,
                        // and another one exists.
                        let new_timer_state = schedule_hrtimer(
                            now,
                            &timer_proxy,
                            deadline,
                            snd.clone(),
                            &timer_config,
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
                    notify_all(&mut timers, &keep_alive, now).expect("notification succeeds");
                if expired_count == 0 {
                    // This could be a resolution switch, or a straggler notification.
                    // Either way, the hardware timer is still ticking, cancel it.
                    debug!("wake_timer_loop: no expired alarms, reset hrtimer state");
                    stop_hrtimer(&timer_proxy).await;
                }
                // There is a timer to reschedule, do that now.
                hrtimer_status = match timers.peek_deadline() {
                    None => None,
                    Some(deadline) => Some(
                        schedule_hrtimer(now, &timer_proxy, deadline, snd.clone(), &timer_config)
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
                notify_all(&mut timers, &peer, now).expect("notification succeeds");
                hrtimer_status = match timers.peek_deadline() {
                    None => None, // No remaining timers, nothing to schedule.
                    Some(deadline) => Some(
                        schedule_hrtimer(now, &timer_proxy, deadline, snd.clone(), &timer_config)
                            .await,
                    ),
                }
            }
            Cmd::AlarmDriverError { expired_deadline, error } => {
                trace::duration!(c"alarms", c"Cmd::AlarmDriverError");
                let (_dummy_lease, peer) = zx::EventPair::create();
                debug!("XXX: [{}] bogus lease: {:?}", line!(), &peer.get_koid().unwrap());
                notify_all(&mut timers, &peer, now).expect("notification succeeds");
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
                                    now,
                                    &timer_proxy,
                                    deadline,
                                    snd.clone(),
                                    &timer_config,
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
/// - `now`: the time instant used as the value of current instant.
/// - `hrtimer`: the proxy for the hrtimer device driver.
/// - `deadline`: the time instant in the future at which the alarm should fire.
/// - `command_send`: the sender channel to use when the timer expires.
/// - `timer_config`: a configuration of the hardware timer showing supported resolutions and
///   max tick value.
/// - `needs_cancel`: if set, we must first cancel a hrtimer before scheduling a new one.
async fn schedule_hrtimer(
    now: fasync::BootInstant,
    hrtimer: &ffhh::DeviceProxy,
    deadline: fasync::BootInstant,
    mut command_send: mpsc::Sender<Cmd>,
    timer_config: &TimerConfig,
) -> TimerState {
    let timeout = deadline - now;
    trace::duration!(c"alarms", c"schedule_hrtimer", "timeout" => timeout.into_nanos());
    assert!(
        now < deadline,
        "now: {}, deadline: {}, diff: {}",
        format_timer(now.into()),
        format_timer(deadline.into()),
        format_duration(timeout),
    );
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
        MAIN_TIMER_ID.try_into().expect("infallible"),
        &ffhh::Resolution::Duration(resolution_nanos),
        ticks,
        clone_handle(&hrtimer_scheduled),
    );
    let hrtimer_task = fasync::Task::local(async move {
        debug!("hrtimer_task: waiting for hrtimer driver response");
        trace::instant!(c"alarms", c"hrtimer:started", trace::Scope::Process);
        let response = start_and_wait_fut.await;
        trace::instant!(c"alarms", c"hrtimer:response", trace::Scope::Process);
        match response {
            Err(e) => {
                trace::instant!(c"alarms", c"hrtimer:response:fidl_error", trace::Scope::Process);
                debug!("hrtimer_task: hrtimer FIDL error: {:?}", e);
                command_send
                    .start_send(Cmd::AlarmFidlError { expired_deadline: now, error: e })
                    .unwrap();
                // BAD: no way to keep alive.
            }
            Ok(Err(e)) => {
                let driver_error_str = format!("{:?}", e);
                trace::instant!(c"alarms", c"hrtimer:response:driver_error", trace::Scope::Process, "error" => &driver_error_str[..]);
                debug!("schedule_hrtimer: hrtimer driver error: {:?}", e);
                command_send
                    .start_send(Cmd::AlarmDriverError { expired_deadline: now, error: e })
                    .unwrap();
                // BAD: no way to keep alive.
            }
            Ok(Ok(keep_alive)) => {
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
    });
    debug!("schedule_hrtimer: waiting for event to be signaled");

    // We must wait here to ensure that the wake alarm has been scheduled.
    wait_signaled(&hrtimer_scheduled).await;
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
) -> Result<usize> {
    trace::duration!(c"alarms", c"notify_all");
    let now = fasync::BootInstant::now();
    let mut expired = 0;
    while let Some(mut timer_node) = timers.maybe_expire_earliest(reference_instant) {
        expired += 1;
        // How much later than requested did the notification happen.
        let deadline = *timer_node.get_deadline();
        let alarm_id = timer_node.get_alarm_id().to_string();
        let cid = timer_node.get_cid().clone();
        let slack: zx::BootDuration = deadline - now;
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
pub fn connect_to_hrtimer_async() -> Result<ffhh::DeviceProxy> {
    debug!("connect_to_hrtimer: trying directory: {}", HRTIMER_DIRECTORY);
    let mut dir = std::fs::read_dir(HRTIMER_DIRECTORY)
        .map_err(|e| anyhow!("Failed to open hrtimer directory: {e}"))?;
    let entry = dir
        .next()
        .ok_or_else(|| anyhow!("No entry in the hrtimer directory"))?
        .map_err(|e| anyhow!("Failed to find hrtimer device: {e}"))?;
    let path = entry
        .path()
        .into_os_string()
        .into_string()
        .map_err(|e| anyhow!("Failed to parse the device entry path: {e:?}"))?;

    let (hrtimer, server_end) = fidl::endpoints::create_proxy::<ffhh::DeviceMarker>();
    fdio::service_connect(&path, server_end.into_channel())
        .map_err(|e| anyhow!("Failed to open hrtimer device: {e}"))?;

    Ok(hrtimer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use futures::{select, Future};
    use std::task::Poll;
    use test_case::test_case;
    use test_util::{assert_gt, assert_lt};

    // A test fixture function that sets up the fake wake alarms machinery.
    //
    // The user supplies a factory function with the async code to run.
    //
    // Args:
    //  - `run_for_duration`: the amount of fake time that the test should run for.
    //  - `test_fn_factory`: a normal function, which takes a WakeProxy, and returns
    //    an async closure that the test should run.
    fn run_in_fake_time_and_test_context<F, U, T>(
        run_for_duration: zx::MonotonicDuration,
        test_fn_factory: F,
    ) where
        F: FnOnce(fta::WakeProxy, finspect::Inspector) -> U, // F returns an async closure.
        U: Future<Output = T> + 'static, // the async closure may return an arbitrary type T.
        T: 'static,
    {
        let mut exec = fasync::TestExecutor::new_with_fake_time(); // We will be running this test case in fake time.
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));
        let (mut fake_commands_in, fake_commands_out) = mpsc::channel::<FakeCmd>(0);
        let (hrtimer_proxy, hrtimer_task) = fake_hrtimer_connection(fake_commands_out);
        let inspector = finspect::component::inspector();
        let alarms = Rc::new(Loop::new(hrtimer_proxy, inspector.root().create_child("test")));

        let (_handle, peer) = zx::EventPair::create();

        let done_set_properties = zx::Event::create();
        let begin_test = clone_handle(&done_set_properties);
        let begin_serve = clone_handle(&done_set_properties);

        let mut fake_commands_in_clone = fake_commands_in.clone();
        let config_task = async move {
            fake_commands_in
                .start_send(FakeCmd::SetProperties {
                    resolutions: vec![zx::Duration::from_nanos(43)],
                    max_ticks: 100,
                    keep_alive: peer,
                    done: clone_handle(&done_set_properties),
                })
                .unwrap();
        };

        let (wake_proxy, wake_stream) =
            fidl::endpoints::create_proxy_and_stream::<fta::WakeMarker>();

        let serving_task = async move {
            fasync::OnSignals::new(begin_serve, zx::Signals::EVENT_SIGNALED).await.unwrap();
            serve(alarms, wake_stream).await;
        };

        let seq_fn_fut = test_fn_factory(wake_proxy, inspector.clone());

        let test_task = async move {
            // Wait until configuration has completed.
            fasync::OnSignals::new(begin_test, zx::Signals::EVENT_SIGNALED).await.unwrap();

            let result = seq_fn_fut.await;

            // Request orderly shutdown.
            fake_commands_in_clone.start_send(FakeCmd::Exit).unwrap();
            result
        };

        let mut main_fut = fasync::Task::local(async {
            let _r = futures::join!(hrtimer_task, config_task, serving_task, test_task);
        });
        run_in_fake_time(&mut exec, &mut main_fut, run_for_duration);
    }

    // A loop that moves fake time forward in small increments, waking timers along the way.
    //
    // In almost all tests, we set up the environment for the test to run in, under a
    // test executor running in fake time. We then submit the resulting future
    // to this function for execution.
    //
    // This has been taken from //src/ui/lib/input_pipeline/src/autorepeater.rs
    // with some adaptation.
    fn run_in_fake_time<F>(
        executor: &mut fasync::TestExecutor,
        main_fut: &mut F,
        total_duration: zx::MonotonicDuration,
    ) where
        F: Future<Output = ()> + Unpin,
    {
        const INCREMENT: zx::MonotonicDuration = zx::MonotonicDuration::from_nanos(13);
        let mut current = zx::MonotonicDuration::ZERO;
        let mut poll_status = Poll::Pending;

        // We run until either the future completes or the timeout is reached,
        // whichever comes first.
        // Running the future after it returns Poll::Ready is not allowed, so
        // we must exit the loop then.
        while current < (total_duration + INCREMENT) && poll_status == Poll::Pending {
            let next = executor.now() + INCREMENT;
            executor.set_fake_time(next);
            executor.wake_expired_timers();
            poll_status = executor.run_until_stalled(main_fut);
            current = current + INCREMENT;
        }
        let now = executor.now();
        assert_eq!(
            poll_status,
            Poll::Ready(()),
            "the main future did not complete at {}, perhaps increase total_duration?",
            format_timer(now.into())
        );
    }

    // Human readable duration formatting is useful.
    #[test_case(0, "0ns (0)" ; "zero")]
    #[test_case(1000, "1μs (1000)" ; "1us positive")]
    #[test_case(-1000, "-1μs (-1000)"; "1us negative")]
    #[test_case(YEAR_IN_NANOS, "1year(s) (31536000000000000)"; "A year")]
    #[test_case(YEAR_IN_NANOS + 8 * DAY_IN_NANOS + 1,
        "1year(s)_1week(s)_1day(s)_1ns (32227200000000001)" ; "A weird duration")]
    #[test_case(2 * HOUR_IN_NANOS + 8 * MIN_IN_NANOS + 32 * SEC_IN_NANOS + 1,
        "2h_8min_32s_1ns (7712000000001)" ; "A reasonable long duration")]
    fn test_format_common(value: i64, repr: &str) {
        assert_eq!(format_common(value), repr.to_string());
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
        let config = TimerConfig::new_from_data(&resolutions[..], max_ticks);
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
        Exit,
    }

    use std::cell::RefCell;
    use std::rc::Rc;

    // A fake that emulates some aspects of the hrtimer driver.
    //
    // Specifically it can be configured with different resolutions, and will
    // bomb out if any waiting methods are called twice in a succession, without
    // canceling the timer in between.
    fn fake_hrtimer_connection(
        rcv: mpsc::Receiver<FakeCmd>,
    ) -> (ffhh::DeviceProxy, fasync::Task<()>) {
        debug!("fake_hrtimer_connection: entry.");
        let (hrtimer, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<ffhh::DeviceMarker>();
        let task = fasync::Task::local(async move {
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
                            Some(FakeCmd::Exit) => { break; }
                            Some(FakeCmd::SetProperties{ resolutions, max_ticks, keep_alive, done}) => {
                                let mut timer_props = vec![];
                                for _ in 0..10 {
                                    timer_props.push(ffhh::TimerProperties {
                                        supported_resolutions: Some(
                                            resolutions.iter()
                                                .map(|d| ffhh::Resolution::Duration(d.into_nanos())).collect()),
                                        max_ticks: Some(max_ticks.try_into().unwrap()),
                                        // start_and_wait method works.
                                        supports_wait: Some(true),
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
                                    fasync::Task::local(async move {
                                        // Respond after the requested sleep time. In tests this will
                                        // be sleeping in fake time.
                                        fasync::Timer::new(sleep_duration).await;
                                        *timer_running_clone.borrow_mut() = false;
                                        responder.send(Ok(clone_handle(wake_lease.borrow().as_ref().unwrap()))).unwrap();

                                        // Signaling the setup event allows the client to proceed
                                        // with post-scheduling work.
                                        signal(&setup_event);

                                    }).detach();
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
                                ffhh::DeviceRequest::_UnknownMethod { .. } => todo!(),
                            }
                        }
                    },
                }
            }
            debug!("fake_hrtimer_connection: exiting");
        });
        (hrtimer, task)
    }

    #[fuchsia::test]
    fn test_basic_timed_wait() {
        let deadline = zx::BootInstant::from_nanos(100);
        let test_duration = zx::MonotonicDuration::from_nanos(110);
        run_in_fake_time_and_test_context(test_duration, |wake_proxy, _| async move {
            let keep_alive = zx::Event::create();

            wake_proxy
                .set_and_wait(deadline.into(), keep_alive, "Hello".into())
                .await
                .unwrap()
                .unwrap();

            assert_gt!(fasync::BootInstant::now().into_nanos(), deadline.into_nanos());
        });
    }

    #[test_case(
        zx::BootInstant::from_nanos(100),
        zx::BootInstant::from_nanos(200),
        zx::MonotonicDuration::from_nanos(250) ;
        "Two timers: one at 100 and another at 200 ns"
    )]
    #[test_case(
        zx::BootInstant::from_nanos(100),
        zx::BootInstant::from_nanos(100),
        // A tight end-of-test will detect a stuck timer.
        zx::MonotonicDuration::from_nanos(104) ;
        "Two timers at the same deadline."
    )]
    #[test_case(
        zx::BootInstant::from_nanos(-1),
        zx::BootInstant::from_nanos(-1),
        zx::MonotonicDuration::from_nanos(30) ;
        "Two timers expire immediately."
    )]
    #[fuchsia::test]
    fn test_timed_wait_two_timers_params(
        // One timer scheduled at this instant (fake time starts from zero).
        first_deadline: zx::BootInstant,
        // Another timer scheduled at this instant.
        second_deadline: zx::BootInstant,
        // Run the fake time for this long.
        duration: zx::MonotonicDuration,
    ) {
        run_in_fake_time_and_test_context(duration, |wake_proxy, _| async move {
            let lease1 = zx::Event::create();
            let fut1 = wake_proxy.set_and_wait(first_deadline.into(), lease1, "Hello1".into());

            let lease2 = zx::Event::create();
            let fut2 = wake_proxy.set_and_wait(second_deadline.into(), lease2, "Hello2".into());

            let (result1, result2) = futures::join!(fut1, fut2);

            result1.unwrap().unwrap();
            result2.unwrap().unwrap();

            assert_gt!(fasync::BootInstant::now().into_nanos(), first_deadline.into_nanos());
            assert_gt!(fasync::BootInstant::now().into_nanos(), second_deadline.into_nanos());
        });
    }

    #[test_case(
        zx::BootInstant::from_nanos(100),
        zx::BootInstant::from_nanos(200),
        zx::MonotonicDuration::from_nanos(250) ;
        "Reschedule with push-out"
    )]
    #[test_case(
        zx::BootInstant::from_nanos(100),
        zx::BootInstant::from_nanos(100),
        // A tight end-of-test will detect a stuck timer.
        zx::MonotonicDuration::from_nanos(104) ;
        "Reschedule with same deadline"
    )]
    #[test_case(
        zx::BootInstant::from_nanos(200),
        zx::BootInstant::from_nanos(100),
        // A tight end-of-test will detect a stuck timer.
        zx::MonotonicDuration::from_nanos(240) ;
        "Pull in"
    )]
    #[fuchsia::test]
    fn test_timed_wait_same_timer(
        // One timer scheduled at this instant (fake time starts from zero).
        first_deadline: zx::BootInstant,
        // Another timer scheduled at this instant.
        second_deadline: zx::BootInstant,
        // Run the fake time for this long.
        duration: zx::MonotonicDuration,
    ) {
        run_in_fake_time_and_test_context(duration, |wake_proxy, _| async move {
            let lease1 = zx::Event::create();

            wake_proxy
                .set_and_wait(first_deadline.into(), lease1, "Hello".into())
                .await
                .unwrap()
                .unwrap();
            let lease2 = zx::Event::create();
            wake_proxy
                .set_and_wait(second_deadline.into(), lease2, "Hello2".into())
                .await
                .unwrap()
                .unwrap();
        });
    }

    // Test what happens when we schedule a timer, then change our mind and
    // reschedule the same timer, but with a sooner deadline.
    #[fuchsia::test]
    fn test_reschedule_pull_in() {
        const LONG_DEADLINE_NANOS: i64 = 200;
        const SHORT_DEADLINE_NANOS: i64 = 100;
        const ALARM_ID: &str = "Hello";
        run_in_fake_time_and_test_context(
            zx::MonotonicDuration::from_nanos(LONG_DEADLINE_NANOS + 50),
            |wake_proxy, _| async move {
                let wake_proxy = Rc::new(RefCell::new(wake_proxy));

                let keep_alive = zx::Event::create();

                let (mut sync_send, mut sync_recv) = mpsc::channel(1);

                // Schedule timer with a long timeout first. Let it wait, then
                // try to reschedule the same timer
                let wake_proxy_clone = wake_proxy.clone();
                let long_deadline_fut = async move {
                    let wake_fut = wake_proxy_clone.borrow().set_and_wait(
                        zx::BootInstant::from_nanos(LONG_DEADLINE_NANOS).into(),
                        keep_alive,
                        ALARM_ID.into(),
                    );
                    // Allow the rest of the test to proceed from here.
                    sync_send.send(()).await.unwrap();

                    // Yield-wait for the first scheduled timer.
                    wake_fut.await.unwrap().unwrap();
                };

                // Schedule the same timer as above, but with a shorter deadline.
                // The result should be that when the short deadline expires, it's
                // sooner than the long deadline.
                let short_deadline_fut = async move {
                    // Wait until we know that the long deadline timer has been scheduled.
                    let _ = sync_recv.next().await;

                    let keep_alive2 = zx::Event::create();
                    let _ = wake_proxy
                        .borrow()
                        .set_and_wait(
                            zx::BootInstant::from_nanos(SHORT_DEADLINE_NANOS).into(),
                            keep_alive2,
                            ALARM_ID.into(),
                        )
                        .await
                        .unwrap()
                        .unwrap();

                    // We get here presumably after the "short" deadline expires, verify that:
                    assert_gt!(fasync::BootInstant::now().into_nanos(), SHORT_DEADLINE_NANOS);
                    assert_lt!(fasync::BootInstant::now().into_nanos(), LONG_DEADLINE_NANOS);
                };
                futures::join!(short_deadline_fut, long_deadline_fut);
            },
        );
    }

    // Test what happens when we schedule a timer, then change our mind and
    // reschedule the same timer, but with a sooner deadline.
    #[fuchsia::test]
    fn test_reschedule_push_out() {
        const LONG_DEADLINE_NANOS: i64 = 200;
        const SHORT_DEADLINE_NANOS: i64 = 100;
        const ALARM_ID: &str = "Hello";
        run_in_fake_time_and_test_context(
            zx::MonotonicDuration::from_nanos(LONG_DEADLINE_NANOS + 50),
            |wake_proxy, inspector| async move {
                let wake_proxy = Rc::new(RefCell::new(wake_proxy));

                let keep_alive = zx::Event::create();

                let (mut sync_send, mut sync_recv) = mpsc::channel(1);

                // Schedule timer with a long timeout first. Let it wait, then
                // try to reschedule the same timer
                let wake_proxy_clone = wake_proxy.clone();
                let short_deadline_fut = async move {
                    let wake_fut = wake_proxy_clone.borrow().set_and_wait(
                        zx::BootInstant::from_nanos(SHORT_DEADLINE_NANOS).into(),
                        keep_alive,
                        ALARM_ID.into(),
                    );
                    // Allow the rest of the test to proceed from here.
                    sync_send.send(()).await.unwrap();

                    // Yield-wait for the first scheduled timer.
                    let result = wake_fut.await.unwrap();
                    assert_eq!(
                        result,
                        Err(fta::WakeError::Dropped),
                        "expected wake alarm to be dropped"
                    );
                    assert_gt!(fasync::BootInstant::now().into_nanos(), SHORT_DEADLINE_NANOS);
                };

                // Schedule the same timer as above, but with a shorter deadline.
                // The result should be that when the short deadline expires, it's
                // sooner than the long deadline.
                let long_deadline_fut = async move {
                    // Wait until we know that the other deadline timer has been scheduled.
                    let _ = sync_recv.next().await;

                    let keep_alive2 = zx::Event::create();
                    let _ = wake_proxy
                        .borrow()
                        .set_and_wait(
                            zx::BootInstant::from_nanos(LONG_DEADLINE_NANOS).into(),
                            keep_alive2,
                            ALARM_ID.into(),
                        )
                        .await
                        .unwrap()
                        .unwrap();

                    // Both the short and the long deadline expire.
                    assert_gt!(fasync::BootInstant::now().into_nanos(), LONG_DEADLINE_NANOS);
                };
                futures::join!(long_deadline_fut, short_deadline_fut);

                // The values in the inspector tree are fixed because the test
                // runs fully deterministically in fake time.
                assert_data_tree!(inspector, root: {
                    test: {
                        hardware: {
                            // All alarms fired, so this should be "none".
                            current_deadline: "(none)",
                            remaining_until_alarm: "(none)",
                        },
                        now_formatted: "247ns (247)",
                        now_ns: 247i64,
                        pending_timers: "\n\t",
                        pending_timers_count: 0u64,
                        requested_deadlines_ns: AnyProperty,
                    },
                });
            },
        );
    }
}
