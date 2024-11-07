// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This implements a *very* limited version of the fuchsia Scope API.  More can be added
// as needed.

use crate::condition::Condition;
use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};
use std::{fmt, hash};
use tokio::task::AbortHandle;

/// A unique handle to a scope. When this handle is dropped, the scope is
/// cancelled.
///
/// See the [Fuchsia target documentation on this type][docs] for more
/// information about scopes. This version of the Scope API is a limited subset
/// of that API.
///
/// [docs]: https://fuchsia-docs.firebaseapp.com/rust/fuchsia_async/struct.Scope.html
pub struct Scope {
    inner: ScopeRef,
}

impl Scope {
    /// Returns a new scope that is a child of the root scope of the executor.
    pub fn new() -> Self {
        Self {
            inner: ScopeRef {
                inner: Arc::new(ScopeInner {
                    state: Condition::new(ScopeState { all_tasks: HashMap::new() }),
                }),
            },
        }
    }

    /// Creates a [`ScopeRef`] to this scope.
    pub fn make_ref(&self) -> ScopeRef {
        self.inner.clone()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        for (_, (abort_handle, _)) in std::mem::take(&mut self.inner.inner.state.lock().all_tasks) {
            abort_handle.abort();
        }
    }
}

impl Deref for Scope {
    type Target = ScopeRef;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// A reference to a scope, which may be used to spawn tasks.
#[derive(Clone)]
pub struct ScopeRef {
    inner: Arc<ScopeInner>,
}

impl ScopeRef {
    /// Spawns a task on the scope.
    pub fn spawn(
        &self,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> crate::JoinHandle<()> {
        self.compute(future).detach_on_drop()
    }

    /// Spawns a task on the scope.
    pub fn spawn_local(&self, future: impl Future<Output = ()> + 'static) -> crate::JoinHandle<()> {
        self.compute_local(future).detach_on_drop()
    }

    /// Spawns a task on the scope, but unlike `spawn` returns a handle that will drop the future
    /// when dropped.
    pub fn compute<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> crate::Task<T> {
        let (task_id, fut) = self.mk_task_fut(future);
        // Take the lock first so that the insert below happens before the task tries to take the
        // lock to set the waker.
        let mut state = self.inner.state.lock();
        let task = crate::Task::spawn(fut);
        state.all_tasks.insert(task_id, (task.abort_handle(), None));
        task
    }

    /// Spawns a task on the scope, but unlike `spawn` returns a handle that will drop the future
    /// when dropped.
    pub fn compute_local<T: 'static>(
        &self,
        future: impl Future<Output = T> + 'static,
    ) -> crate::Task<T> {
        let (task_id, fut) = self.mk_task_fut(future);
        // Take the lock first so that the insert below happens before the task tries to take the
        // lock to set the waker.
        let mut state = self.inner.state.lock();
        let task = crate::Task::local(fut);
        state.all_tasks.insert(task_id, (task.abort_handle(), None));
        task
    }

    fn mk_task_fut<T: 'static, F: Future<Output = T> + 'static>(
        &self,
        future: F,
    ) -> (u64, impl Future<Output = T>) {
        static TASK_ID: AtomicU64 = AtomicU64::new(0);

        let task_id = TASK_ID.fetch_add(1, Ordering::Relaxed);

        // When the task is dropped, we must clean up our entry in `all_tasks`.
        struct OnDrop {
            scope: WeakScopeRef,
            task_id: u64,
        }

        impl Drop for OnDrop {
            fn drop(&mut self) {
                if let Some(scope) = self.scope.upgrade() {
                    let wakers = {
                        let mut state = scope.inner.state.lock();
                        state.all_tasks.remove(&self.task_id);
                        if state.all_tasks.is_empty() {
                            state.drain_wakers().collect()
                        } else {
                            Vec::new()
                        }
                    };
                    for waker in wakers {
                        waker.wake();
                    }
                }
            }
        }

        let on_drop = OnDrop { scope: self.downgrade(), task_id };

        let weak = self.downgrade();
        let fut = async move {
            let _on_drop = on_drop;
            let mut future = std::pin::pin!(future);
            // Every time we poll, we must set the waker so that `wake_all` works.
            poll_fn(|cx| {
                if let Some(scope) = weak.upgrade() {
                    if let Some(task) = scope.inner.state.lock().all_tasks.get_mut(&task_id) {
                        task.1 = Some(cx.waker().clone());
                    }
                }
                future.as_mut().poll(cx)
            })
            .await
        };

        (task_id, fut)
    }

    /// Creates a [`WeakScopeRef`] for this scope.
    fn downgrade(&self) -> WeakScopeRef {
        WeakScopeRef { inner: Arc::downgrade(&self.inner) }
    }

    /// Waits for there to be no tasks.  This is racy: as soon as this returns it is possible for
    /// another task to have been spawned on this scope.
    pub async fn on_no_tasks(&self) {
        self.inner
            .state
            .when(|state| if state.all_tasks.is_empty() { Poll::Ready(()) } else { Poll::Pending })
            .await;
    }

    /// Wakes all the scope's futures.
    pub fn wake_all(&self) {
        let mut wakers = Vec::new();
        {
            let mut state = self.inner.state.lock();
            for (_, (_, waker)) in &mut state.all_tasks {
                wakers.extend(waker.take());
            }
        }
        for waker in wakers {
            waker.wake();
        }
    }
}

impl fmt::Debug for ScopeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope").finish()
    }
}

/// A weak reference to a scope.
#[derive(Clone)]
struct WeakScopeRef {
    inner: Weak<ScopeInner>,
}

impl WeakScopeRef {
    /// Upgrades to a [`ScopeRef`] if the scope still exists.
    pub fn upgrade(&self) -> Option<ScopeRef> {
        self.inner.upgrade().map(|inner| ScopeRef { inner })
    }
}

impl hash::Hash for WeakScopeRef {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        Weak::as_ptr(&self.inner).hash(state);
    }
}

impl PartialEq for WeakScopeRef {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for WeakScopeRef {
    // Weak::ptr_eq should return consistent results, even when the inner value
    // has been dropped.
}

struct ScopeInner {
    state: Condition<ScopeState>,
}

struct ScopeState {
    all_tasks: HashMap<u64, (AbortHandle, Option<Waker>)>,
}

#[cfg(test)]
mod tests {
    use super::Scope;
    use crate::TestExecutor;
    use futures::future::join_all;
    use std::future::{pending, poll_fn, Future};
    use std::pin::{pin, Pin};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};

    #[test]
    fn on_no_tasks() {
        let mut executor = TestExecutor::new();
        let scope = Scope::new();
        let _task1 = scope.spawn(std::future::ready(()));
        let task2 = scope.compute(pending::<()>());

        executor.run_singlethreaded(async {
            let mut on_no_tasks = pin!(scope.on_no_tasks());

            for _ in 0..10 {
                assert!(futures::poll!(&mut on_no_tasks).is_pending());
            }

            let _ = task2.cancel();

            let on_no_tasks2 = pin!(scope.on_no_tasks());
            let on_no_tasks3 = pin!(scope.on_no_tasks());
            join_all([on_no_tasks, on_no_tasks2, on_no_tasks3]).await;
        });
    }

    #[test]
    fn wake_all() {
        let mut executor = TestExecutor::new();
        let scope = Scope::new();

        let poll_count = Arc::new(AtomicU64::new(0));

        struct PollCounter(Arc<AtomicU64>);

        impl Future for PollCounter {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Poll::Pending
            }
        }

        scope.spawn(PollCounter(poll_count.clone()));
        scope.spawn(PollCounter(poll_count.clone()));

        executor.run_singlethreaded(async {
            let mut start_count = 0;
            for _ in 0..4 {
                poll_fn(|cx| {
                    if poll_count.load(Ordering::Relaxed) < start_count + 2 {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    } else {
                        Poll::Ready(())
                    }
                })
                .await;
                scope.wake_all();
                start_count += 2;
            }
        });
    }
}
