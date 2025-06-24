// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::{create_proxy_for_wake_events_counter_zero, OnWakeOps};
use crate::task::{CurrentTask, HandleWaitCanceler, TargetTime, WaitCanceler};
use crate::vfs::timer::TimerOps;

use anyhow::{Context, Result};
use fidl::endpoints::Proxy;
use fuchsia_inspect::ArrayProperty;
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::{FutureExt, SinkExt, StreamExt};
use scopeguard::defer;
use starnix_logging::{log_debug, log_error, log_warn};
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, from_status_like_fdio};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};
use zx::{self as zx, AsHandleRef, HandleBased, HandleRef};
use {fidl_fuchsia_time_alarms as fta, fuchsia_async as fasync};

/// Max value for inspect event history.
const INSPECT_GRAPH_EVENT_BUFFER_SIZE: usize = 128;

fn to_errno_with_log<T: std::fmt::Debug>(v: T) -> Errno {
    log_error!("hr_timer_manager internal error: {v:?}");
    from_status_like_fdio!(zx::Status::IO)
}

fn signal_handle<H: HandleBased>(
    handle: &H,
    clear_mask: zx::Signals,
    set_mask: zx::Signals,
) -> Result<(), zx::Status> {
    handle.signal_handle(clear_mask, set_mask).map_err(|err| {
        log_error!("while signaling handle: {err:?}: clear: {clear_mask:?}, set: {set_mask:?}");
        err
    })
}

fn duplicate_handle<H: HandleBased>(h: &H) -> Result<H, Errno> {
    h.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(|status| from_status_like_fdio!(status))
}

/// Waits forever synchronously for EVENT_SIGNALED.
fn wait_signaled_sync<H: HandleBased>(handle: &H) -> zx::WaitResult {
    handle.wait_handle(zx::Signals::EVENT_SIGNALED, zx::MonotonicInstant::INFINITE)
}

/// Waits forever asynchronously for EVENT_SIGNALED.
async fn wait_signaled<H: HandleBased>(handle: &H) -> Result<()> {
    fasync::OnSignals::new(handle, zx::Signals::EVENT_SIGNALED)
        .await
        .context("hr_timer_manager:wait_signaled")?;
    Ok(())
}

// Used to inject a fake proxy in tests.
fn get_wake_proxy_internal(mut wake_channel: Option<zx::Channel>) -> fta::WakeAlarmsProxy {
    wake_channel
        .take()
        .map(|c| fta::WakeAlarmsProxy::new(fidl::AsyncChannel::from_channel(c)))
        .unwrap_or_else(|| {
            connect_to_wake_alarms_async().expect("connection to wake alarms async proxy")
        })
}

/// Cancels an alarm by ID.
async fn cancel_by_id(
    _suspend_lock: &SuspendLock,
    timer_state: Option<TimerState>,
    timer_id: &zx::Koid,
    proxy: &fta::WakeAlarmsProxy,
    interval_timers_pending_reschedule: &mut HashMap<zx::Koid, SuspendLock>,
    alarm_id: &str,
) {
    if let Some(timer_state) = timer_state {
        fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:cancel_by_id", "timer_id" => *timer_id);
        log_debug!("cancel_by_id: START canceling timer: {:?}: alarm_id: {}", timer_id, alarm_id);
        proxy.cancel(&alarm_id).expect("infallible");
        log_debug!("cancel_by_id: 1/2 canceling timer: {:?}: alarm_id: {}", timer_id, alarm_id);

        // Let the timer closure complete before continuing.
        let _ = timer_state.task.await;

        // If this timer is an interval timer, we must remove it from the pending reschedule list.
        // This does not affect container suspend, since `_suspend_lock` is live. It's a no-op for
        // other timers.
        interval_timers_pending_reschedule.remove(timer_id);
        log_debug!("cancel_by_id: 2/2 DONE canceling timer: {timer_id:?}: alarm_id: {alarm_id}");
    }
}

/// Called when the underlying wake alarms manager reports a fta::WakeAlarmsError
/// as a result of a call to set_and_wait.
fn process_alarm_protocol_error(
    pending: &mut HashMap<zx::Koid, TimerState>,
    timer_id: &zx::Koid,
    error: fta::WakeAlarmsError,
) -> Option<TimerState> {
    match error {
        fta::WakeAlarmsError::Unspecified => {
            log_warn!(
                "watch_new_hrtimer_loop: Cmd::AlarmProtocolFail: unspecified error: {error:?}"
            );
            pending.remove(timer_id)
        }
        fta::WakeAlarmsError::Dropped => {
            log_debug!("watch_new_hrtimer_loop: Cmd::AlarmProtocolFail: alarm dropped: {error:?}");
            // Do not remove a Dropped timer here, in contrast to other error states: a Dropped
            // timer is a result of a Stop or a Cancel ahead of a reschedule. In both cases, that
            // code takes care of removing the timer from the pending timers list.
            None
        }
        error => {
            log_warn!(
                "watch_new_hrtimer_loop: Cmd::AlarmProtocolFail: unspecified error: {error:?}"
            );
            pending.remove(timer_id)
        }
    }
}

// This function is swapped out for an injected proxy in tests.
fn connect_to_wake_alarms_async() -> Result<fta::WakeAlarmsProxy, Errno> {
    log_debug!("connecting to wake alarms");
    fuchsia_component::client::connect_to_protocol::<fta::WakeAlarmsMarker>().map_err(|err| {
        errno!(EINVAL, format!("Failed to connect to fuchsia.time.alarms/Wake: {err}"))
    })
}

#[derive(Debug)]
enum InspectHrTimerEvent {
    Add,
    Update,
    Remove,
    // The String inside will be used in fmt. But the compiler does not recognize the use when
    // formatting with the Debug derivative.
    Error(#[allow(dead_code)] String),
}

impl InspectHrTimerEvent {
    fn retain_err(prev_len: usize, after_len: usize, context: &str) -> InspectHrTimerEvent {
        InspectHrTimerEvent::Error(format!(
            "retain the timer incorrectly, before len: {prev_len}, after len: {after_len}, context: {context}",
        ))
    }
}

impl std::fmt::Display for InspectHrTimerEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
struct TimerState {
    /// The task that waits for the timer to expire.
    task: fasync::Task<()>,
    /// The desired deadline for the timer.
    deadline: zx::BootInstant,
}

impl std::fmt::Display for TimerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TimerState[deadline:{:?}]", self.deadline)
    }
}

/// Prevents the Starnix container from going to sleep as long as it is in scope.
///
/// Each live SuspendLock is responsible for one increment of the underlying `zx::Counter` while it
/// in scope, and removes it from the counter when it goes out of scope.  Processes that need to
/// cooperate can pass a SuspendLock to each other to ensure that once the work is done, the lock
/// goes out of scope as well. This allows for precise accounting of remaining work, and should
/// give us control over container suspension which is guarded by the compiler, not conventions.
#[derive(Debug)]
struct SuspendLock {
    counter: Arc<zx::Counter>,
}

impl Drop for SuspendLock {
    fn drop(&mut self) {
        self.counter.add(-1).expect("decrement counter");
    }
}

impl SuspendLock {
    /// Creates a suspend lock on the provided counter. The counter will be incrementeed
    /// at creation, and decremented at disposition.
    fn new_with_increment(prototype: Arc<zx::Counter>) -> Self {
        prototype.add(1).expect("increment counter");
        Self::new_internal(prototype)
    }

    /// Creates a suspend lock on the provided counter, but does not increase the counter.
    fn new_without_increment(prototype: Arc<zx::Counter>) -> Self {
        // No increment - the counter was already incremented by the wake proxy
        Self::new_internal(prototype)
    }

    fn new_internal(prototype: Arc<zx::Counter>) -> Self {
        Self { counter: prototype }
    }
}

struct HrTimerManagerState {
    /// All pending timers are stored here.
    pending_timers: HashMap<zx::Koid, TimerState>,

    /// The event that is registered with runner to allow the hrtimer to wake the kernel.
    /// Optional, because we want the ability to inject a counter in tests.
    message_counter: Option<Arc<zx::Counter>>,

    /// For recording timer events.
    inspect_node: BoundedListNode,
}

impl HrTimerManagerState {
    fn new(parent_node: &fuchsia_inspect::Node) -> Self {
        Self {
            pending_timers: HashMap::new(),
            // Initialized later in the State's lifecycle because it only becomes
            // available after making a connection to the wake proxy.
            message_counter: None,
            inspect_node: BoundedListNode::new(
                parent_node.create_child("events"),
                INSPECT_GRAPH_EVENT_BUFFER_SIZE,
            ),
        }
    }

    fn get_pending_timers_count(&self) -> usize {
        self.pending_timers.len()
    }

    /// Gets a new shareable instance of the message counter.
    fn get_counter(&self) -> Arc<zx::Counter> {
        let counter_ref =
            self.message_counter.as_ref().expect("message_counter is None, but should not be.");
        counter_ref.clone()
    }
}

/// Asynchronous commands sent to `watch_new_hrtimer_loop`.
///
/// The synchronous methods on HrTimerManager use these commands to communicate
/// with the alarm manager actor that loops about in `watch_new_hrtimer_loop`.
///
/// This allows us to not have to share state between the synchronous and async
/// methods of `HrTimerManager`.
#[derive(Debug)]
enum Cmd {
    // Start the timer contained in `new_timer_node`.
    // The processing loop will signal `done` to allow synchronous
    // return from scheduling an async Cmd::Start.
    Start {
        new_timer_node: HrTimerNode,
        /// Signaled once the timer is started.
        done: zx::Event,
        /// The Starnix container suspend lock. Keep it alive until no more
        /// work is necessary.
        suspend_lock: SuspendLock,
    },
    /// Stop the timer noted below. `done` is similar to above.
    Stop {
        /// The timer to stop.
        timer: HrTimerHandle,
        /// Signaled once the timer is stopped.
        done: zx::Event,
        /// The Starnix container suspend lock. Keep it alive until no more
        /// work is necessary.
        suspend_lock: SuspendLock,
    },
    // A wake alarm occurred.
    Alarm {
        /// The affected timer's node.
        new_timer_node: HrTimerNode,
        /// The wake lease provided by the underlying API.
        lease: zx::EventPair,
        /// The Starnix container suspend lock. Keep it alive until no more
        /// work is necessary.
        suspend_lock: SuspendLock,
    },
}

/// The manager for high-resolution timers.
///
/// This manager is responsible for creating and managing high-resolution timers.
pub struct HrTimerManager {
    state: Mutex<HrTimerManagerState>,

    /// The channel sender that notifies the worker thread that HrTimer driver needs to be
    /// (re)started with a new deadline.
    start_next_sender: OnceLock<UnboundedSender<Cmd>>,
}
pub type HrTimerManagerHandle = Arc<HrTimerManager>;

impl HrTimerManager {
    pub fn new(parent_node: &fuchsia_inspect::Node) -> HrTimerManagerHandle {
        let inspect_node = parent_node.create_child("hr_timer_manager");
        let new_manager = Arc::new(Self {
            state: Mutex::new(HrTimerManagerState::new(&inspect_node)),
            start_next_sender: Default::default(),
        });
        let manager_weak = Arc::downgrade(&new_manager);

        // Create a lazy inspect node to get HrTimerManager info at read-time.
        inspect_node.record_lazy_child("hr_timer_manager", move || {
            let manager_ref = manager_weak.upgrade().expect("inner HrTimerManager");
            async move {
                // This gets the clock value directly from the kernel, it is not subject
                // to the local runner's clock.
                let now = zx::BootInstant::get();

                let inspector = fuchsia_inspect::Inspector::default();
                inspector.root().record_int("now_ns", now.into_nanos());

                let (timers, pending_timers_count, message_counter) = {
                    let guard = manager_ref.lock();
                    (
                        guard
                            .pending_timers
                            .iter()
                            .map(|(k, v)| (*k, v.deadline))
                            .collect::<Vec<_>>(),
                        guard.get_pending_timers_count(),
                        guard
                            .message_counter
                            .as_ref()
                            .map(|c| c.read().expect("message counter is readable"))
                            .unwrap_or(0),
                    )
                };
                inspector.root().record_uint("pending_timers_count", pending_timers_count as u64);
                inspector.root().record_int("message_counter", message_counter);

                // These are the deadlines we are currently waiting for. The format is:
                // `alarm koid` -> `deadline nanos` (remains: `duration until alarm nanos`)
                let deadlines = inspector.root().create_string_array("timers", timers.len());
                for (i, (k, v)) in timers.into_iter().enumerate() {
                    deadlines.set(
                        i,
                        format!(
                            "{:?} -> {:?} ns (remains: {:?} ns)",
                            k,
                            v.into_nanos(),
                            (v - now).into_nanos()
                        ),
                    );
                }
                inspector.root().record(deadlines);

                Ok(inspector)
            }
            .boxed()
        });
        parent_node.record(inspect_node);
        new_manager
    }

    /// Get a copy of a sender channel used for passing async command to the
    /// event processing loop.
    fn get_sender(&self) -> UnboundedSender<Cmd> {
        self.start_next_sender.get().expect("start_next_sender is initialized").clone()
    }

    /// Initialize the [HrTimerManager] in the context of the current system task.
    pub fn init(self: &HrTimerManagerHandle, system_task: &CurrentTask) -> Result<(), Errno> {
        self.init_for_test(
            system_task,
            /*wake_channel_for_test=*/ None,
            /*message_counter_for_test=*/ None,
        )
    }

    // Call this init for testing instead of the one above.
    fn init_for_test(
        self: &HrTimerManagerHandle,
        system_task: &CurrentTask,
        // Can be injected for testing.
        wake_channel_for_test: Option<zx::Channel>,
        // Can be injected for testing.
        message_counter_for_test: Option<zx::Counter>,
    ) -> Result<(), Errno> {
        let (start_next_sender, start_next_receiver) = mpsc::unbounded();
        self.start_next_sender.set(start_next_sender).map_err(|_| errno!(EEXIST))?;

        let self_ref = self.clone();

        // Ensure that all internal init has completed in `watch_new_hrtimer_loop`
        // before proceeding from here.
        let setup_done = zx::Event::create();
        let setup_done_clone = duplicate_handle(&setup_done)?;

        system_task.kernel().kthreads.spawn(move |_, system_task| {
            let mut executor = fasync::LocalExecutor::new();
            let _ = executor
                .run_singlethreaded(self_ref.watch_new_hrtimer_loop(
                    &system_task,
                    start_next_receiver,
                    wake_channel_for_test,
                    message_counter_for_test,
                    Some(setup_done_clone),
                ))
                .map_err(|err| log_error!("while running watch_new_hrtimer_loop: {err:?}"));
            log_warn!("hr_timer_manager: finished kernel thread. should never happen in prod code");
        });
        wait_signaled_sync(&setup_done)
            .to_result()
            .map_err(|status| from_status_like_fdio!(status))?;

        Ok(())
    }

    // Notifies `timer` and wake sources about a triggered alarm.
    fn notify_timer(
        self: &HrTimerManagerHandle,
        system_task: &CurrentTask,
        timer: &HrTimerNode,
        lease: impl HandleBased,
    ) -> Result<()> {
        let timer_id = timer.hr_timer.get_id();
        log_debug!("watch_new_hrtimer_loop: Cmd::Alarm: triggered alarm: {:?}", timer_id);
        fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:notify_timer", "timer_id" => timer_id);
        self.lock().pending_timers.remove(&timer_id).map(|s| s.task.detach());
        signal_handle(&timer.hr_timer.event(), zx::Signals::NONE, zx::Signals::TIMER_SIGNALED)
            .context("notify_timer: hrtimer signal handle")?;

        // Handle wake source here.
        let wake_source = timer.wake_source.clone();
        if let Some(wake_source) = wake_source.as_ref().and_then(|f| f.upgrade()) {
            let lease_token = lease.into_handle();
            wake_source.on_wake(system_task, &lease_token);
            // Drop the baton lease after wake leases in associated epfd
            // are activated.
            drop(lease_token);
        }
        fuchsia_trace::instant!(c"alarms", c"starnix:hrtimer:notify_timer:drop_lease", fuchsia_trace::Scope::Process, "timer_id" => timer_id);
        Ok(())
    }

    // If no counter has been injected for tests, set provided `counter` to serve as that
    // counter. Used to inject a fake counter in tests.
    fn inject_or_set_message_counter(
        self: &HrTimerManagerHandle,
        message_counter: Arc<zx::Counter>,
    ) {
        let mut guard = self.lock();
        if guard.message_counter.is_none() {
            guard.message_counter = Some(message_counter);
        }
    }

    fn record_inspect_on_stop(
        self: &HrTimerManagerHandle,
        guard: &mut MutexGuard<'_, HrTimerManagerState>,
        prev_len: usize,
    ) {
        let after_len = guard.get_pending_timers_count();
        let inspect_event_type = if after_len == prev_len {
            None
        } else if after_len == prev_len - 1 {
            Some(InspectHrTimerEvent::Remove)
        } else {
            Some(InspectHrTimerEvent::retain_err(prev_len, after_len, "removing timer"))
        };
        if let Some(inspect_event_type) = inspect_event_type {
            self.record_event(guard, inspect_event_type, None);
        }
    }

    fn record_inspect_on_start(
        self: &HrTimerManagerHandle,
        guard: &mut MutexGuard<'_, HrTimerManagerState>,
        timer_id: zx::Koid,
        task: fasync::Task<()>,
        deadline: zx::BootInstant,
        prev_len: usize,
    ) {
        guard
            .pending_timers
            .insert(timer_id, TimerState { task, deadline })
            .map(|timer_state| {
                // This should not happen, at this point we already canceled
                // any previous instances of the same wake alarm.
                log_debug!(
                    "watch_new_hrtimer_loop: removing timer task in Cmd::Start: {:?}",
                    timer_state
                );
                timer_state
            })
            .map(|v| v.task.detach());

        // Record the inspect event
        let after_len = guard.get_pending_timers_count();
        let inspect_event_type = if after_len == prev_len {
            InspectHrTimerEvent::Update
        } else if after_len == prev_len + 1 {
            InspectHrTimerEvent::Add
        } else {
            InspectHrTimerEvent::retain_err(prev_len, after_len, "adding timer")
        };
        self.record_event(guard, inspect_event_type, Some(deadline));
    }

    /// Timer handler loop.
    ///
    /// # Args:
    /// - `wake_channel_for_test`: a channel implementing `fuchsia.time.alarms/Wake`
    ///   injected by tests only.
    /// - `message_counter_for_test`: a zx::Counter injected only by tests, to
    ///   emulate the wake proxy message counter.
    /// - `setup_done`: signaled once the initial loop setup is complete. Allows
    ///   pausing any async callers until this loop is in a runnable state.
    async fn watch_new_hrtimer_loop(
        self: &HrTimerManagerHandle,
        system_task: &CurrentTask,
        mut start_next_receiver: UnboundedReceiver<Cmd>,
        wake_channel_for_test: Option<zx::Channel>,
        message_counter_for_test: Option<zx::Counter>,
        setup_done: Option<zx::Event>,
    ) -> Result<()> {
        defer! {
            log_warn!("watch_new_hrtimer_loop: exiting. This should only happen in tests.");
        }

        let wake_proxy = get_wake_proxy_internal(wake_channel_for_test);
        let wake_channel = wake_proxy
            .into_channel()
            .expect("Failed to convert wake alarms proxy to channel")
            .into();

        let (device_channel, message_counter) =
            if let Some(message_counter) = message_counter_for_test {
                // For tests only.
                (wake_channel, message_counter)
            } else {
                create_proxy_for_wake_events_counter_zero(wake_channel, "wake-alarms".to_string())
            };
        let message_counter = Arc::new(message_counter);
        self.inject_or_set_message_counter(message_counter.clone());
        setup_done
            .as_ref()
            .map(|e| signal_handle(e, zx::Signals::NONE, zx::Signals::EVENT_SIGNALED));

        let device_async_proxy =
            fta::WakeAlarmsProxy::new(fidl::AsyncChannel::from_channel(device_channel));

        // Contains suspend locks for interval (periodic) timers that expired, but have not been
        // rescheduled yet. This allows us to defer container suspend until all such timers have
        // been rescheduled.
        // TODO: b/418813184 - Remove in favor of Fuchsia-specific interval timer support
        // once it is available.
        let mut interval_timers_pending_reschedule: HashMap<zx::Koid, SuspendLock> = HashMap::new();

        while let Some(cmd) = start_next_receiver.next().await {
            fuchsia_trace::duration!(c"alarms", c"start_next_receiver:loop");

            log_debug!("watch_new_hrtimer_loop: got command: {cmd:?}");
            match cmd {
                // A new timer needs to be started.  The timer node for the timer
                // is provided, and `done` must be signaled once the setup is
                // complete.
                Cmd::Start { new_timer_node, done, suspend_lock } => {
                    defer! {
                        // Allow add_timer to proceed once command processing is done.
                        signal_handle(&done, zx::Signals::NONE, zx::Signals::EVENT_SIGNALED).map_err(|err| to_errno_with_log(err)).expect("event can be signaled");
                    }

                    let hr_timer = &new_timer_node.hr_timer;
                    let timer_id = hr_timer.get_id();
                    let wake_alarm_id = hr_timer.wake_alarm_id();
                    let trace_id = hr_timer.trace_id();
                    log_debug!(
                        "watch_new_hrtimer_loop: Cmd::Start: timer_id: {:?}, wake_alarm_id: {}",
                        timer_id,
                        wake_alarm_id
                    );
                    fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:start", "timer_id" => timer_id);
                    fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", trace_id);

                    let maybe_cancel = self.lock().pending_timers.remove(&timer_id);
                    cancel_by_id(
                        &suspend_lock,
                        maybe_cancel,
                        &timer_id,
                        &device_async_proxy,
                        &mut interval_timers_pending_reschedule,
                        &wake_alarm_id,
                    )
                    .await;

                    // Signaled when the timer completed setup.
                    let setup_event = zx::Event::create();
                    let deadline = new_timer_node.deadline;

                    // A hrtimer may enter Start while signaled, let's see if this improves on the
                    // situation, thought I'm sceptical.
                    signal_handle(
                        &hr_timer.event(),
                        zx::Signals::TIMER_SIGNALED,
                        zx::Signals::NONE,
                    )?;

                    // Make a request here. Move it into the closure after. Current FIDL semantics
                    // ensure that even though we do not `.await` on this future, a request to
                    // schedule a wake alarm based on this timer will be sent.
                    let request_fut = device_async_proxy.set_and_wait(
                        new_timer_node.deadline,
                        fta::SetAndWaitMode::NotifySetupDone(duplicate_handle(&setup_event)?),
                        &wake_alarm_id,
                    );
                    let mut done_sender = self.get_sender();
                    let prev_len = self.lock().get_pending_timers_count();

                    let counter_clone = message_counter.clone();
                    let self_clone = self.clone();
                    let task = fasync::Task::local(async move {
                        log_debug!(
                            "wake_alarm_future: set_and_wait will block here: {wake_alarm_id:?}"
                        );
                        fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:wait", "timer_id" => timer_id);
                        fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", trace_id);

                        let response = request_fut.await;
                        let suspend_lock = SuspendLock::new_without_increment(counter_clone);
                        fuchsia_trace::instant!(c"alarms", c"starnix:hrtimer:wake", fuchsia_trace::Scope::Process, "timer_id" => timer_id);

                        log_debug!("wake_alarm_future: set_and_wait over: {:?}", response);
                        match response {
                            // Alarm.  This must be processed in the main loop because notification
                            // requires access to &CurrentTask, which is not available here. So we
                            // only forward it.
                            Ok(Ok(lease)) => {
                                done_sender
                                    .send(Cmd::Alarm { new_timer_node, lease, suspend_lock })
                                    .await
                                    .expect("infallible");
                            }
                            Ok(Err(error)) => {
                                fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:wake_error", "timer_id" => timer_id);
                                log_debug!(
                                    "wake_alarm_future: protocol error: {error:?}: timer_id: {timer_id:?}"
                                );
                                let mut guard = self_clone.lock();
                                let pending = &mut guard.pending_timers;
                                process_alarm_protocol_error(pending, &timer_id, error);
                            }
                            Err(error) => {
                                fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:fidl_error", "timer_id" => timer_id);
                                log_debug!(
                                    "wake_alarm_future: FIDL error: {error:?}: timer_id: {timer_id:?}"
                                );
                                self_clone.lock().pending_timers.remove(&timer_id);
                            }
                        }
                        log_debug!("wake_alarm_future: closure done for timer_id: {timer_id:?}");
                    });
                    wait_signaled(&setup_event).await.map_err(|e| to_errno_with_log(e))?;
                    let mut guard = self.lock();
                    self.record_inspect_on_start(&mut guard, timer_id, task, deadline, prev_len);
                    log_debug!("Cmd::Start scheduled: timer_id: {:?}", timer_id);
                }
                Cmd::Alarm { new_timer_node, lease, suspend_lock } => {
                    let timer = &new_timer_node.hr_timer;
                    let timer_id = timer.get_id();
                    fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:alarm", "timer_id" => timer_id);
                    fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", timer.trace_id());
                    self.notify_timer(system_task, &new_timer_node, lease)
                        .map_err(|e| to_errno_with_log(e))?;

                    // Interval timers currently need special handling: we must not suspend the
                    // container until the interval timer in question gets re-scheduled. To
                    // ensure that we stay awake, we store the suspend lock for a while. This
                    // prevents container suspend.
                    //
                    // This map entry and its SuspendLock is removed in one of the following cases:
                    //
                    // (1) When the interval timer eventually gets rescheduled. We
                    // assume that for interval timers the reschedule will be imminent and that
                    // therefore not suspending until that re-schedule happens will not unreasonably
                    // extend the awake period.
                    //
                    // (2) When the timer is canceled.
                    if *timer.is_interval.lock() {
                        interval_timers_pending_reschedule.insert(timer_id, suspend_lock);
                    }
                    log_debug!("Cmd::Alarm done: timer_id: {timer_id:?}");
                }
                Cmd::Stop { timer, done, suspend_lock } => {
                    defer! {
                        signal_handle(&done, zx::Signals::NONE, zx::Signals::EVENT_SIGNALED).expect("can signal");
                    }
                    let timer_id = timer.get_id();
                    log_debug!("watch_new_hrtimer_loop: Cmd::Stop: timer_id: {:?}", timer_id);
                    fuchsia_trace::duration!(c"alarms", c"starnix:hrtimer:stop", "timer_id" => timer_id);
                    fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", timer.trace_id());

                    let (maybe_cancel, prev_len) = {
                        let mut guard = self.lock();
                        let prev_len = guard.get_pending_timers_count();
                        (guard.pending_timers.remove(&timer_id), prev_len)
                    };

                    cancel_by_id(
                        &suspend_lock,
                        maybe_cancel,
                        &timer_id,
                        &device_async_proxy,
                        &mut interval_timers_pending_reschedule,
                        &timer.wake_alarm_id(),
                    )
                    .await;

                    {
                        let mut guard = self.lock();
                        self.record_inspect_on_stop(&mut guard, prev_len);
                    }
                    log_debug!("Cmd::Stop done: {timer_id:?}");
                }
            }
            let guard = self.lock();

            log_debug!(
                "watch_new_hrtimer_loop: pending timers count: {}",
                guard.pending_timers.len()
            );
            log_debug!("watch_new_hrtimer_loop: pending timers:       {:?}", guard.pending_timers);
            log_debug!(
                "watch_new_hrtimer_loop: message counter:      {:?}",
                message_counter.read().expect("message counter is readable"),
            );
            log_debug!(
                "watch_new_hrtimer_loop: interval timers:      {:?}",
                interval_timers_pending_reschedule.len(),
            );
        } // while

        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, HrTimerManagerState> {
        self.state.lock()
    }

    fn record_event(
        self: &HrTimerManagerHandle,
        guard: &mut MutexGuard<'_, HrTimerManagerState>,
        event_type: InspectHrTimerEvent,
        deadline: Option<zx::BootInstant>,
    ) {
        guard.inspect_node.add_entry(move |node| {
            node.record_string("type", event_type.to_string());
            node.record_int("created_at", zx::BootInstant::get().into_nanos());
            if let Some(deadline) = deadline {
                node.record_int("deadline", deadline.into_nanos());
            }
        });
    }

    /// Add a new timer.
    ///
    /// A wake alarm is scheduled for the timer.
    pub fn add_timer(
        self: &HrTimerManagerHandle,
        wake_source: Option<Weak<dyn OnWakeOps>>,
        new_timer: &HrTimerHandle,
        deadline: zx::BootInstant,
    ) -> Result<(), Errno> {
        log_debug!("add_timer: entry: {new_timer:?}, deadline: {deadline:?}");
        fuchsia_trace::duration!(c"alarms", c"starnix:add_timer", "deadline" => deadline.into_nanos());
        fuchsia_trace::flow_step!(c"alarms", c"hrtimer_lifecycle", new_timer.trace_id());

        let counter = self.lock().get_counter();
        let suspend_lock_until_timer_scheduled = SuspendLock::new_with_increment(counter);

        let sender = self.get_sender();
        let new_timer_node = HrTimerNode::new(deadline, wake_source, new_timer.clone());
        let wake_alarm_scheduled = zx::Event::create();
        let wake_alarm_scheduled_clone = duplicate_handle(&wake_alarm_scheduled)?;
        let timer_id = new_timer.get_id();
        sender
            .unbounded_send(Cmd::Start {
                new_timer_node,
                suspend_lock: suspend_lock_until_timer_scheduled,
                done: wake_alarm_scheduled_clone,
            })
            .map_err(|_| errno!(EINVAL, "add_timer: could not send Cmd::Start"))?;

        // Block until the wake alarm for this timer is scheduled.
        wait_signaled_sync(&wake_alarm_scheduled)
            .map_err(|_| errno!(EINVAL, "add_timer: wait_signaled_sync failed"))?;

        log_debug!("add_timer: exit : timer_id: {timer_id:?}");
        Ok(())
    }

    /// Remove a timer.
    ///
    /// The timer is removed if scheduled, nothing is changed if it is not.
    pub fn remove_timer(self: &HrTimerManagerHandle, timer: &HrTimerHandle) -> Result<(), Errno> {
        log_debug!("remove_timer: entry:  {timer:?}");
        fuchsia_trace::duration!(c"alarms", c"starnix:remove_timer");
        let counter = self.lock().get_counter();
        let suspend_lock_until_removed = SuspendLock::new_with_increment(counter);

        let sender = self.get_sender();
        let done = zx::Event::create();
        let done_clone = duplicate_handle(&done)?;
        let timer_id = timer.get_id();
        sender
            .unbounded_send(Cmd::Stop {
                timer: timer.clone(),
                suspend_lock: suspend_lock_until_removed,
                done: done_clone,
            })
            .map_err(|_| errno!(EINVAL, "remove_timer: could not send Cmd::Stop"))?;

        // Block until the alarm for this timer is scheduled.
        wait_signaled_sync(&done)
            .map_err(|_| errno!(EINVAL, "add_timer: wait_signaled_sync failed"))?;
        log_debug!("remove_timer: exit:  {timer_id:?}");
        Ok(())
    }
}

#[derive(Debug)]
pub struct HrTimer {
    event: Arc<zx::Event>,

    /// True iff the timer is currently set to trigger at an interval.
    ///
    /// This is used to determine at which point the hrtimer event (not
    /// `HrTimer::event` but the one that is shared with the actual driver)
    /// should be cleared.
    ///
    /// If this is true, the timer manager will wait to clear the timer event
    /// until the next timer request has been sent to the driver. This prevents
    /// lost wake ups where the container happens to suspend between two instances
    /// of an interval timer triggering.
    pub is_interval: Mutex<bool>,
}
pub type HrTimerHandle = Arc<HrTimer>;

impl Drop for HrTimer {
    fn drop(&mut self) {
        let wake_alarm_id = self.wake_alarm_id();
        fuchsia_trace::duration!(c"alarms", c"hrtimer::drop", "timer_id" => self.get_id(), "wake_alarm_id" => &wake_alarm_id[..]);
        fuchsia_trace::flow_end!(c"alarms", c"hrtimer_lifecycle", self.trace_id());
    }
}

impl HrTimer {
    pub fn new() -> HrTimerHandle {
        let ret =
            Arc::new(Self { event: Arc::new(zx::Event::create()), is_interval: Mutex::new(false) });
        let wake_alarm_id = ret.wake_alarm_id();
        fuchsia_trace::duration!(c"alarms", c"hrtimer::new", "timer_id" => ret.get_id(), "wake_alarm_id" => &wake_alarm_id[..]);
        fuchsia_trace::flow_begin!(c"alarms", c"hrtimer_lifecycle", ret.trace_id(), "wake_alarm_id" => &wake_alarm_id[..]);
        ret
    }

    pub fn event(&self) -> zx::Event {
        self.event
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .expect("Duplicate hrtimer event handle")
    }

    /// Returns the unique identifier of this [HrTimer].
    ///
    /// All holders of the same [HrTimerHandle] will see the same value here.
    pub fn get_id(&self) -> zx::Koid {
        self.event.as_handle_ref().get_koid().expect("infallible")
    }

    /// Returns the unique alarm ID for this [HrTimer].
    ///
    /// The naming pattern is: `starnix:Koid(NNNNN):iB`, where `NNNNN` is a koid
    /// and B is `1` if the timer is an interval timer, or `0` otherwise.
    fn wake_alarm_id(&self) -> String {
        let i = if *self.is_interval.lock() { "i1" } else { "i0" };
        let koid = self.get_id();
        format!("starnix:{koid:?}:{i}")
    }

    fn trace_id(&self) -> fuchsia_trace::Id {
        self.get_id().raw_koid().into()
    }
}

impl TimerOps for HrTimerHandle {
    fn start(
        &self,
        current_task: &CurrentTask,
        source: Option<Weak<dyn OnWakeOps>>,
        deadline: TargetTime,
    ) -> Result<(), Errno> {
        // Before (re)starting the timer, ensure the signal is cleared.
        signal_handle(&*self.event, zx::Signals::TIMER_SIGNALED, zx::Signals::NONE)
            .map_err(|status| from_status_like_fdio!(status))?;
        current_task.kernel().hrtimer_manager.add_timer(
            source,
            self,
            deadline.estimate_boot().ok_or_else(|| errno!(EINVAL))?,
        )?;
        Ok(())
    }

    fn stop(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        // Clear the signal when removing the hrtimer.
        signal_handle(&*self.event, zx::Signals::TIMER_SIGNALED, zx::Signals::NONE)
            .map_err(|status| from_status_like_fdio!(status))?;
        Ok(current_task.kernel().hrtimer_manager.remove_timer(self)?)
    }

    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler {
        WaitCanceler::new_event(Arc::downgrade(&self.event), canceler)
    }

    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.event.as_handle_ref()
    }
}

/// Represents a node of `HrTimer`.
#[derive(Clone, Debug)]
struct HrTimerNode {
    /// The deadline of the associated `HrTimer`.
    deadline: zx::BootInstant,

    /// The source where initiated this `HrTimer`.
    ///
    /// When the timer expires, the system will be woken up if necessary. The `on_wake` callback
    /// will be triggered with a baton lease to prevent further suspend while Starnix handling the
    /// wake event.
    wake_source: Option<Weak<dyn OnWakeOps>>,

    /// The underlying HrTimer.
    hr_timer: HrTimerHandle,
}

impl HrTimerNode {
    fn new(
        deadline: zx::BootInstant,
        wake_source: Option<Weak<dyn OnWakeOps>>,
        hr_timer: HrTimerHandle,
    ) -> Self {
        Self { deadline, wake_source, hr_timer }
    }
}

impl PartialEq for HrTimerNode {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline
            && Arc::ptr_eq(&self.hr_timer, &other.hr_timer)
            && match (self.wake_source.as_ref(), other.wake_source.as_ref()) {
                (Some(this), Some(other)) => Weak::ptr_eq(this, other),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for HrTimerNode {}

impl Ord for HrTimerNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Sooner the deadline, higher the priority.
        match other.deadline.cmp(&self.deadline) {
            std::cmp::Ordering::Equal => {
                Arc::as_ptr(&other.hr_timer).cmp(&Arc::as_ptr(&self.hr_timer))
            }
            other => other,
        }
    }
}

impl PartialOrd for HrTimerNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::HrTimer;
    use crate::testing::{create_kernel_and_task, AutoReleasableTask};
    use starnix_logging::log_warn;
    use std::thread;
    use {fidl_fuchsia_time_alarms as fta, fuchsia_async as fasync};

    #[derive(Debug, Copy, Clone)]
    enum Response {
        Immediate,
        Delayed,
        Error,
    }

    impl HrTimerManagerState {
        fn new_for_test() -> Self {
            Self {
                inspect_node: BoundedListNode::new(
                    fuchsia_inspect::component::inspector().root().create_child("events"),
                    INSPECT_GRAPH_EVENT_BUFFER_SIZE,
                ),
                pending_timers: Default::default(),
                message_counter: None,
            }
        }
    }

    // Scheduling a hrtimer with this deadline will expire it.
    const MAGIC_EXPIRE_DEADLINE: i64 = 424242;

    // Makes sure that a dropped responder is properly responded to.
    struct ResponderCleanup {
        responder: Option<fta::WakeAlarmsSetAndWaitResponder>,
    }

    impl Drop for ResponderCleanup {
        fn drop(&mut self) {
            let responder = self.responder.take();
            log_debug!("dropping responder: {responder:?}");
            responder.map(|r| {
                r.send(Err(fta::WakeAlarmsError::Dropped))
                    .map_err(|err| log_error!("could not respond to a FIDL message: {err:?}"))
                    .expect("should be able to respond to a FIDL message")
            });
        }
    }

    // Serves a fake `fuchsia.time.alarms/Wake` API. The behavior is simplistic when compared to
    // the "real" implementation in that it never actually expires alarms on its own, and has
    // fixed behavior for each scheduled alarm, which is selected at the beginning of the test.
    //
    // Despite this, we can use it to check a number of correctness scenarios with unit tests.
    //
    // This allows us to remove the flakiness that may arise from the use of real time, and also
    // avoid the complications of fake time.  If you want an alarm that expires, schedule it with
    // a deadline of `MAGIC_EXPIRE_DEADLINE` above, and call this with `response_type ==
    // Response::Delayed`.
    async fn serve_fake_wake_alarms(
        message_counter: zx::Counter,
        response_type: Response,
        mut stream: fta::WakeAlarmsRequestStream,
        once: bool,
    ) {
        log_warn!("serve_fake_wake_alarms: serving loop entry. response_type={:?}", response_type);
        let mut responders: HashMap<String, ResponderCleanup> = HashMap::new();
        if once {
            return;
        }

        while let Some(maybe_request) = stream.next().await {
            match maybe_request {
                Ok(request) => {
                    log_debug!(
                        "serve_fake_wake_alarms: request: {:?}; response_type: {:?}",
                        request,
                        response_type
                    );
                    match request {
                        fta::WakeAlarmsRequest::SetAndWait {
                            mode,
                            responder,
                            alarm_id,
                            deadline,
                        } => {
                            log_debug!(
                                "serve_fake_wake_alarms: SetAndWait: alarm_id: {:?}: deadline: {:?}",
                                alarm_id,
                                deadline
                            );
                            defer! {
                                if let fta::SetAndWaitMode::NotifySetupDone(setup_done) = mode {
                                    // Caller blocks until this event is signaled.
                                    signal_handle(&setup_done, zx::Signals::NONE, zx::Signals::EVENT_SIGNALED).map_err(|e| to_errno_with_log(e)).unwrap();
                                }
                            };
                            match response_type {
                                Response::Delayed => {
                                    // Just don't respond, forever.
                                    log_debug!(
                                        "serve_fake_wake_alarms: SetAndWait: will not respond"
                                    );
                                    // A special value that causes alarm expiry.
                                    if deadline.into_nanos() == MAGIC_EXPIRE_DEADLINE {
                                        // If any responders are removed, then add one return
                                        // message for each.
                                        let r_count_before = responders.len();
                                        responders.retain(|k, _| *k != alarm_id);
                                        let r_count_after = responders.len();

                                        message_counter
                                            .add(
                                                (r_count_before - r_count_after)
                                                    .try_into()
                                                    .expect("should be convertible"),
                                            )
                                            .expect("add to message_counter");
                                        let (_, peer) = zx::EventPair::create();
                                        message_counter.add(1).expect("add 1 to message counter");
                                        responder.send(Ok(peer)).expect("send FIDL response");
                                    } else {
                                        let removed = responders.insert(
                                            alarm_id,
                                            ResponderCleanup { responder: Some(responder) },
                                        );
                                        // If a responder was removed, add a return message for it.
                                        if let Some(_) = removed {
                                            message_counter.add(1).unwrap();
                                        }
                                    }
                                }
                                Response::Immediate => {
                                    // Manufacture a token to return, not relevant for the test.
                                    let (_ignored, fake_lease) = zx::EventPair::create();
                                    message_counter.add(1).unwrap();
                                    responder.send(Ok(fake_lease)).expect("infallible");
                                    log_debug!(
                                        "serve_fake_wake_alarms: SetAndWait: test fake responded immediately"
                                    );
                                }
                                Response::Error => {
                                    message_counter.add(1).unwrap();
                                    responder
                                        .send(Err(fta::WakeAlarmsError::Unspecified))
                                        .expect("infallible");
                                    log_debug!(
                                        "serve_fake_wake_alarms: SetAndWait: Responded with error"
                                    );
                                }
                            }
                        }
                        fta::WakeAlarmsRequest::Cancel { alarm_id, .. } => {
                            let r_count_before = responders.len();
                            responders.retain(|k, _| *k != alarm_id);
                            let r_count_after = responders.len();
                            message_counter
                                .add((r_count_before - r_count_after).try_into().unwrap())
                                .unwrap();

                            log_debug!("serve_fake_wake_alarms: Cancel: {}", alarm_id);
                        }
                        fta::WakeAlarmsRequest::_UnknownMethod { .. } => unreachable!(),
                    }
                }
                Err(e) => {
                    // This may or may not be an error, depending on what you wanted
                    // to test.
                    log_warn!("alarms::serve: error in request: {:?}", e);
                }
            }
        }
        log_warn!("serve_fake_wake_alarms: exiting");
    }

    // Injected for testing.
    async fn connect_factory(
        message_counter: zx::Counter,
        response_type: Response,
    ) -> fta::WakeAlarmsProxy {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fta::WakeAlarmsMarker>();
        let channel = server_end.into_channel();

        // A separate thread is needed to allow independent execution of the server.
        let _detached = thread::spawn(move || {
            let mut executor = fasync::LocalExecutor::new();
            let server_end: fidl::endpoints::ServerEnd<fta::WakeAlarmsMarker> =
                fidl::endpoints::ServerEnd::new(channel);
            let _ = executor.run_singlethreaded(async move {
                serve_fake_wake_alarms(
                    message_counter,
                    response_type,
                    server_end.into_stream(),
                    /*once*/ false,
                )
                .await;
            });
        });
        proxy
    }

    // Initializes HrTimerManager for tests.
    //
    // # Returns
    //
    // A tuple of:
    // - `HrTimerManagerHandle` the unit under test
    // - `AutoReleasableTask` the kernel task to keep alive
    // - `zx::Counter` a message counter to use in tests to observe suspend state
    async fn init_hr_timer_manager(
        response_type: Response,
    ) -> (HrTimerManagerHandle, AutoReleasableTask, zx::Counter) {
        let (_, current_task) = create_kernel_and_task();
        let manager = Arc::new(HrTimerManager {
            state: Mutex::new(HrTimerManagerState::new_for_test()),
            start_next_sender: Default::default(),
        });
        let counter = zx::Counter::create();
        let counter_clone = duplicate_handle(&counter).unwrap();
        let proxy = connect_factory(counter_clone, response_type).await;
        let counter_clone = duplicate_handle(&counter).unwrap();
        manager
            .init_for_test(
                &current_task,
                Some(proxy.into_channel().unwrap().into_zx_channel()),
                Some(counter_clone),
            )
            .expect("infallible");
        (manager, current_task, counter)
    }

    #[fuchsia::test]
    async fn test_triggering() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Immediate).await;
        let timer1 = HrTimer::new();
        let timer2 = HrTimer::new();
        let timer3 = HrTimer::new();

        hrtimer_manager.add_timer(None, &timer1, zx::BootInstant::from_nanos(1)).unwrap();
        hrtimer_manager.add_timer(None, &timer2, zx::BootInstant::from_nanos(2)).unwrap();
        hrtimer_manager.add_timer(None, &timer3, zx::BootInstant::from_nanos(3)).unwrap();

        wait_signaled(&timer1.event()).await.unwrap();
        wait_signaled(&timer2.event()).await.unwrap();
        wait_signaled(&timer3.event()).await.unwrap();

        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_NON_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_NON_POSITIVE)
        );
    }

    #[fuchsia::test]
    async fn test_delayed_response() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Delayed).await;
        let timer = HrTimer::new();

        hrtimer_manager.add_timer(None, &timer, zx::BootInstant::from_nanos(1)).unwrap();

        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_NON_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_NON_POSITIVE)
        );
    }

    #[fuchsia::test]
    async fn test_protocol_error_response() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Error).await;
        let timer = HrTimer::new();
        hrtimer_manager.add_timer(None, &timer, zx::BootInstant::from_nanos(1)).unwrap();
        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_NON_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_NON_POSITIVE)
        );
    }

    #[fuchsia::test]
    async fn reschedule_same_timer() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Delayed).await;
        let timer = HrTimer::new();

        hrtimer_manager.add_timer(None, &timer, zx::BootInstant::from_nanos(1)).unwrap();
        hrtimer_manager.add_timer(None, &timer, zx::BootInstant::from_nanos(2)).unwrap();

        // Force alarm expiry.
        hrtimer_manager
            .add_timer(None, &timer, zx::BootInstant::from_nanos(MAGIC_EXPIRE_DEADLINE))
            .unwrap();
        wait_signaled(&timer.event()).await.unwrap();

        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_NON_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_NON_POSITIVE)
        );
    }

    #[fuchsia::test]
    async fn rescheduling_interval_timers_forbids_suspend() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Delayed).await;

        // Schedule an interval timer and let it expire.
        let timer1 = HrTimer::new();
        *timer1.is_interval.lock() = true;
        hrtimer_manager
            .add_timer(None, &timer1, zx::BootInstant::from_nanos(MAGIC_EXPIRE_DEADLINE))
            .unwrap();
        wait_signaled(&timer1.event()).await.unwrap();

        // Schedule a regular timer and let it expire.
        let timer2 = HrTimer::new();
        hrtimer_manager
            .add_timer(None, &timer2, zx::BootInstant::from_nanos(MAGIC_EXPIRE_DEADLINE))
            .unwrap();
        wait_signaled(&timer2.event()).await.unwrap();

        // When we have an expired but not rescheduled interval timer (`timer1`), and we have
        // an intervening timer that gets scheduled and expires (`timer2`) before `timer1` is
        // rescheduled, then suspend should be disallowed (counter > 0) to allow `timer1` to
        // be scheduled eventually.
        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_POSITIVE)
        );
    }

    #[fuchsia::test]
    async fn canceling_interval_timer_allows_suspend() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Delayed).await;

        let timer1 = HrTimer::new();
        *timer1.is_interval.lock() = true;
        hrtimer_manager
            .add_timer(None, &timer1, zx::BootInstant::from_nanos(MAGIC_EXPIRE_DEADLINE))
            .unwrap();
        wait_signaled(&timer1.event()).await.unwrap();

        // When an interval timer expires, we should not be allowed to suspend.
        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_POSITIVE)
        );

        // Schedule the same timer again. This time around we do not wait for it to expire,
        // but cancel the timer instead.
        const DURATION_100S: zx::BootDuration = zx::BootDuration::from_seconds(100);
        let deadline2 = fasync::BootInstant::after(DURATION_100S);
        hrtimer_manager.add_timer(None, &timer1, deadline2.into()).unwrap();

        hrtimer_manager.remove_timer(&timer1).unwrap();

        // When we cancel an interval timer, we should be allowed to suspend.
        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_NON_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_NON_POSITIVE),
        );
    }

    #[fuchsia::test]
    async fn canceling_interval_timer_allows_suspend_with_flake() {
        let (hrtimer_manager, _starnix_task, counter) =
            init_hr_timer_manager(Response::Delayed).await;

        let timer1 = HrTimer::new();
        *timer1.is_interval.lock() = true;
        hrtimer_manager
            .add_timer(None, &timer1, zx::BootInstant::from_nanos(MAGIC_EXPIRE_DEADLINE))
            .unwrap();
        wait_signaled(&timer1.event()).await.unwrap();

        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_POSITIVE)
        );
        const DURATION_100S: zx::BootDuration = zx::BootDuration::from_seconds(100);
        let deadline2 = fasync::BootInstant::after(DURATION_100S);
        hrtimer_manager.add_timer(None, &timer1, deadline2.into()).unwrap();
        // No pause between start and stop has led to flakes before.
        hrtimer_manager.remove_timer(&timer1).unwrap();

        assert_eq!(
            counter.wait_handle(zx::Signals::COUNTER_NON_POSITIVE, zx::MonotonicInstant::INFINITE),
            zx::WaitResult::Ok(zx::Signals::COUNTER_NON_POSITIVE),
        );
    }

    #[fuchsia::test]
    async fn suspend_locks_behaviors() {
        let counter = Arc::new(zx::Counter::create());

        assert_eq!(counter.read().unwrap(), 0, "no locks");
        let lock1 = SuspendLock::new_with_increment(counter.clone());
        assert_eq!(counter.read().unwrap(), 1, "lock1 is created");
        {
            let _lock2 = SuspendLock::new_with_increment(counter.clone());
            assert_eq!(counter.read().unwrap(), 2, "lock1 and _lock2 are live");
            let _lock3 = SuspendLock::new_with_increment(counter.clone());
            assert_eq!(counter.read().unwrap(), 3, "lock1, and _lock2 and _lock 3 are all live");
        }

        assert_eq!(counter.read().unwrap(), 1, "lock1 is still live");
        {
            let _lock4 = SuspendLock::new_without_increment(counter.clone());
            assert_eq!(counter.read().unwrap(), 1, "2 locks live, but only one increment");
            let _lock5 = SuspendLock::new_with_increment(counter.clone());
            assert_eq!(counter.read().unwrap(), 2, "3 locks, 2 with increment");
        }
        assert_eq!(counter.read().unwrap(), 0, "only lock1 is live");

        drop(lock1);
        assert_eq!(
            counter.read().unwrap(),
            -1,
            "no locks live, but 1 no-increment lock was available"
        );
    }
}
