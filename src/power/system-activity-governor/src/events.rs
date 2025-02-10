// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_power_observability as fobs;
use fuchsia_inspect::Node as INode;
use fuchsia_inspect_contrib::nodes::BoundedListNode as IRingBuffer;
use std::cell::RefCell;
use std::rc::Rc;

const SUSPEND_EVENTS_RING_BUFFER_SIZE: usize = 512;

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
}

/// A logger for SagEvent objects that inserts the event into a circular buffer
/// in inspect.
#[derive(Clone)]
pub struct SagEventLogger {
    event_log: Rc<RefCell<IRingBuffer>>,
}

impl SagEventLogger {
    pub fn new(node: INode) -> Self {
        let event_log = IRingBuffer::new(node, SUSPEND_EVENTS_RING_BUFFER_SIZE);
        Self { event_log: Rc::new(RefCell::new(event_log)) }
    }

    pub fn log(&self, event: SagEvent) {
        let time = zx::MonotonicInstant::get().into_nanos();

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
            };
        });
    }
}
