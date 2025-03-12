// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::ready;
use std::cell::UnsafeCell;
use std::future::Future;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::task::{Context, Poll};

/// A lock-free thread-safe future.
// The debugger knows the layout so that async backtraces work, so if this changes the debugger
// might need to be changed too.
// LINT.IfChange
pub struct AtomicFuture<'a> {
    // A bitfield (holds the bits INACTIVE, READY or DONE).
    state: AtomicUsize,

    // `future` is safe to access after successfully clearing the INACTIVE bit and the `DONE` bit
    // isn't set.
    future: UnsafeCell<Box<dyn FutureOrResultAccess<'a>>>,
}
// LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)

trait FutureOrResultAccess<'a>: 'a {
    /// Drops the future.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future hasn't been dropped.
    // zxdb uses this method to figure out the concrete type of the future and it currently assumes
    // it is the first method in the trait.
    // LINT.IfChange
    unsafe fn drop_future(&mut self);
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)

    /// Polls the future.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future hasn't been dropped.
    unsafe fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()>;

    /// Gets the result.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future is finished and the result hasn't been taken or dropped.
    unsafe fn get_result(&self) -> *const ();

    /// Drops the result.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future is finished and the result hasn't already been taken or
    /// dropped.
    unsafe fn drop_result(&mut self);
}

union FutureOrResult<'a, F: 'a, R: 'a> {
    future: ManuallyDrop<F>,
    result: ManuallyDrop<R>,
    lifetime: PhantomData<&'a ()>,
}

impl<'a, F: Future<Output = R> + 'a, R: 'a> FutureOrResultAccess<'a> for FutureOrResult<'a, F, R> {
    unsafe fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let result = ready!(Pin::new_unchecked(&mut *self.future).poll(cx));
        // This might panic which will leave ourselves in a bad state.  We deal with this by
        // aborting (see below).
        ManuallyDrop::drop(&mut self.future);
        self.result = ManuallyDrop::new(result);
        Poll::Ready(())
    }

    unsafe fn drop_future(&mut self) {
        ManuallyDrop::drop(&mut self.future);
    }

    unsafe fn get_result(&self) -> *const () {
        &*self.result as *const R as *const ()
    }

    unsafe fn drop_result(&mut self) {
        ManuallyDrop::drop(&mut self.result);
    }
}

/// `AtomicFuture` is safe to access from multiple threads at once.
unsafe impl Sync for AtomicFuture<'_> {}
unsafe impl Send for AtomicFuture<'_> {}

/// State Bits

// Exclusive access is gained by clearing this bit.
const INACTIVE: usize = 1 << 0;

// Set to indicate the future needs to be polled again.
const READY: usize = 1 << 1;

// Terminal state: the future is dropped upon entry to this state.  When in this state, other bits
// can be set, including READY (which has no meaning).
const DONE: usize = 1 << 2;

// The task has been detached.
const DETACHED: usize = 1 << 3;

// The task has been cancelled.
const CANCELLED: usize = 1 << 4;

// The result has been taken.
const RESULT_TAKEN: usize = 1 << 5;

/// The result of a call to `try_poll`.
/// This indicates the result of attempting to `poll` the future.
#[derive(Debug)]
pub enum AttemptPollResult {
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
    /// The future was cancelled.
    Cancelled,
}

/// The result of calling the `cancel_and_detach` function.
#[must_use]
pub enum CancelAndDetachResult {
    /// The future has finished; it can be dropped.
    Done,

    /// The future needs to be added to a run queue to be cancelled.
    AddToRunQueue,

    /// The future is soon to be cancelled and nothing needs to be done.
    Pending,
}

impl<'a> AtomicFuture<'a> {
    /// Create a new `AtomicFuture`.
    pub fn new<F: Future<Output = R> + Send + 'a, R: Send + 'a>(future: F, detached: bool) -> Self {
        unsafe { Self::new_local(future, detached) }
    }

    /// Create a new `AtomicFuture` from a !Send future.
    ///
    /// # Safety
    ///
    /// The caller must uphold the Send requirements.
    pub unsafe fn new_local<F: Future<Output = R> + 'a, R: 'a>(future: F, detached: bool) -> Self {
        AtomicFuture {
            state: AtomicUsize::new(
                INACTIVE + {
                    if detached {
                        DETACHED
                    } else {
                        0
                    }
                },
            ),
            future: UnsafeCell::new(Box::new(FutureOrResult { future: ManuallyDrop::new(future) })),
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
                // We are now the (only) active worker, proceed to poll...
                if old & CANCELLED != 0 {
                    // The future was cancelled.
                    // SAFETY: We have exclusive access.
                    unsafe {
                        self.drop_future_unchecked();
                    }
                    return AttemptPollResult::Cancelled;
                }
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

        // We cannot recover from panics.
        struct Bomb;
        impl Drop for Bomb {
            fn drop(&mut self) {
                std::process::abort();
            }
        }

        let bomb = Bomb;

        // This `UnsafeCell` access is valid because `self.future.get()` is only called here, inside
        // the critical section where we performed the transition from INACTIVE to ACTIVE.
        let result = unsafe { (*self.future.get()).poll(cx) };

        std::mem::forget(bomb);

        if let Poll::Ready(()) = result {
            // The future will have been dropped, so we just need to set the state.
            //
            // This needs to be Release ordering because we need to synchronize with another thread
            // that takes the result.
            self.state.fetch_or(DONE, Release);
            // No one else will read `future` unless they see `INACTIVE`, which will never
            // happen again.
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
    #[must_use]
    pub fn mark_ready(&self) -> bool {
        self.state.fetch_or(READY, Relaxed) & (INACTIVE | READY | DONE) == INACTIVE
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
        assert!(self.state.fetch_or(DONE | RESULT_TAKEN, Relaxed) & DONE == 0);
        (*self.future.get()).drop_future();
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

    /// Cancels the task.  Returns true if the task needs to be added to a run queue.
    #[must_use]
    pub fn cancel(&self) -> bool {
        self.state.fetch_or(CANCELLED | READY, Relaxed) & (INACTIVE | READY | DONE) == INACTIVE
    }

    /// Marks the task as detached.
    pub fn detach(&self) {
        self.state.fetch_or(DETACHED, Relaxed);
    }

    /// Marks the task as cancelled and detached (for when the caller isn't interested in waiting
    /// for the cancellation to be finished).  Returns true if the task should be added to a run
    /// queue.
    pub fn cancel_and_detach(&self) -> CancelAndDetachResult {
        let old_state = self.state.fetch_or(CANCELLED | DETACHED | READY, Relaxed);
        if old_state & DONE != 0 {
            CancelAndDetachResult::Done
        } else if old_state & (INACTIVE | READY) == INACTIVE {
            CancelAndDetachResult::AddToRunQueue
        } else {
            CancelAndDetachResult::Pending
        }
    }

    /// Returns true if the task is detached.
    pub fn is_detached(&self) -> bool {
        self.state.load(Relaxed) & DETACHED != 0
    }

    /// Takes the result.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `R` is the correct type.
    pub unsafe fn take_result<R>(&self) -> Option<R> {
        // This needs to be Acquire ordering to synchronize with the polling thread.
        if self.state.load(Relaxed) & (DONE | RESULT_TAKEN) == DONE
            && self.state.fetch_or(RESULT_TAKEN, Acquire) & RESULT_TAKEN == 0
        {
            Some(((*self.future.get()).get_result() as *const R).read())
        } else {
            None
        }
    }
}

impl Drop for AtomicFuture<'_> {
    fn drop(&mut self) {
        let state = *self.state.get_mut();
        if state & DONE == 0 {
            // SAFETY: The state isn't DONE so we must drop the future.
            unsafe {
                (*self.future.get()).drop_future();
            }
        } else if state & RESULT_TAKEN == 0 {
            // SAFETY: The result hasn't been taken so we must drop the result.
            unsafe {
                (*self.future.get()).drop_result();
            }
        }
    }
}
