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

use crate::token_registry::TokenRegistry;

use fuchsia_async::{JoinHandle, Scope, Task};
use futures::task::{self, Poll};
use futures::Future;
use std::future::{pending, poll_fn};
use std::pin::pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::ready;

#[cfg(target_os = "fuchsia")]
use fuchsia_async::EHandle;

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
#[derive(Clone)]
pub struct ExecutionScope {
    executor: Arc<Executor>,
}

struct Executor {
    inner: Mutex<Inner>,
    token_registry: TokenRegistry,
    scope: OnceLock<Scope>,
}

struct Inner {
    /// Records the kind of shutdown that has been called on the executor.
    shutdown_state: ShutdownState,

    /// The number of active tasks preventing shutdown.
    active_count: usize,

    /// A fake active task that we use when there are no other tasks yet there's still an an active
    /// count.
    fake_active_task: Option<Task<()>>,
}

#[derive(Copy, Clone, PartialEq)]
enum ShutdownState {
    Active,
    Shutdown,
    ForceShutdown,
}

impl ExecutionScope {
    /// Constructs an execution scope.  Use [`ExecutionScope::build()`] if you want to specify
    /// parameters.
    pub fn new() -> Self {
        Self::build().new()
    }

    /// Constructs a new execution scope builder, wrapping the specified executor and optionally
    /// accepting additional parameters.  Run [`ExecutionScopeParams::new()`] to get an actual
    /// [`ExecutionScope`] object.
    pub fn build() -> ExecutionScopeParams {
        ExecutionScopeParams::default()
    }

    /// Returns the active count: the number of tasks that are active and will prevent shutdown.
    pub fn active_count(&self) -> usize {
        self.executor.inner.lock().unwrap().active_count
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
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
        let executor = self.executor.clone();
        self.executor.scope().spawn(async move {
            let mut task = std::pin::pin!(task);
            poll_fn(|cx| {
                let shutdown_state = executor.inner.lock().unwrap().shutdown_state;
                match task.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(()),
                    Poll::Pending => match shutdown_state {
                        ShutdownState::Active => Poll::Pending,
                        ShutdownState::Shutdown
                            if executor.inner.lock().unwrap().active_count > 0 =>
                        {
                            Poll::Pending
                        }
                        _ => Poll::Ready(()),
                    },
                }
            })
            .await;
        })
    }

    pub fn token_registry(&self) -> &TokenRegistry {
        &self.executor.token_registry
    }

    pub fn shutdown(&self) {
        self.executor.shutdown();
    }

    /// Forcibly shut down the executor without respecting the active guards.
    pub fn force_shutdown(&self) {
        let mut inner = self.executor.inner.lock().unwrap();
        inner.shutdown_state = ShutdownState::ForceShutdown;
        self.executor.scope().wake_all();
    }

    /// Restores the executor so that it is no longer in the shut-down state.  Any tasks
    /// that are still running will continue to run after calling this.
    pub fn resurrect(&self) {
        self.executor.inner.lock().unwrap().shutdown_state = ShutdownState::Active;
    }

    /// Wait for all tasks to complete.
    pub async fn wait(&self) {
        let mut on_no_tasks = pin!(self.executor.scope().on_no_tasks());
        poll_fn(|cx| {
            // Hold the lock whilst we poll the scope so that the active count can't change.
            let mut inner = self.executor.inner.lock().unwrap();
            ready!(on_no_tasks.as_mut().poll(cx));
            if inner.active_count == 0 {
                Poll::Ready(())
            } else {
                // There are no tasks but there's an active count and we must only finish when there
                // are no tasks *and* the active count is zero.  To address this, we spawn a fake
                // task so that we can just use `on_no_tasks`, and then we'll cancel the task when
                // the active count drops to zero.
                let scope = self.executor.scope();
                inner.fake_active_task = Some(scope.compute(pending::<()>()));
                on_no_tasks.set(scope.on_no_tasks());
                assert!(on_no_tasks.as_mut().poll(cx).is_pending());
                Poll::Pending
            }
        })
        .await;
    }

    /// Prevents the executor from shutting down whilst the guard is held. Returns None if the
    /// executor is shutting down.
    pub fn try_active_guard(&self) -> Option<ActiveGuard> {
        let mut inner = self.executor.inner.lock().unwrap();
        if inner.shutdown_state != ShutdownState::Active {
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

impl PartialEq for ExecutionScope {
    fn eq(&self, other: &Self) -> bool {
        Arc::as_ptr(&self.executor) == Arc::as_ptr(&other.executor)
    }
}

impl Eq for ExecutionScope {}

impl std::fmt::Debug for ExecutionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("ExecutionScope {:?}", Arc::as_ptr(&self.executor)))
    }
}

#[derive(Default)]
pub struct ExecutionScopeParams {
    #[cfg(target_os = "fuchsia")]
    async_executor: Option<EHandle>,
}

impl ExecutionScopeParams {
    #[cfg(target_os = "fuchsia")]
    pub fn executor(mut self, value: EHandle) -> Self {
        assert!(self.async_executor.is_none(), "`executor` is already set");
        self.async_executor = Some(value);
        self
    }

    pub fn new(self) -> ExecutionScope {
        ExecutionScope {
            executor: Arc::new(Executor {
                token_registry: TokenRegistry::new(),
                inner: Mutex::new(Inner {
                    shutdown_state: ShutdownState::Active,
                    active_count: 0,
                    fake_active_task: None,
                }),
                #[cfg(target_os = "fuchsia")]
                scope: self
                    .async_executor
                    .map_or_else(|| OnceLock::new(), |e| e.global_scope().new_child().into()),
                #[cfg(not(target_os = "fuchsia"))]
                scope: OnceLock::new(),
            }),
        }
    }
}

impl Executor {
    fn scope(&self) -> &Scope {
        // We lazily initialize the executor rather than at construction time as there are currently
        // a few tests that create the ExecutionScope before the async executor has been initialized
        // (which means we cannot call EHandle::local()).
        self.scope.get_or_init(|| {
            #[cfg(target_os = "fuchsia")]
            return Scope::global().new_child();
            #[cfg(not(target_os = "fuchsia"))]
            return Scope::new();
        })
    }

    fn shutdown(&self) {
        let wake_all = {
            let mut inner = self.inner.lock().unwrap();
            inner.shutdown_state = ShutdownState::Shutdown;
            inner.active_count == 0
        };
        if wake_all {
            if let Some(scope) = self.scope.get() {
                scope.wake_all();
            }
        }
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
        let wake_all = {
            let mut inner = self.0.inner.lock().unwrap();
            inner.active_count -= 1;
            if inner.active_count == 0 {
                if let Some(task) = inner.fake_active_task.take() {
                    let _ = task.cancel();
                }
            }
            inner.active_count == 0 && inner.shutdown_state == ShutdownState::Shutdown
        };
        if wake_all {
            self.0.scope().wake_all();
        }
    }
}

/// Yields to the executor, providing an opportunity for other futures to run.
pub async fn yield_to_executor() {
    let mut done = false;
    poll_fn(|cx| {
        if done {
            Poll::Ready(())
        } else {
            done = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::{yield_to_executor, ExecutionScope};

    use fuchsia_async::{Task, TestExecutor, Timer};
    use futures::channel::oneshot;
    use futures::stream::FuturesUnordered;
    use futures::task::Poll;
    use futures::{Future, StreamExt};
    use std::pin::pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[cfg(target_os = "fuchsia")]
    fn run_test<GetTest, GetTestRes>(get_test: GetTest)
    where
        GetTest: FnOnce(ExecutionScope) -> GetTestRes,
        GetTestRes: Future<Output = ()>,
    {
        let mut exec = TestExecutor::new();

        let scope = ExecutionScope::new();

        let test = get_test(scope);

        assert_eq!(
            exec.run_until_stalled(&mut pin!(test)),
            Poll::Ready(()),
            "Test did not complete"
        );
    }

    #[cfg(not(target_os = "fuchsia"))]
    fn run_test<GetTest, GetTestRes>(get_test: GetTest)
    where
        GetTest: FnOnce(ExecutionScope) -> GetTestRes,
        GetTestRes: Future<Output = ()>,
    {
        use fuchsia_async::TimeoutExt;
        let mut exec = TestExecutor::new();

        let scope = ExecutionScope::new();

        // This isn't a perfect equivalent to the target version, but Tokio
        // doesn't have run_until_stalled and it sounds like it's
        // architecturally impossible.
        let test =
            get_test(scope).on_stalled(Duration::from_secs(30), || panic!("Test did not complete"));

        exec.run_singlethreaded(&mut pin!(test));
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
                    Timer::new(Duration::from_millis(100)).await;
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

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test]
    async fn test_shutdown_waits_for_channels() {
        use fuchsia_async as fasync;

        let scope = ExecutionScope::new();
        let (rx, tx) = zx::Channel::create();
        let received_msg = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = futures::channel::oneshot::channel();
        {
            let received_msg = received_msg.clone();
            scope.spawn(async move {
                let mut msg_buf = zx::MessageBuf::new();
                msg_buf.ensure_capacity_bytes(64);
                let _ = sender.send(());
                let _ = fasync::Channel::from_channel(rx).recv_msg(&mut msg_buf).await;
                received_msg.store(true, Ordering::Relaxed);
            });
        }
        // Wait until the spawned future has been polled once.
        let _ = receiver.await;

        tx.write(b"hello", &mut []).expect("write failed");
        scope.shutdown();
        scope.wait().await;
        assert!(received_msg.load(Ordering::Relaxed));
    }

    #[fuchsia::test]
    async fn test_force_shutdown() {
        let scope = ExecutionScope::new();
        let scope_clone = scope.clone();
        let ref_count = Arc::new(());
        let ref_count_clone = ref_count.clone();

        // Spawn a task that holds a reference.  When the task is dropped the reference will get
        // dropped with it.
        scope.spawn(async move {
            let _ref_count_clone = ref_count_clone;

            // Hold an active guard so that only a forced shutdown will work.
            let _guard = scope_clone.active_guard();

            let _: () = std::future::pending().await;
        });

        scope.force_shutdown();
        scope.wait().await;

        // The task should have been dropped leaving us with the only reference.
        assert_eq!(Arc::strong_count(&ref_count), 1);

        // Test resurrection...
        scope.resurrect();

        let ref_count_clone = ref_count.clone();
        scope.spawn(async move {
            // Yield so that if the executor is in the shutdown state, it will kill this task.
            yield_to_executor().await;

            // Take another reference count so that we can check we got here below.
            let _ref_count = ref_count_clone.clone();

            let _: () = std::future::pending().await;
        });

        while Arc::strong_count(&ref_count) != 3 {
            yield_to_executor().await;
        }

        // Yield some more just to be sure the task isn't killed.
        for _ in 0..5 {
            yield_to_executor().await;
            assert_eq!(Arc::strong_count(&ref_count), 3);
        }
    }

    #[fuchsia::test]
    async fn test_task_runs_once() {
        let scope = ExecutionScope::new();

        // Spawn a task.
        scope.spawn(async {});

        scope.shutdown();

        let polled = Arc::new(AtomicBool::new(false));
        let polled_clone = polled.clone();

        let scope_clone = scope.clone();

        // Use FuturesUnordered so that it uses its own waker.
        let mut futures = FuturesUnordered::new();
        futures.push(async move { scope_clone.wait().await });

        // Poll it now to set up a waker.
        assert_eq!(futures::poll!(futures.next()), Poll::Pending);

        // Spawn another task.  When this task runs, wait still shouldn't be resolved because at
        // this point the first task hasn't finished.
        scope.spawn(async move {
            assert_eq!(futures::poll!(futures.next()), Poll::Pending);
            polled_clone.store(true, Ordering::Relaxed);
        });

        scope.wait().await;

        // Make sure the second spawned task actually ran.
        assert!(polled.load(Ordering::Relaxed));
    }

    mod mocks {
        use futures::channel::oneshot;
        use futures::task::{Context, Poll};
        use futures::Future;
        use std::pin::Pin;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

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

            fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
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

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
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
    }
}
