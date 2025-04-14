// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_power_observability as fobs;
use fuchsia_inspect::{LazyNode as ILazyNode, Node as INode};
use fuchsia_inspect_contrib::nodes::BoundedListNode as IRingBuffer;
use fuchsia_sync::Mutex;
use futures::FutureExt;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

const SUSPEND_EVENT_BUFFER_SIZE: usize = 512;

static INSPECT_FIELD_EVENT_CAPACITY: &str = "event_capacity";
static INSPECT_FIELD_HISTORY_DURATION: &str = "history_duration_seconds";
static INSPECT_FIELD_HISTORY_DURATION_WHEN_FULL: &str = "at_capacity_history_duration_seconds";

/// An event logged by system-activity-governor.
pub enum SagEvent {
    /// Suspend is being attempted.
    SuspendAttempted,
    /// Suspend was entered and exited successfully and the system is resuming.
    SuspendResumed { suspend_duration: i64 },
    /// Suspend attempt was requested but is not allowed due to an unmet
    /// precondition, e.g. active wake leases, CPU power element is active.
    SuspendAttemptBlocked,
    /// Suspend attempt failed.
    SuspendFailed,
    /// A suspend blocker has been acquired, so suspend is blocked.
    SuspendBlockerAcquired,
    /// A suspend blocker has been dropped, so suspend is no longer blocked.
    SuspendBlockerDropped,
    /// A suspend lock has been acquired, so an uninterruptible suspend attempt
    /// is imminent.
    SuspendLockAcquired,
    /// A suspend lock has been dropped, so a suspend attempt has completed.
    SuspendLockDropped,
    /// A wake lease was created.
    WakeLeaseCreated { name: String },
    /// The underlying power broker lease for a wake lease failed to be satisfied.
    WakeLeaseSatisfactionFailed { name: String, error: String },
    /// The underlying power broker lease for a wake lease was satisfied.
    WakeLeaseSatisfied { name: String },
    /// A wake lease was dropped and is no longer active.
    WakeLeaseDropped { name: String },
    /// Suspend callback processing started.
    SuspendCallbackPhaseStarted,
    /// Suspend callback processing ended.
    SuspendCallbackPhaseEnded,
    /// Resume callback processing started.
    ResumeCallbackPhaseStarted,
    /// Resume callback processing ended.
    ResumeCallbackPhaseEnded,
}

/// A logger for SagEvent objects that inserts the event into a circular buffer
/// in inspect.
#[derive(Clone)]
pub struct SagEventLogger {
    event_log: Rc<RefCell<IRingBuffer>>,
    /// Vector of timestamps that mirror `event_log` contents.
    /// Used to compute history durations in `_event_log_stats`.
    /// Note: Inspect's lazy node requires Arc+Mutex.
    event_log_times: Arc<Mutex<VecDeque<i64>>>,
    /// Inspect node that tracks wall-time history duration.
    /// Schema follows Power Broker's topology stats:
    ///   event_capacity: u64
    ///   history_duration_seconds: i64
    ///   at_capacity_history_duration_seconds: i64
    _event_log_stats: Rc<RefCell<ILazyNode>>,
}

impl SagEventLogger {
    pub fn new(node: &INode) -> Self {
        let event_log = Rc::new(RefCell::new(IRingBuffer::new(
            node.create_child(fobs::SUSPEND_EVENTS_NODE),
            SUSPEND_EVENT_BUFFER_SIZE,
        )));
        let event_log_times =
            Arc::new(Mutex::new(VecDeque::with_capacity(SUSPEND_EVENT_BUFFER_SIZE)));
        let weak_arc_of_times = Arc::downgrade(&event_log_times);
        let event_log_stats = node.create_lazy_child("suspend_events_stats", move || {
            let weak_times = weak_arc_of_times.clone();
            async move {
                let inspector = fuchsia_inspect::Inspector::default();
                let root = inspector.root();
                root.record_uint(INSPECT_FIELD_EVENT_CAPACITY, SUSPEND_EVENT_BUFFER_SIZE as u64);
                if let Some(event_log_times) = weak_times.upgrade() {
                    let timestamps = event_log_times.lock();
                    if !timestamps.is_empty() {
                        let head_ns = timestamps.front().unwrap();
                        let tail_ns = timestamps.back().unwrap();
                        let duration =
                            zx::BootDuration::from_nanos(tail_ns - head_ns).into_seconds();
                        root.record_int(INSPECT_FIELD_HISTORY_DURATION, duration);
                        if timestamps.len() == SUSPEND_EVENT_BUFFER_SIZE {
                            root.record_int(INSPECT_FIELD_HISTORY_DURATION_WHEN_FULL, duration);
                        }
                    } else {
                        root.record_int(INSPECT_FIELD_HISTORY_DURATION, 0i64);
                    }
                }
                Ok(inspector)
            }
            .boxed()
        });
        Self {
            event_log,
            event_log_times,
            _event_log_stats: Rc::new(RefCell::new(event_log_stats)),
        }
    }

    pub fn log(&self, event: SagEvent) {
        let time = zx::BootInstant::get().into_nanos();

        self.event_log.borrow_mut().add_entry(|node| {
            match event {
                SagEvent::SuspendAttempted => {
                    node.record_int(fobs::SUSPEND_ATTEMPTED_AT, time);
                }
                SagEvent::SuspendResumed { suspend_duration } => {
                    node.record_int(fobs::SUSPEND_RESUMED_AT, time);
                    node.record_int(fobs::SUSPEND_LAST_TIMESTAMP, suspend_duration);
                }
                SagEvent::SuspendFailed => {
                    node.record_int(fobs::SUSPEND_FAILED_AT, time);
                }
                SagEvent::SuspendAttemptBlocked => {
                    node.record_int(fobs::SUSPEND_ATTEMPT_BLOCKED_AT, time);
                }
                SagEvent::SuspendBlockerAcquired => {
                    node.record_int(fobs::SUSPEND_BLOCKER_ACQUIRED_AT, time);
                }
                SagEvent::SuspendBlockerDropped => {
                    node.record_int(fobs::SUSPEND_BLOCKER_DROPPED_AT, time);
                }
                SagEvent::SuspendLockAcquired => {
                    node.record_int(fobs::SUSPEND_LOCK_ACQUIRED_AT, time);
                }
                SagEvent::SuspendLockDropped => {
                    node.record_int(fobs::SUSPEND_LOCK_DROPPED_AT, time);
                }
                SagEvent::WakeLeaseCreated { name } => {
                    node.record_int(fobs::WAKE_LEASE_CREATED_AT, time);
                    node.record_string(fobs::WAKE_LEASE_ITEM_NAME, name);
                }
                SagEvent::WakeLeaseSatisfactionFailed { name, error } => {
                    node.record_int(fobs::WAKE_LEASE_SATISFACTION_FAILED_AT, time);
                    node.record_string(fobs::WAKE_LEASE_ITEM_NAME, name);
                    node.record_string(fobs::WAKE_LEASE_ITEM_ERROR, error);
                }
                SagEvent::WakeLeaseSatisfied { name } => {
                    node.record_int(fobs::WAKE_LEASE_SATISFIED_AT, time);
                    node.record_string(fobs::WAKE_LEASE_ITEM_NAME, name);
                }
                SagEvent::WakeLeaseDropped { name } => {
                    node.record_int(fobs::WAKE_LEASE_DROPPED_AT, time);
                    node.record_string(fobs::WAKE_LEASE_ITEM_NAME, name);
                }
                SagEvent::SuspendCallbackPhaseStarted => {
                    node.record_int(fobs::SUSPEND_CALLBACK_PHASE_START_AT, time);
                }
                SagEvent::SuspendCallbackPhaseEnded => {
                    node.record_int(fobs::SUSPEND_CALLBACK_PHASE_END_AT, time);
                }
                SagEvent::ResumeCallbackPhaseStarted => {
                    node.record_int(fobs::RESUME_CALLBACK_PHASE_START_AT, time);
                }
                SagEvent::ResumeCallbackPhaseEnded => {
                    node.record_int(fobs::RESUME_CALLBACK_PHASE_END_AT, time);
                }
            };
        });
        // Record timestamps that are tracked within IRingBuffer.
        {
            let mut times = self.event_log_times.lock();
            if times.len() == SUSPEND_EVENT_BUFFER_SIZE {
                times.pop_front();
            }
            times.push_back(time);
        }
    }
}
