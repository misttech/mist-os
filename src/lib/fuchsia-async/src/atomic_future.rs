// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::future::FutureObj;
use futures::FutureExt;
use std::cell::UnsafeCell;
use std::mem::ManuallyDrop;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::task::Context;

/// A lock-free thread-safe future.
pub struct AtomicFuture {
    // A bitfield (holds the bits INACTIVE, READY or DONE).
    state: AtomicUsize,

    // `future` is safe to access after successfully clearing the INACTIVE bit and the `DONE` bit
    // isn't set.  The debugger knows the layout so that async backtraces work, so if this changes
    // the debugger might need to be changed too.
    // LINT.IfChange
    future: UnsafeCell<ManuallyDrop<FutureObj<'static, ()>>>,
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
}

/// `AtomicFuture` is safe to access from multiple threads at once.
unsafe impl Sync for AtomicFuture {}
#[allow(dead_code)]
trait AssertSend: Send {}
impl AssertSend for AtomicFuture {}

/// State Bits

// Exclusive access is gained by clearing this bit.
const INACTIVE: usize = 1 << 0;

// Set to indicate the future needs to be polled again.
const READY: usize = 1 << 1;

// Terminal state: the future is dropped upon entry to this state.  When in this state, the READY
// bit can be set, but it has no meaning.
const DONE: usize = 1 << 2;

/// The result of a call to `try_poll`.
/// This indicates the result of attempting to `poll` the future.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AttemptPollResult {
    /// The future was being polled by another thread, but it was notified
    /// to poll at least once more before yielding.
    Busy,
    /// The future was polled, but did not complete.
    Pending,
    /// The future was polled and finished by this thread.
    /// This result is normally used to trigger garbage-collection of the future.
    IFinished,
    /// The future was already completed by another thread.
    SomeoneElseFinished,
    /// The future was polled, did not complete, but it is woken whilst it is polled so it
    /// should be polled again.
    Yield,
}

impl AtomicFuture {
    /// Create a new `AtomicFuture`.
    pub fn new(future: FutureObj<'static, ()>) -> Self {
        AtomicFuture {
            state: AtomicUsize::new(INACTIVE),
            future: UnsafeCell::new(ManuallyDrop::new(future)),
        }
    }

    /// Attempt to poll the underlying future.
    ///
    /// `try_poll` ensures that the future is polled at least once more
    /// unless it has already finished.
    pub fn try_poll(&self, cx: &mut Context<'_>) -> AttemptPollResult {
        loop {
            // Attempt to acquire sole responsibility for polling the future (by clearing the
            // INACTIVE bit) and also clear the READY bit at the same time so that we track if it
            // becomes READY again whilst we are polling.
            let old = self.state.fetch_and(!(INACTIVE | READY), Acquire);
            if old & DONE != 0 {
                // Someone else completed this future already
                return AttemptPollResult::SomeoneElseFinished;
            }
            if old & INACTIVE != 0 {
                // We are now the (only) active worker. proceed to poll!
                break;
            }
            // Future was already active; this shouldn't really happen because we shouldn't be
            // polling it from multiple threads at the same time.  Still, we handle it by setting
            // the READY bit so that it gets polled again.  We do this regardless of whether we
            // cleared the READY bit above.
            let old = self.state.fetch_or(READY, Relaxed);
            // If the future is still active, or the future was already marked as ready, we can
            // just return and it will get polled again.
            if old & INACTIVE == 0 || old & READY != 0 {
                return AttemptPollResult::Pending;
            }
            // The worker finished, and we marked the future as ready, so we must try again because
            // the future won't be in a run queue.
        }

        // This `UnsafeCell` access is valid because `self.future.get()` is only called here,
        // inside the critical section where we performed the transition from INACTIVE to
        // ACTIVE.
        let future: &mut FutureObj<'static, ()> = unsafe { &mut *self.future.get() };

        if future.poll_unpin(cx).is_ready() {
            // No one else will read `future` unless they see `INACTIVE`, which will never
            // happen again.

            // SAFETY: We have exclusive access.
            unsafe {
                self.drop_future_unchecked();
            }

            AttemptPollResult::IFinished
        } else if self.state.fetch_or(INACTIVE, Release) & READY == 0 {
            AttemptPollResult::Pending
        } else {
            // The future was marked ready whilst we were polling, so yield.
            AttemptPollResult::Yield
        }
    }

    /// Marks the future as ready and returns true if it needs to be added to a run queue, i.e.
    /// it isn't already ready, active or done.
    pub fn mark_ready(&self) -> bool {
        self.state.fetch_or(READY, Relaxed) == INACTIVE
    }

    /// Drops the future without checking its current state.
    ///
    /// # Safety
    ///
    /// This doesn't check the current state, so this must only be called if it is known that there
    /// is no concurrent access.  This also does *not* include any memory barriers before dropping
    /// the future.
    pub unsafe fn drop_future_unchecked(&self) {
        // Set the state first in case we panic when we drop.
        self.state.store(DONE, Relaxed);
        ManuallyDrop::drop(&mut *self.future.get());
    }

    /// Drops the future if it is not currently being polled. Returns success if the future was
    /// dropped or was already dropped.
    pub fn try_drop(&self) -> Result<(), ()> {
        let old = self.state.fetch_and(!INACTIVE, Acquire);
        if old & DONE != 0 {
            Ok(())
        } else if old & INACTIVE != 0 {
            // SAFETY: We have exclusive access.
            unsafe {
                self.drop_future_unchecked();
            }
            Ok(())
        } else {
            Err(())
        }
    }
}

impl Drop for AtomicFuture {
    fn drop(&mut self) {
        if *self.state.get_mut() & DONE == 0 {
            // SAFETY: The state isn't DONE so we must drop.
            unsafe {
                ManuallyDrop::drop(self.future.get_mut());
            }
        }
    }
}
