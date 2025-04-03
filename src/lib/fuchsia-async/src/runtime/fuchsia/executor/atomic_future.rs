// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod spawnable_future;

use crate::ScopeHandle;
use futures::ready;
use std::borrow::Borrow;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// A lock-free thread-safe future.

// The debugger knows the layout so that async backtraces work, so if this changes the debugger
// might need to be changed too.
//
// This is `repr(C)` so that we can cast between `NonNull<Meta>` and `NonNull<AtomicFuture<F>>`.

// LINT.IfChange
#[repr(C)]
struct AtomicFuture<F: Future> {
    meta: Meta,

    // `future` is safe to access after successfully clearing the INACTIVE state bit and the `DONE`
    // state bit isn't set.
    future: FutureOrResult<F>,
}
// LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)

/// A lock-free thread-safe future.  The handles can be cloned.
pub struct AtomicFutureHandle<'a>(NonNull<Meta>, PhantomData<&'a ()>);

/// `AtomicFutureHandle` is safe to access from multiple threads at once.
unsafe impl Sync for AtomicFutureHandle<'_> {}
unsafe impl Send for AtomicFutureHandle<'_> {}

impl Drop for AtomicFutureHandle<'_> {
    fn drop(&mut self) {
        self.meta().release();
    }
}

impl Clone for AtomicFutureHandle<'_> {
    fn clone(&self) -> Self {
        self.meta().retain();
        Self(self.0, PhantomData)
    }
}

struct Meta {
    vtable: &'static VTable,

    // Holds the reference count and state bits (INACTIVE, READY, etc.).
    state: AtomicUsize,

    scope: Option<ScopeHandle>,
    id: usize,
}

impl Meta {
    // # Safety
    //
    // This mints a handle with the 'static lifetime, so this should only be called from
    // `AtomicFutureHandle<'static>`.
    unsafe fn wake(&self) {
        if self.state.fetch_or(READY, Relaxed) & (INACTIVE | READY | DONE) == INACTIVE {
            self.retain();
            self.scope().executor().task_is_ready(AtomicFutureHandle(self.into(), PhantomData));
        }
    }

    fn scope(&self) -> &ScopeHandle {
        self.scope.as_ref().unwrap()
    }

    fn retain(&self) {
        let old = self.state.fetch_add(1, Relaxed) & REF_COUNT_MASK;
        assert!(old != REF_COUNT_MASK);
    }

    fn release(&self) {
        // This can be Relaxed because there is a barrier in the drop function.
        let old = self.state.fetch_sub(1, Relaxed) & REF_COUNT_MASK;
        if old == 1 {
            // SAFETY: This is safe because we just released the last reference.
            unsafe {
                (self.vtable.drop)(self.into());
            }
        } else {
            // Check for underflow.
            assert!(old > 0);
        }
    }

    // # Safety
    //
    // The caller must know that the future has completed.
    unsafe fn drop_result(&self, ordering: Ordering) {
        // It's possible for this to race with another thread so we only drop the result if we are
        // successful in setting the RESULT_TAKEN bit.
        if self.state.fetch_or(RESULT_TAKEN, ordering) & RESULT_TAKEN == 0 {
            (self.vtable.drop_result)(self.into());
        }
    }
}

struct VTable {
    /// Drops the atomic future.
    ///
    /// # Safety
    ///
    /// The caller must ensure there are no other references i.e. the reference count should be
    /// zero.
    // zxdb uses this method to figure out the concrete type of the future.
    // LINT.IfChange
    drop: unsafe fn(NonNull<Meta>),
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
    /// Drops the future.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future hasn't been dropped.
    drop_future: unsafe fn(NonNull<Meta>),
    /// Polls the future.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future hasn't been dropped and has exclusive access.
    poll: unsafe fn(NonNull<Meta>, cx: &mut Context<'_>) -> Poll<()>,

    /// Gets the result.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future is finished and the result hasn't been taken or dropped.
    get_result: unsafe fn(NonNull<Meta>) -> *const (),

    /// Drops the result.
    ///
    /// # Safety
    ///
    /// The caller must ensure the future is finished and the result hasn't already been taken or
    /// dropped.
    drop_result: unsafe fn(NonNull<Meta>),
}

union FutureOrResult<F: Future> {
    future: ManuallyDrop<F>,
    result: ManuallyDrop<F::Output>,
}

impl<F: Future> AtomicFuture<F> {
    const VTABLE: VTable = VTable {
        drop: Self::drop,
        drop_future: Self::drop_future,
        poll: Self::poll,
        get_result: Self::get_result,
        drop_result: Self::drop_result,
    };

    unsafe fn drop(meta: NonNull<Meta>) {
        drop(Box::from_raw(meta.cast::<Self>().as_mut()));
    }

    unsafe fn poll(meta: NonNull<Meta>, cx: &mut Context<'_>) -> Poll<()> {
        let future = &mut meta.cast::<Self>().as_mut().future;
        let result = ready!(Pin::new_unchecked(&mut *future.future).poll(cx));
        // This might panic which will leave ourselves in a bad state.  We deal with this by
        // aborting (see below).
        ManuallyDrop::drop(&mut future.future);
        future.result = ManuallyDrop::new(result);
        Poll::Ready(())
    }

    unsafe fn drop_future(meta: NonNull<Meta>) {
        ManuallyDrop::drop(&mut meta.cast::<Self>().as_mut().future.future);
    }

    unsafe fn get_result(meta: NonNull<Meta>) -> *const () {
        &*meta.cast::<Self>().as_mut().future.result as *const F::Output as *const ()
    }

    unsafe fn drop_result(meta: NonNull<Meta>) {
        ManuallyDrop::drop(&mut meta.cast::<Self>().as_mut().future.result);
    }
}

/// State Bits

// Exclusive access is gained by clearing this bit.
const INACTIVE: usize = 1 << 63;

// Set to indicate the future needs to be polled again.
const READY: usize = 1 << 62;

// Terminal state: the future is dropped upon entry to this state.  When in this state, other bits
// can be set, including READY (which has no meaning).
const DONE: usize = 1 << 61;

// The task has been detached.
const DETACHED: usize = 1 << 60;

// The task has been cancelled.
const CANCELLED: usize = 1 << 59;

// The result has been taken.
const RESULT_TAKEN: usize = 1 << 58;

// The mask for the ref count.
const REF_COUNT_MASK: usize = RESULT_TAKEN - 1;

/// The result of a call to `try_poll`.
/// This indicates the result of attempting to `poll` the future.
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

impl<'a> AtomicFutureHandle<'a> {
    /// Create a new `AtomicFuture`.
    pub fn new<F: Future + Send + 'a>(scope: Option<ScopeHandle>, id: usize, future: F) -> Self
    where
        F::Output: Send + 'a,
    {
        // SAFETY: This is safe because the future and output are both Send.
        unsafe { Self::new_local(scope, id, future) }
    }

    /// Create a new `AtomicFuture` from a !Send future.
    ///
    /// # Safety
    ///
    /// The caller must uphold the Send requirements.
    pub unsafe fn new_local<F: Future + 'a>(
        scope: Option<ScopeHandle>,
        id: usize,
        future: F,
    ) -> Self
    where
        F::Output: 'a,
    {
        Self(
            NonNull::new_unchecked(Box::into_raw(Box::new(AtomicFuture {
                meta: Meta {
                    vtable: &AtomicFuture::<F>::VTABLE,
                    // The future is inactive and we start with a single reference.
                    state: AtomicUsize::new(1 | INACTIVE),
                    scope,
                    id,
                },
                future: FutureOrResult { future: ManuallyDrop::new(future) },
            })))
            .cast::<Meta>(),
            PhantomData,
        )
    }

    fn meta(&self) -> &Meta {
        // SAFETY: This is safe because we hold a reference count.
        unsafe { self.0.as_ref() }
    }

    /// Returns the future's ID.
    pub fn id(&self) -> usize {
        self.meta().id
    }

    /// Returns the associated scope.
    pub fn scope(&self) -> &ScopeHandle {
        &self.meta().scope()
    }

    /// Attempt to poll the underlying future.
    ///
    /// `try_poll` ensures that the future is polled at least once more
    /// unless it has already finished.
    pub fn try_poll(&self, cx: &mut Context<'_>) -> AttemptPollResult {
        let meta = self.meta();
        loop {
            // Attempt to acquire sole responsibility for polling the future (by clearing the
            // INACTIVE bit) and also clear the READY bit at the same time so that we track if it
            // becomes READY again whilst we are polling.
            let old = meta.state.fetch_and(!(INACTIVE | READY), Acquire);
            assert_ne!(old & REF_COUNT_MASK, 0);
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
            let old = meta.state.fetch_or(READY, Relaxed);
            // If the future is still active, or the future was already marked as ready, we can
            // just return and it will get polled again.
            if old & INACTIVE == 0 || old & READY != 0 {
                return AttemptPollResult::Pending;
            }
            // The worker finished, and we marked the future as ready, so we must try again because
            // the future won't be in a run queue.
        }

        // We cannot recover from panics.
        let bomb = Bomb;

        // SAFETY: We have exclusive access because we cleared the INACTIVE state bit.
        let result = unsafe { (meta.vtable.poll)(meta.into(), cx) };

        std::mem::forget(bomb);

        if let Poll::Ready(()) = result {
            // The future will have been dropped, so we just need to set the state.
            //
            // This needs to be Release ordering because we need to synchronize with another thread
            // that takes or drops the result.
            let old = meta.state.fetch_or(DONE, Release);

            if old & DETACHED != 0 {
                // If the future is detached, we should eagerly drop the result.  This can be Relaxed
                // ordering because the result was written by this thread.

                // SAFETY: The future has completed.
                unsafe {
                    meta.drop_result(Relaxed);
                }
            }
            // No one else will read `future` unless they see `INACTIVE`, which will never
            // happen again.
            AttemptPollResult::IFinished
        } else if meta.state.fetch_or(INACTIVE, Release) & READY == 0 {
            AttemptPollResult::Pending
        } else {
            // The future was marked ready whilst we were polling, so yield.
            AttemptPollResult::Yield
        }
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
        let meta = self.meta();
        assert!(meta.state.fetch_or(DONE | RESULT_TAKEN, Relaxed) & DONE == 0);
        (meta.vtable.drop_future)(meta.into());
    }

    /// Drops the future if it is not currently being polled. Returns success if the future was
    /// dropped or was already dropped.
    pub fn try_drop(&self) -> Result<(), ()> {
        let old = self.meta().state.fetch_and(!INACTIVE, Acquire);
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
        self.meta().state.fetch_or(CANCELLED | READY, Relaxed) & (INACTIVE | READY | DONE)
            == INACTIVE
    }

    /// Marks the task as detached.
    pub fn detach(&self) {
        let meta = self.meta();
        let old = meta.state.fetch_or(DETACHED, Relaxed);

        if old & (DONE | RESULT_TAKEN) == DONE {
            // If the future is done, we should eagerly drop the result.  This needs to be acquire
            // ordering because another thread might have written the result.

            // SAFETY: The future has completed.
            unsafe {
                meta.drop_result(Acquire);
            }
        }
    }

    /// Marks the task as cancelled and detached (for when the caller isn't interested in waiting
    /// for the cancellation to be finished).  Returns true if the task should be added to a run
    /// queue.
    pub fn cancel_and_detach(&self) -> CancelAndDetachResult {
        let meta = self.meta();
        let old_state = meta.state.fetch_or(CANCELLED | DETACHED | READY, Relaxed);
        if old_state & DONE != 0 {
            // If the future is done, we should eagerly drop the result.  This needs to be acquire
            // ordering because another thread might have written the result.

            // SAFETY: The future has completed.
            unsafe {
                meta.drop_result(Acquire);
            }

            CancelAndDetachResult::Done
        } else if old_state & (INACTIVE | READY) == INACTIVE {
            CancelAndDetachResult::AddToRunQueue
        } else {
            CancelAndDetachResult::Pending
        }
    }

    /// Returns true if the task is detached.
    pub fn is_detached(&self) -> bool {
        self.meta().state.load(Relaxed) & DETACHED != 0
    }

    /// Takes the result.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `R` is the correct type.
    pub unsafe fn take_result<R>(&self) -> Option<R> {
        // This needs to be Acquire ordering to synchronize with the polling thread.
        let meta = self.meta();
        if meta.state.load(Relaxed) & (DONE | RESULT_TAKEN) == DONE
            && meta.state.fetch_or(RESULT_TAKEN, Acquire) & RESULT_TAKEN == 0
        {
            Some(((meta.vtable.get_result)(meta.into()) as *const R).read())
        } else {
            None
        }
    }
}

impl AtomicFutureHandle<'static> {
    /// Returns a waker for the future.
    pub fn waker(&self) -> BorrowedWaker<'_> {
        static BORROWED_WAKER_VTABLE: RawWakerVTable =
            RawWakerVTable::new(waker_clone, waker_wake_by_ref, waker_wake_by_ref, waker_noop);
        static WAKER_VTABLE: RawWakerVTable =
            RawWakerVTable::new(waker_clone, waker_wake, waker_wake_by_ref, waker_drop);

        fn waker_clone(raw_meta: *const ()) -> RawWaker {
            // SAFETY: We did the reverse cast below.
            let meta = unsafe { &*(raw_meta as *const Meta) };
            meta.retain();
            RawWaker::new(raw_meta, &WAKER_VTABLE)
        }

        fn waker_wake(raw_meta: *const ()) {
            // SAFETY: We did the reverse cast below.
            let meta = unsafe { &*(raw_meta as *const Meta) };
            if meta.state.fetch_or(READY, Relaxed) & (INACTIVE | READY | DONE) == INACTIVE {
                // This consumes the reference count.
                meta.scope().executor().task_is_ready(AtomicFutureHandle(
                    // SAFETY: We know raw_meta is not null.
                    unsafe { NonNull::new_unchecked(raw_meta as *mut Meta) },
                    PhantomData,
                ));
            } else {
                meta.release();
            }
        }

        fn waker_wake_by_ref(meta: *const ()) {
            // SAFETY: We did the reverse cast below.
            let meta = unsafe { &*(meta as *const Meta) };
            // SAFETY: The lifetime on `AtomicFutureHandle` is 'static.
            unsafe {
                meta.wake();
            }
        }

        fn waker_noop(_meta: *const ()) {}

        fn waker_drop(meta: *const ()) {
            // SAFETY: We did the reverse cast below.
            let meta = unsafe { &*(meta as *const Meta) };
            meta.release();
        }

        BorrowedWaker(
            // SAFETY: We meet the contract for RawWaker/RawWakerVtable.
            unsafe {
                Waker::from_raw(RawWaker::new(self.0.as_ptr() as *const (), &BORROWED_WAKER_VTABLE))
            },
            PhantomData,
        )
    }

    /// Wakes the future.
    pub fn wake(&self) {
        // SAFETY: The lifetime on `AtomicFutureHandle` is 'static.
        unsafe {
            self.meta().wake();
        }
    }
}

impl<F: Future> Drop for AtomicFuture<F> {
    fn drop(&mut self) {
        let meta = &mut self.meta;
        // This needs to be acquire ordering so that we see writes that might have just happened
        // in another thread when the future was polled.
        let state = meta.state.load(Acquire);
        if state & DONE == 0 {
            // SAFETY: The state isn't DONE so we must drop the future.
            unsafe {
                (meta.vtable.drop_future)(meta.into());
            }
        } else if state & RESULT_TAKEN == 0 {
            // SAFETY: The result hasn't been taken so we must drop the result.
            unsafe {
                (meta.vtable.drop_result)(meta.into());
            }
        }
    }
}

pub struct BorrowedWaker<'a>(std::task::Waker, PhantomData<&'a ()>);

impl Deref for BorrowedWaker<'_> {
    type Target = Waker;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<usize> for AtomicFutureHandle<'static> {
    fn borrow(&self) -> &usize {
        &self.meta().id
    }
}

impl Hash for AtomicFutureHandle<'static> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.meta().id.hash(state);
    }
}

impl PartialEq for AtomicFutureHandle<'static> {
    fn eq(&self, other: &Self) -> bool {
        self.meta().id == other.meta().id
    }
}

impl Eq for AtomicFutureHandle<'static> {}

struct Bomb;
impl Drop for Bomb {
    fn drop(&mut self) {
        std::process::abort();
    }
}
