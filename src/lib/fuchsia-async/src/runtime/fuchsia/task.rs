// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::scope::ScopeRef;
use crate::EHandle;
use futures::prelude::*;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A handle to a future that is owned and polled by the executor.
///
/// Once a task is created, the executor will poll it until done, even if the task handle itself is
/// not polled.
///
/// NOTE: When a JoinHandle is dropped, its future will be detached.
///
/// Polling (or attempting to extract the value from) a task after the executor is dropped may
/// trigger a panic.
#[derive(Debug)]
// LINT.IfChange
pub struct JoinHandle<T> {
    scope: ScopeRef,
    task_id: usize,
    phantom: PhantomData<T>,
}
// LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)

impl<T> Unpin for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    pub(crate) fn new(scope: ScopeRef, task_id: usize) -> Self {
        Self { scope, task_id, phantom: PhantomData }
    }

    /// Cancel a task and returns a future that resolves once the cancellation is complete.  The
    /// future can be ignored in which case the task will still be cancelled.
    pub fn cancel(mut self) -> impl Future<Output = Option<T>> {
        // SAFETY: We spawned the task so the return type should be correct.
        let result = unsafe { self.scope.cancel_task(self.task_id) };
        async move {
            match result {
                Some(output) => Some(output),
                None => {
                    // If we are dropped from here, we'll end up calling `cancel_and_detach`.
                    let result = std::future::poll_fn(|cx| {
                        // SAFETY: We spawned the task so the return type should be correct.
                        unsafe { self.scope.poll_cancelled(self.task_id, cx) }
                    })
                    .await;
                    self.task_id = 0;
                    result
                }
            }
        }
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if self.task_id != 0 {
            self.scope.detach(self.task_id);
        }
    }
}

impl<T: 'static> Future for JoinHandle<T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We spawned the task so the return type should be correct.
        let result = unsafe { self.scope.poll_join_result(self.task_id, cx) };
        if result.is_ready() {
            self.task_id = 0;
        }
        result
    }
}

/// This is the same as a JoinHandle, except that the future will be cancelled when the task is
/// dropped.
#[must_use]
#[repr(transparent)]
#[derive(Debug)]
// LINT.IfChange
pub struct Task<T>(JoinHandle<T>);
// LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)

impl<T> Task<T> {
    /// Returns a `JoinHandle` which will have detach-on-drop semantics.
    pub fn detach_on_drop(self) -> JoinHandle<T> {
        let this = ManuallyDrop::new(self);
        // SAFETY: We are bypassing our drop implementation.
        unsafe { std::ptr::read(&this.0) }
    }
}

impl Task<()> {
    /// Detach this task so that it can run independently in the background.
    ///
    /// *Note*: this is usually not what you want. This API severs the control flow from the
    /// caller, making it impossible to return values (including errors). If your goal is to run
    /// multiple futures concurrently, consider using [`TaskGroup`] or other futures combinators
    /// such as:
    ///
    /// * [`futures::future::join`]
    /// * [`futures::future::select`]
    /// * [`futures::select`]
    ///
    /// or their error-aware variants
    ///
    /// * [`futures::future::try_join`]
    /// * [`futures::future::try_select`]
    ///
    /// or their stream counterparts
    ///
    /// * [`futures::stream::StreamExt::for_each`]
    /// * [`futures::stream::StreamExt::for_each_concurrent`]
    /// * [`futures::stream::TryStreamExt::try_for_each`]
    /// * [`futures::stream::TryStreamExt::try_for_each_concurrent`]
    ///
    /// can meet your needs.
    pub fn detach(mut self) {
        self.0.scope.detach(self.0.task_id);
        self.0.task_id = 0;
    }
}

impl<T: Send + 'static> Task<T> {
    /// Spawn a new task on the root scope of the current executor.
    ///
    /// The task may be executed on any thread(s) owned by the current executor.
    /// See [`Task::local`] for an equivalent that ensures locality.
    ///
    /// The passed future will live until either (a) the future completes,
    /// (b) the returned [`Task`] is dropped while the executor is running, or
    /// (c) the executor is destroyed; whichever comes first.
    ///
    /// # Panics
    ///
    /// `spawn` may panic if not called in the context of an executor (e.g.
    /// within a call to `run` or `run_singlethreaded`).
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        let executor = EHandle::local();
        let scope = executor.root_scope();
        let task_id = executor.spawn(scope, future);
        Task(JoinHandle::new(scope.clone(), task_id))
    }
}

impl<T: 'static> Task<T> {
    /// Spawn a new task on the root scope of the thread local executor.
    ///
    /// The passed future will live until either (a) the future completes,
    /// (b) the returned [`Task`] is dropped while the executor is running, or
    /// (c) the executor is destroyed; whichever comes first.
    ///
    /// NOTE: This is not supported with a [`SendExecutor`] and will cause a
    /// runtime panic. Use [`Task::spawn`] instead.
    ///
    /// # Panics
    ///
    /// `local` may panic if not called in the context of a local executor (e.g.
    /// within a call to `run` or `run_singlethreaded`).
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T> {
        let executor = EHandle::local();
        let scope = executor.root_scope();
        let task_id = executor.spawn_local(scope, future);
        Task(JoinHandle::new(scope.clone(), task_id))
    }
}

impl<T: 'static> Task<T> {
    /// Cancel a task and returns a future that resolves once the cancellation is complete.  The
    /// future can be ignored in which case the task will still be cancelled.
    pub fn cancel(self) -> impl Future<Output = Option<T>> {
        self.detach_on_drop().cancel()
    }
}

impl<T: 'static> Future for Task<T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We spawned the task so the return type should be correct.
        let result = unsafe { self.0.scope.poll_join_result(self.0.task_id, cx) };
        if result.is_ready() {
            self.0.task_id = 0;
        }
        result
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if self.0.task_id != 0 {
            self.0.scope.cancel_and_detach(self.0.task_id);
            self.0.task_id = 0;
        }
    }
}

impl<T> From<JoinHandle<T>> for Task<T> {
    fn from(value: JoinHandle<T>) -> Self {
        Self(value)
    }
}

/// Offload a blocking function call onto a different thread.
///
/// This function can be called from an asynchronous function without blocking
/// it, returning a future that can be `.await`ed normally. The provided
/// function should contain at least one blocking operation, such as:
///
/// - A synchronous syscall that does not yet have an async counterpart.
/// - A compute operation which risks blocking the executor for an unacceptable
///   amount of time.
///
/// If neither of these conditions are satisfied, just call the function normally,
/// as synchronous functions themselves are allowed within an async context,
/// as long as they are not blocking.
///
/// If you have an async function that may block, refactor the function such that
/// the blocking operations are offloaded onto the function passed to [`unblock`].
///
/// NOTE:
///
/// - The input function should not interact with the executor. Attempting to do so
///   can cause runtime errors. This includes spawning, creating new executors,
///   passing futures between the input function and the calling context, and
///   in some cases constructing async-aware types (such as IO-, IPC- and timer objects).
/// - Synchronous functions cannot be cancelled and may keep running after
///   the returned future is dropped. As a result, resources held by the function
///   should be assumed to be held until the returned future completes.
/// - This function assumes panic=abort semantics, so if the input function panics,
///   the process aborts. Behavior for panic=unwind is not defined.
// TODO(https://fxbug.dev/42158447): Consider using a backing thread pool to alleviate the cost of
// spawning new threads if this proves to be a bottleneck.
pub fn unblock<T: 'static + Send>(
    f: impl 'static + Send + FnOnce() -> T,
) -> impl 'static + Send + Future<Output = T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.map(|r| r.unwrap())
}

#[cfg(test)]
mod tests {
    use super::super::executor::{LocalExecutor, SendExecutor};
    use super::*;
    use std::sync::{Arc, Mutex};

    /// This struct holds a thread-safe mutable boolean and
    /// sets its value to true when dropped.
    #[derive(Clone)]
    struct SetsBoolTrueOnDrop {
        value: Arc<Mutex<bool>>,
    }

    impl SetsBoolTrueOnDrop {
        fn new() -> (Self, Arc<Mutex<bool>>) {
            let value = Arc::new(Mutex::new(false));
            let sets_bool_true_on_drop = Self { value: value.clone() };
            (sets_bool_true_on_drop, value)
        }
    }

    impl Drop for SetsBoolTrueOnDrop {
        fn drop(&mut self) {
            let mut lock = self.value.lock().unwrap();
            *lock = true;
        }
    }

    #[test]
    #[should_panic]
    fn spawn_from_unblock_fails() {
        // no executor in the off-thread, so spawning fails
        SendExecutor::new(2).run(async move {
            unblock(|| {
                let _ = Task::spawn(async {});
            })
            .await;
        });
    }

    #[test]
    fn future_destroyed_before_await_returns() {
        LocalExecutor::new().run_singlethreaded(async {
            let (sets_bool_true_on_drop, value) = SetsBoolTrueOnDrop::new();

            // Move the switch into a different thread.
            // Once we return from this await, that switch should have been dropped.
            unblock(move || {
                let lock = sets_bool_true_on_drop.value.lock().unwrap();
                assert_eq!(*lock, false);
            })
            .await;

            // Switch moved into the future should have been dropped at this point.
            // The value of the boolean should now be true.
            let lock = value.lock().unwrap();
            assert_eq!(*lock, true);
        });
    }
}
