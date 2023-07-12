// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Values of this type represent "execution scopes" used by the library to give fine grained
//! control of the lifetimes of the tasks associated with particular connections.  When a new
//! connection is attached to a pseudo directory tree, an execution scope is provided.  This scope
//! is then used to start any tasks related to this connection.  All connections opened as a result
//! of operations on this first connection will also use the same scope, as well as any tasks
//! related to those connections.
//!
//! This way, it is possible to control the lifetime of a group of connections.  All connections
//! and their tasks can be shutdown by calling `shutdown` method on the scope that is hosting them.
//! Scope will also shutdown all the tasks when it goes out of scope.
//!
//! Implementation wise, execution scope is just a proxy, that forwards all the tasks to an actual
//! executor, provided as an instance of a [`futures::task::Spawn`] trait.

use crate::{
    directory::mutable::entry_constructor::EntryConstructor, token_registry::TokenRegistry,
};

use {
    futures::{
        channel::oneshot,
        task::{self, Context, Poll, Waker},
        Future,
    },
    pin_project::pin_project,
    slab::Slab,
    std::{
        ops::Drop,
        pin::Pin,
        sync::{Arc, Mutex},
    },
};

pub type SpawnError = task::SpawnError;

/// An execution scope that is hosting tasks for a group of connections.  See the module level
/// documentation for details.
///
/// Actual execution will be delegated to an "upstream" executor - something that implements
/// [`futures::task::Spawn`].  In a sense, this is somewhat of an analog of a multithreaded capable
/// [`futures::stream::FuturesUnordered`], but this some additional functionality specific to the
/// vfs library.
///
/// Use [`ExecutionScope::new()`] or [`ExecutionScope::build()`] to construct new
/// `ExecutionScope`es.
pub struct ExecutionScope {
    executor: Arc<Executor>,

    entry_constructor: Option<Arc<dyn EntryConstructor + Send + Sync>>,
}

struct Executor {
    inner: Mutex<Inner>,
    token_registry: TokenRegistry,
}

struct Inner {
    /// Registered tasks that are to be shut down when shutting down the executor.
    registered: Slab<Waker>,

    /// Waiters waiting for all tasks to be terminated.
    waiters: std::vec::Vec<oneshot::Sender<()>>,

    /// Records if shutdown has been called on the executor.
    is_shutdown: bool,

    /// The number of active tasks preventing shutdown.
    active_count: usize,
}

impl Inner {
    // If there are no active tasks, and we are terminating, the tasks are woken and they should
    // terminate. If there are no active tasks and no registered tasks, wake any tasks that might
    // be waiting for that.
    fn check_active_count(&mut self) {
        if self.active_count == 0 {
            // If shutting down, wake all registered tasks which will cause them to terminate.
            if self.is_shutdown {
                for (_, waker) in &self.registered {
                    waker.wake_by_ref();
                }
            }
            if self.registered.is_empty() {
                for waiter in self.waiters.drain(..) {
                    let _ = waiter.send(());
                }
            }
        }
    }

    fn task_did_finish(&mut self, task_id: usize) {
        // If the task was never registered, then we must balance the increment to `active_count` in
        // `spawn`.
        if task_id == usize::MAX {
            self.active_count -= 1;
        } else {
            self.registered.remove(task_id);
        }
        self.check_active_count();
    }

    // Registers a task. If task_id == usize, it means this is the first time the task has been
    // registered. Returns true if the executor has been shut down and this task should terminate
    // (which is the case if the active count is zero).
    fn register_task(&mut self, task_id: &mut usize, waker: std::task::Waker) -> bool {
        if self.is_shutdown && self.active_count == 0 {
            return true;
        }
        if *task_id == usize::MAX {
            // Balance the increment to `active_count` in `spawn`.
            self.active_count -= 1;
            *task_id = self.registered.insert(waker);
            self.check_active_count();
        } else {
            *self.registered.get_mut(*task_id).unwrap() = waker;
        }
        false
    }
}

impl ExecutionScope {
    /// Constructs an execution scope that has no `entry_constructor`.  Use
    /// [`ExecutionScope::build()`] if you want to specify other parameters.
    pub fn new() -> Self {
        Self::build().new()
    }

    /// Constructs a new execution scope builder, wrapping the specified executor and optionally
    /// accepting additional parameters.  Run [`ExecutionScopeParams::new()`] to get an actual
    /// [`ExecutionScope`] object.
    pub fn build() -> ExecutionScopeParams {
        ExecutionScopeParams { entry_constructor: None }
    }

    /// Sends a `task` to be executed in this execution scope.  This is very similar to
    /// [`futures::task::Spawn::spawn_obj()`] with a minor difference that `self` reference is not
    /// exclusive.
    ///
    /// If the task needs to prevent itself from being shutdown, then it should use the
    /// `try_active_guard` function below.
    ///
    /// For the "vfs" library it is more convenient that this method allows non-exclusive
    /// access.  And as the implementation is employing internal mutability there are no downsides.
    /// This way `ExecutionScope` can actually also implement [`futures::task::Spawn`] - it just was
    /// not necessary for now.
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        // We consider the task as active until it first runs. Otherwise, it is possible to shut
        // down the executor before the task has been dropped which would deny it the opportunity to
        // correctly spawn a task in its drop function.
        self.executor.inner.lock().unwrap().active_count += 1;
        fuchsia_async::Task::spawn(TaskRunner {
            task,
            task_state: TaskState { executor: self.executor.clone(), task_id: usize::MAX },
        })
        .detach();
    }

    pub fn token_registry(&self) -> &TokenRegistry {
        &self.executor.token_registry
    }

    pub fn entry_constructor(&self) -> Option<Arc<dyn EntryConstructor + Send + Sync>> {
        self.entry_constructor.as_ref().map(Arc::clone)
    }

    pub fn shutdown(&self) {
        self.executor.shutdown();
    }

    /// Wait for all tasks to complete.
    pub async fn wait(&self) {
        let receiver = {
            let mut this = self.executor.inner.lock().unwrap();
            if this.registered.is_empty() && this.active_count == 0 {
                None
            } else {
                let (sender, receiver) = oneshot::channel::<()>();
                this.waiters.push(sender);
                Some(receiver)
            }
        };
        if let Some(receiver) = receiver {
            receiver.await.unwrap();
        }
    }

    /// Prevents the executor from shutting down whilst the guard is held. Returns None if the
    /// executor is shutting down.
    pub fn try_active_guard(&self) -> Option<ActiveGuard> {
        let mut inner = self.executor.inner.lock().unwrap();
        if inner.is_shutdown {
            return None;
        }
        inner.active_count += 1;
        Some(ActiveGuard(self.executor.clone()))
    }

    /// As above, but succeeds even if the executor is shutting down. This can be used in drop
    /// implementations to spawn tasks that *must* run before the executor shuts down.
    pub fn active_guard(&self) -> ActiveGuard {
        self.executor.inner.lock().unwrap().active_count += 1;
        ActiveGuard(self.executor.clone())
    }
}

impl Clone for ExecutionScope {
    fn clone(&self) -> Self {
        ExecutionScope {
            executor: self.executor.clone(),
            entry_constructor: self.entry_constructor.as_ref().map(Arc::clone),
        }
    }
}

impl PartialEq for ExecutionScope {
    fn eq(&self, other: &Self) -> bool {
        Arc::as_ptr(&self.executor) == Arc::as_ptr(&other.executor)
    }
}

impl Eq for ExecutionScope {}

impl std::fmt::Debug for ExecutionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(ExecutionScope)).finish()
    }
}

pub struct ExecutionScopeParams {
    entry_constructor: Option<Arc<dyn EntryConstructor + Send + Sync>>,
}

impl ExecutionScopeParams {
    pub fn entry_constructor(mut self, value: Arc<dyn EntryConstructor + Send + Sync>) -> Self {
        assert!(self.entry_constructor.is_none(), "`entry_constructor` is already set");
        self.entry_constructor = Some(value);
        self
    }

    pub fn new(self) -> ExecutionScope {
        ExecutionScope {
            executor: Arc::new(Executor {
                token_registry: TokenRegistry::new(),
                inner: Mutex::new(Inner {
                    registered: Slab::new(),
                    waiters: Vec::new(),
                    is_shutdown: false,
                    active_count: 0,
                }),
            }),
            entry_constructor: self.entry_constructor,
        }
    }
}

struct TaskState {
    executor: Arc<Executor>,
    task_id: usize,
}

impl Drop for TaskState {
    fn drop(&mut self) {
        self.executor
            .inner
            .lock()
            .unwrap()
            .task_did_finish(std::mem::replace(&mut self.task_id, usize::MAX));
    }
}

#[pin_project]
// The debugger knows how to get to `task`. If this changes, or its fully-qualified name changes,
// the debugger will need updating.
// LINT.IfChange
struct TaskRunner<F> {
    #[pin]
    task: F,
    // Must be dropped *after* task above.
    task_state: TaskState,
}
// LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)

impl<F: 'static + Future<Output = ()> + Send> Future for TaskRunner<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.project();
        if this
            .task_state
            .executor
            .inner
            .lock()
            .unwrap()
            .register_task(&mut this.task_state.task_id, cx.waker().clone())
        {
            return Poll::Ready(());
        }
        this.task.poll(cx)
    }
}

impl Executor {
    fn shutdown(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.is_shutdown = true;
        inner.check_active_count();
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ActiveGuard prevents the executor from shutting down until the guard is dropped.
pub struct ActiveGuard(Arc<Executor>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        let mut inner = self.0.inner.lock().unwrap();
        inner.active_count -= 1;
        inner.check_active_count();
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutionScope;

    use crate::directory::mutable::entry_constructor::EntryConstructor;

    use {
        fuchsia_async::{Task, TestExecutor, Time, Timer},
        fuchsia_zircon::prelude::*,
        futures::{channel::oneshot, task::Poll, Future},
        pin_utils::pin_mut,
        std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    fn run_test<GetTest, GetTestRes>(get_test: GetTest)
    where
        GetTest: FnOnce(ExecutionScope) -> GetTestRes,
        GetTestRes: Future<Output = ()>,
    {
        let mut exec = TestExecutor::new();

        let scope = ExecutionScope::new();

        let test = get_test(scope);

        pin_mut!(test);
        assert_eq!(exec.run_until_stalled(&mut test), Poll::Ready(()), "Test did not complete");
    }

    #[test]
    fn simple() {
        run_test(|scope| {
            async move {
                let (sender, receiver) = oneshot::channel();
                let (counters, task) = mocks::ImmediateTask::new(sender);

                scope.spawn(task);

                // Make sure our task had a chance to execute.
                receiver.await.unwrap();

                assert_eq!(counters.drop_call(), 1);
                assert_eq!(counters.poll_call(), 1);
            }
        });
    }

    #[test]
    fn simple_drop() {
        run_test(|scope| {
            async move {
                let (poll_sender, poll_receiver) = oneshot::channel();
                let (processing_done_sender, processing_done_receiver) = oneshot::channel();
                let (drop_sender, drop_receiver) = oneshot::channel();
                let (counters, task) =
                    mocks::ControlledTask::new(poll_sender, processing_done_receiver, drop_sender);

                scope.spawn(task);

                poll_receiver.await.unwrap();

                processing_done_sender.send(()).unwrap();

                scope.shutdown();

                drop_receiver.await.unwrap();

                // poll might be called one or two times depending on the order in which the
                // executor decides to poll the two tasks (this one and the one we spawned).
                let poll_count = counters.poll_call();
                assert!(poll_count >= 1, "poll was not called");

                assert_eq!(counters.drop_call(), 1);
            }
        });
    }

    #[test]
    fn test_wait_waits_for_tasks_to_finish() {
        let mut executor = TestExecutor::new();
        let scope = ExecutionScope::new();
        executor.run_singlethreaded(async {
            let (poll_sender, poll_receiver) = oneshot::channel();
            let (processing_done_sender, processing_done_receiver) = oneshot::channel();
            let (drop_sender, _drop_receiver) = oneshot::channel();
            let (_, task) =
                mocks::ControlledTask::new(poll_sender, processing_done_receiver, drop_sender);

            scope.spawn(task);

            poll_receiver.await.unwrap();

            // We test that wait is working correctly by concurrently waiting and telling the
            // task to complete, and making sure that the order is correct.
            let done = std::sync::Mutex::new(false);
            futures::join!(
                async {
                    scope.wait().await;
                    assert_eq!(*done.lock().unwrap(), true);
                },
                async {
                    // This is a Turing halting problem so the sleep is justified.
                    Timer::new(Time::after(100.millis())).await;
                    *done.lock().unwrap() = true;
                    processing_done_sender.send(()).unwrap();
                }
            );
        });
    }

    #[fuchsia::test]
    async fn test_active_guard() {
        let scope = ExecutionScope::new();
        let (guard_taken_tx, guard_taken_rx) = oneshot::channel();
        let (shutdown_triggered_tx, shutdown_triggered_rx) = oneshot::channel();
        let (drop_task_tx, drop_task_rx) = oneshot::channel();
        let scope_clone = scope.clone();
        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();
        scope.spawn(async move {
            {
                struct OnDrop((ExecutionScope, Option<oneshot::Receiver<()>>));
                impl Drop for OnDrop {
                    fn drop(&mut self) {
                        let guard = self.0 .0.active_guard();
                        let rx = self.0 .1.take().unwrap();
                        Task::spawn(async move {
                            rx.await.unwrap();
                            std::mem::drop(guard);
                        })
                        .detach();
                    }
                }
                let _guard = scope_clone.try_active_guard().unwrap();
                let _on_drop = OnDrop((scope_clone, Some(drop_task_rx)));
                guard_taken_tx.send(()).unwrap();
                shutdown_triggered_rx.await.unwrap();
                // Stick a timer here and record whether we're done to make sure we get to run to
                // completion.
                Timer::new(std::time::Duration::from_millis(100)).await;
                done_clone.store(true, Ordering::SeqCst);
            }
        });
        guard_taken_rx.await.unwrap();
        scope.shutdown();

        // The task should keep running whilst it has an active guard. Introduce a timer here to
        // make failing more likely if it's broken.
        Timer::new(std::time::Duration::from_millis(100)).await;
        let mut shutdown_wait = std::pin::pin!(scope.wait());
        assert_eq!(futures::poll!(shutdown_wait.as_mut()), Poll::Pending);

        shutdown_triggered_tx.send(()).unwrap();

        // The drop task should now start running and the executor still shouldn't have finished.
        Timer::new(std::time::Duration::from_millis(100)).await;
        assert_eq!(futures::poll!(shutdown_wait.as_mut()), Poll::Pending);

        drop_task_tx.send(()).unwrap();

        shutdown_wait.await;

        assert!(done.load(Ordering::SeqCst));
    }

    #[test]
    fn with_mock_entry_constructor() {
        let entry_constructor: Arc<dyn EntryConstructor + Send + Sync> =
            mocks::MockEntryConstructor::new();

        let scope = ExecutionScope::build().entry_constructor(entry_constructor.clone()).new();

        let entry_constructor2 = scope.entry_constructor().unwrap();
        assert!(
            // Note this ugly cast in place of
            // `Arc::ptr_eq(&entry_constructor, &entry_constructor2)` here is
            // to ensure we don't compare vtable pointers, which are not strictly guaranteed to be
            // the same across casts done in different code generation units at compilation time.
            entry_constructor.as_ref() as *const dyn EntryConstructor as *const u8
                == entry_constructor2.as_ref() as *const dyn EntryConstructor as *const u8,
            "`scope` returned `Arc` to an entry constructor is different from the one initially \
             set."
        );
    }

    mod mocks {
        use crate::{
            directory::{
                entry::DirectoryEntry,
                mutable::entry_constructor::{EntryConstructor, NewEntryType},
            },
            path::Path,
        };

        use {
            fuchsia_zircon::Status,
            futures::{
                channel::oneshot,
                task::{Context, Poll},
                Future,
            },
            std::{
                ops::Drop,
                pin::Pin,
                sync::{
                    atomic::{AtomicUsize, Ordering},
                    Arc,
                },
            },
        };

        pub(super) struct TaskCounters {
            poll_call_count: Arc<AtomicUsize>,
            drop_call_count: Arc<AtomicUsize>,
        }

        impl TaskCounters {
            fn new() -> (Arc<AtomicUsize>, Arc<AtomicUsize>, Self) {
                let poll_call_count = Arc::new(AtomicUsize::new(0));
                let drop_call_count = Arc::new(AtomicUsize::new(0));

                (
                    poll_call_count.clone(),
                    drop_call_count.clone(),
                    Self { poll_call_count, drop_call_count },
                )
            }

            pub(super) fn poll_call(&self) -> usize {
                self.poll_call_count.load(Ordering::Relaxed)
            }

            pub(super) fn drop_call(&self) -> usize {
                self.drop_call_count.load(Ordering::Relaxed)
            }
        }

        pub(super) struct ImmediateTask {
            poll_call_count: Arc<AtomicUsize>,
            drop_call_count: Arc<AtomicUsize>,
            done_sender: Option<oneshot::Sender<()>>,
        }

        impl ImmediateTask {
            pub(super) fn new(done_sender: oneshot::Sender<()>) -> (TaskCounters, Self) {
                let (poll_call_count, drop_call_count, counters) = TaskCounters::new();
                (
                    counters,
                    Self { poll_call_count, drop_call_count, done_sender: Some(done_sender) },
                )
            }
        }

        impl Future for ImmediateTask {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Self::Output> {
                self.poll_call_count.fetch_add(1, Ordering::Relaxed);

                if let Some(sender) = self.done_sender.take() {
                    sender.send(()).unwrap();
                }

                Poll::Ready(())
            }
        }

        impl Drop for ImmediateTask {
            fn drop(&mut self) {
                self.drop_call_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        impl Unpin for ImmediateTask {}

        pub(super) struct ControlledTask {
            poll_call_count: Arc<AtomicUsize>,
            drop_call_count: Arc<AtomicUsize>,

            drop_sender: Option<oneshot::Sender<()>>,
            future: Pin<Box<dyn Future<Output = ()> + Send>>,
        }

        impl ControlledTask {
            pub(super) fn new(
                poll_sender: oneshot::Sender<()>,
                processing_complete: oneshot::Receiver<()>,
                drop_sender: oneshot::Sender<()>,
            ) -> (TaskCounters, Self) {
                let (poll_call_count, drop_call_count, counters) = TaskCounters::new();
                (
                    counters,
                    Self {
                        poll_call_count,
                        drop_call_count,
                        drop_sender: Some(drop_sender),
                        future: Box::pin(async move {
                            poll_sender.send(()).unwrap();
                            processing_complete.await.unwrap();
                        }),
                    },
                )
            }
        }

        impl Future for ControlledTask {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
                self.poll_call_count.fetch_add(1, Ordering::Relaxed);
                self.future.as_mut().poll(cx)
            }
        }

        impl Drop for ControlledTask {
            fn drop(&mut self) {
                self.drop_call_count.fetch_add(1, Ordering::Relaxed);
                self.drop_sender.take().unwrap().send(()).unwrap();
            }
        }

        pub(super) struct MockEntryConstructor {}

        impl MockEntryConstructor {
            pub(super) fn new() -> Arc<Self> {
                Arc::new(Self {})
            }
        }

        impl EntryConstructor for MockEntryConstructor {
            fn create_entry(
                self: Arc<Self>,
                _parent: Arc<dyn DirectoryEntry>,
                _what: NewEntryType,
                _name: &str,
                _path: &Path,
            ) -> Result<Arc<dyn DirectoryEntry>, Status> {
                panic!("Not implemented")
            }
        }
    }
}
