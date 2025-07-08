// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::atomic::Ordering;
use std::sync::Arc;

/// A blocking object that can either be notified normally or interrupted
///
/// To block using an `InterruptibleEvent`, first call `begin_wait`. At this point, the event is
/// in the "waiting" state, and future calls to `notify` or `interrupt` will terminate the wait.
///
/// After `begin_wait` returns, call `block_until` to block the current thread until one of the
/// following conditions occur:
///
///  1. The given deadline expires.
///  2. At least one of the `notify` or `interrupt` functions were called after `begin_wait`.
///
/// It's safe to call `notify` or `interrupt` at any time. However, calls to `begin_wait` and
/// `block_until` must alternate, starting with `begin_wait`.
///
/// `InterruptibleEvent` uses two-phase waiting so that clients can register for notification,
/// perform some related work, and then start blocking. This approach ensures that clients do not
/// miss notifications that arrive after they perform the related work but before they actually
/// start blocking.
#[derive(Debug)]
pub struct InterruptibleEvent {
    futex: zx::Futex,
}

/// The initial state.
///
///  * Transitions to `WAITING` after `begin_wait`.
const READY: i32 = 0;

/// The event is waiting for a notification or an interruption.
///
///  * Transitions to `NOTIFIED` after `notify`.
///  * Transitions to `INTERRUPTED` after `interrupt`.
///  * Transitions to `READY` if the deadline for `block_until` expires.
const WAITING: i32 = 1;

/// The event has been notified and will wake up.
///
///  * Transitions to `READY` after `block_until` processes the notification.
const NOTIFIED: i32 = 2;

/// The event has been interrupted and will wake up.
///
///  * Transitions to `READY` after `block_until` processes the interruption.
const INTERRUPTED: i32 = 3;

/// A guard object to enforce that clients call `begin_wait` before `block_until`.
#[must_use = "call block_until to advance the event state machine"]
pub struct EventWaitGuard<'a> {
    event: &'a Arc<InterruptibleEvent>,
}

impl<'a> EventWaitGuard<'a> {
    /// The underlying event associated with this guard.
    pub fn event(&self) -> &'a Arc<InterruptibleEvent> {
        self.event
    }

    /// Block the thread until either `deadline` expires, the event is notified, or the event is
    /// interrupted.
    pub fn block_until(
        self,
        new_owner: Option<&zx::Thread>,
        deadline: zx::MonotonicInstant,
    ) -> Result<(), WakeReason> {
        self.event.block_until(new_owner, deadline)
    }
}

/// A description of why a `block_until` returned without the event being notified.
#[derive(Debug, PartialEq, Eq)]
pub enum WakeReason {
    /// `block_until` returned because another thread interrupted the wait using `interrupt`.
    Interrupted,

    /// `block_until` returned because the given deadline expired.
    DeadlineExpired,
}

impl InterruptibleEvent {
    pub fn new() -> Arc<Self> {
        Arc::new(InterruptibleEvent { futex: zx::Futex::new(0) })
    }

    /// Called to initiate a wait.
    ///
    /// Calls to `notify` or `interrupt` after this function returns will cause the event to wake
    /// up. Calls to those functions prior to calling `begin_wait` will be ignored.
    ///
    /// Once called, this function cannot be called again until `block_until` returns. Otherwise,
    /// this function will panic.
    pub fn begin_wait<'a>(self: &'a Arc<Self>) -> EventWaitGuard<'a> {
        self.futex
            .compare_exchange(READY, WAITING, Ordering::Relaxed, Ordering::Relaxed)
            .expect("Tried to begin waiting on an event when not ready.");
        EventWaitGuard { event: self }
    }

    fn block_until(
        &self,
        new_owner: Option<&zx::Thread>,
        deadline: zx::MonotonicInstant,
    ) -> Result<(), WakeReason> {
        // We need to loop around the call to zx_futex_wake because we can receive spurious
        // wakeups.
        loop {
            match self.futex.wait(WAITING, new_owner, deadline) {
                // The deadline expired while we were sleeping.
                Err(zx::Status::TIMED_OUT) => {
                    self.futex.store(READY, Ordering::Relaxed);
                    return Err(WakeReason::DeadlineExpired);
                }
                // The value changed before we went to sleep.
                Err(zx::Status::BAD_STATE) => (),
                Err(e) => panic!("Unexpected error from zx_futex_wait: {e}"),
                Ok(()) => (),
            }

            let state = self.futex.load(Ordering::Acquire);

            match state {
                // If we're still in the `WAITING` state, then the wake ended spuriously and we
                // need to go back to sleep.
                WAITING => continue,
                NOTIFIED => {
                    // We use a store here rather than a compare_exchange because other threads are
                    // only allowed to write to this value in the `WAITING` state and we are in the
                    // `NOTIFIED` state.
                    self.futex.store(READY, Ordering::Relaxed);
                    return Ok(());
                }
                INTERRUPTED => {
                    // We use a store here rather than a compare_exchange because other threads are
                    // only allowed to write to this value in the `WAITING` state and we are in the
                    // `INTERRUPTED` state.
                    self.futex.store(READY, Ordering::Relaxed);
                    return Err(WakeReason::Interrupted);
                }
                _ => {
                    panic!("Unexpected event state: {state}");
                }
            }
        }
    }

    /// Wake up the event normally.
    ///
    /// If this function is called before `begin_wait`, this notification is ignored. Calling this
    /// function repeatedly has no effect. If both `notify` and `interrupt` are called, the state
    /// observed by `block_until` is a race.
    pub fn notify(&self) {
        self.wake(NOTIFIED);
    }

    /// Wake up the event because of an interruption.
    ///
    /// If this function is called before `begin_wait`, this notification is ignored. Calling this
    /// function repeatedly has no effect. If both `notify` and `interrupt` are called, the state
    /// observed by `block_until` is a race.
    pub fn interrupt(&self) {
        self.wake(INTERRUPTED);
    }

    fn wake(&self, state: i32) {
        // See <https://marabos.nl/atomics/hardware.html#failing-compare-exchange> for why we issue
        // this load before the `compare_exchange` below.
        let observed = self.futex.load(Ordering::Relaxed);
        if observed == WAITING
            && self
                .futex
                .compare_exchange(WAITING, state, Ordering::Release, Ordering::Relaxed)
                .is_ok()
        {
            self.futex.wake_all();
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use zx::AsHandleRef;

    #[test]
    fn test_wait_block_and_notify() {
        let event = InterruptibleEvent::new();

        let guard = event.begin_wait();

        let other_event = Arc::clone(&event);
        let thread = std::thread::spawn(move || {
            other_event.notify();
        });

        guard.block_until(None, zx::MonotonicInstant::INFINITE).expect("failed to be notified");
        thread.join().expect("failed to join thread");
    }

    #[test]
    fn test_wait_block_and_interrupt() {
        let event = InterruptibleEvent::new();

        let guard = event.begin_wait();

        let other_event = Arc::clone(&event);
        let thread = std::thread::spawn(move || {
            other_event.interrupt();
        });

        let result = guard.block_until(None, zx::MonotonicInstant::INFINITE);
        assert_eq!(result, Err(WakeReason::Interrupted));
        thread.join().expect("failed to join thread");
    }

    #[test]
    fn test_wait_block_and_timeout() {
        let event = InterruptibleEvent::new();

        let guard = event.begin_wait();
        let result = guard
            .block_until(None, zx::MonotonicInstant::after(zx::MonotonicDuration::from_millis(20)));
        assert_eq!(result, Err(WakeReason::DeadlineExpired));
    }

    #[test]
    fn futex_ownership_is_transferred() {
        use zx::HandleBased;

        let event = Arc::new(InterruptibleEvent::new());

        let (root_thread_handle, root_thread_koid) = fuchsia_runtime::with_thread_self(|thread| {
            (thread.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(), thread.get_koid().unwrap())
        });

        let event_for_blocked_thread = event.clone();

        let blocked_thread = std::thread::spawn(move || {
            let event = event_for_blocked_thread;
            let guard = event.begin_wait();
            guard.block_until(Some(&root_thread_handle), zx::MonotonicInstant::INFINITE).unwrap();
        });

        // Wait for the correct owner to appear. If for some reason futex PI breaks, it's likely
        // that this test will time out rather than panicking outright. It would be nice to have
        // a clear assertion here, but there's no existing API we can use to atomically wait for a
        // waiter on the futex *and* see what owner they set. If we find ourselves writing a lot of
        // tests like this we might consider setting up a fake vdso.
        while event.futex.get_owner() != Some(root_thread_koid) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        event.notify();
        blocked_thread.join().unwrap();
    }

    #[test]
    fn stale_pi_owner_is_noop() {
        use zx::HandleBased;

        let mut new_owner = None;
        std::thread::scope(|s| {
            s.spawn(|| {
                new_owner = Some(
                    fuchsia_runtime::with_thread_self(|thread| {
                        thread.duplicate_handle(zx::Rights::SAME_RIGHTS)
                    })
                    .unwrap(),
                );
            });
        });
        let new_owner = new_owner.unwrap();

        let event = InterruptibleEvent::new();
        let guard = event.begin_wait();
        let result = guard.block_until(
            Some(&new_owner),
            zx::MonotonicInstant::after(zx::MonotonicDuration::from_millis(20)),
        );
        assert_eq!(result, Err(WakeReason::DeadlineExpired));
    }
}
