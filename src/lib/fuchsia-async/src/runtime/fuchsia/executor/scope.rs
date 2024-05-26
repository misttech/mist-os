// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/339724492): Make this API available for general use.
#![doc(hidden)]

use super::common::{Executor, Task};
use fuchsia_sync::Mutex;
use fuchsia_sync::MutexGuard;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::{
    borrow::Borrow,
    fmt,
    future::Future,
    hash,
    ops::Deref,
    sync::{Arc, Weak},
    task::{Context, Poll, Waker},
};

/// A unique handle to a scope.
///
/// When this handle is dropped, the scope is cancelled.
pub struct Scope {
    pub(super) inner: ScopeRef,
}

/// A reference to a scope, which may be used to spawn tasks.
#[derive(Clone)]
pub struct ScopeRef {
    pub(super) inner: Arc<ScopeInner>,
}

pub(super) struct ScopeInner {
    pub(super) executor: Arc<Executor>,
    pub(super) parent: Option<ScopeRef>,
    pub(super) state: Mutex<ScopeState>,
    _private: (),
}

#[derive(Default)]
pub(super) struct ScopeState {
    pub(super) all_tasks: HashMap<usize, Arc<Task>>,
    pub(super) join_wakers: HashMap<usize, Waker>,
    pub(super) children: HashSet<WeakScopeRef>,
    pub(super) cancelled: bool,
}

impl Scope {
    /// Creates a child scope.
    pub fn new_child(&self) -> Scope {
        self.inner.new_child()
    }

    /// Creates a [`ScopeRef`] to this scope.
    pub fn make_ref(&self) -> ScopeRef {
        self.inner.clone()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        // Cancel all tasks in the scope. Each task has a strong reference to the ScopeState,
        // which will be dropped after all the tasks in the scope are dropped.

        // TODO(https://fxbug.dev/340638625): Ideally we would drop all tasks
        // here, but we cannot do that without either:
        // - Sync drop support in AtomicFuture, or
        // - The ability to reparent tasks, which requires atomic_arc or
        //   acquiring a mutex during polling.
        self.inner.cancel_all_tasks();
    }
}

impl Deref for Scope {
    type Target = ScopeRef;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl ScopeRef {
    /// Spawns a task on the scope.
    pub fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> crate::Task<T> {
        crate::Task::spawn_on(self.clone(), future)
    }

    pub(super) fn root(executor: Arc<Executor>) -> ScopeRef {
        ScopeRef {
            inner: Arc::new(ScopeInner {
                executor,
                parent: None,
                state: Default::default(),
                _private: (),
            }),
        }
    }

    /// Creates a child scope.
    pub fn new_child(&self) -> Scope {
        let child = ScopeRef {
            inner: Arc::new(ScopeInner {
                executor: self.inner.executor.clone(),
                parent: Some(self.clone()),
                state: Mutex::new(ScopeState::default()),
                _private: (),
            }),
        };
        let weak = child.downgrade();
        self.inner.state.lock().children.insert(weak.clone());
        Scope { inner: child }
    }

    /// Creates a [`WeakScopeRef`] for this scope.
    pub fn downgrade(&self) -> WeakScopeRef {
        WeakScopeRef { inner: Arc::downgrade(&self.inner) }
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, ScopeState> {
        self.inner.state.lock()
    }

    #[inline(always)]
    pub(super) fn executor(&self) -> &Arc<Executor> {
        &self.inner.executor
    }

    /// Marks the task as detached.
    pub(crate) fn detach(&self, task_id: usize) {
        let mut state = self.lock();
        if let Some(task) = state.all_tasks.get(&task_id) {
            task.future.detach();
        }
        state.join_wakers.remove(&task_id);
    }

    /// Cancels the task.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `R` is the correct type.
    pub(crate) unsafe fn cancel<R>(&self, task_id: usize) -> Option<R> {
        let mut state = self.lock();
        state.join_wakers.remove(&task_id);
        state.all_tasks.get(&task_id).and_then(|task| {
            if task.future.cancel() {
                self.inner.executor.ready_tasks.push(task.clone());
            }
            task.future.take_result()
        })
    }

    /// Polls for a join result for the given task ID.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `R` is the correct type.
    pub(crate) unsafe fn poll_join_result<R>(
        &self,
        task_id: usize,
        cx: &mut Context<'_>,
    ) -> Poll<R> {
        let mut state = self.inner.state.lock();
        let Some(task) = state.all_tasks.get(&task_id) else { return Poll::Pending };
        if let Some(result) = task.future.take_result() {
            state.join_wakers.remove(&task_id);
            state.all_tasks.remove(&task_id);
            Poll::Ready(result)
        } else {
            state.join_wakers.insert(task_id, cx.waker().clone());
            Poll::Pending
        }
    }

    /// Cancels tasks in this scope and all child scopes.
    fn cancel_all_tasks(&self) {
        let mut scopes = vec![self.clone()];
        while let Some(scope) = scopes.pop() {
            let mut state = scope.lock();
            state.cancelled = true;
            for task in state.all_tasks.values() {
                if task.future.cancel() {
                    task.scope.executor().ready_tasks.push(task.clone());
                }
            }
            // Copy children to a vec so we don't hold the lock for too long.
            scopes.extend(state.children.iter().filter_map(|child| child.upgrade()));
        }
    }

    /// Drops tasks in this scope and all child scopes.
    ///
    /// # Panics
    ///
    /// Panics if any task is being accessed by another thread. Only call this
    /// method when the executor is shutting down and there are no other pollers.
    pub(super) fn drop_all_tasks(&self) {
        let mut scopes = vec![self.clone()];
        while let Some(scope) = scopes.pop() {
            let tasks = {
                let mut state = scope.lock();
                state.cancelled = true;
                scopes.extend(state.children.drain().filter_map(|child| child.upgrade()));
                std::mem::take(&mut state.all_tasks)
            };
            // Call task destructors once the scope lock is released so we don't risk a deadlock.
            for (_id, task) in tasks {
                task.future.try_drop().expect("Expected drop to succeed");
            }
        }
    }
}

impl fmt::Debug for ScopeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope").finish()
    }
}

impl Drop for ScopeInner {
    fn drop(&mut self) {
        if let Some(parent) = &self.parent {
            let mut parent_state = parent.lock();
            // SAFETY: PtrKey is a ZST so we aren't creating a reference to invalid memory.
            // This also complies with the correctness requirements of
            // HashSet::remove because the implementations of Hash and Eq match
            // between PtrKey and WeakScopeRef.
            let key = unsafe { &*(self as *const _ as *const PtrKey) };
            if !parent_state.children.is_empty() && !parent_state.children.remove(key) {
                // We should match a key in the parent because ScopeInner is
                // never moved out of its Arc, and any parented scope is in its
                // parent unless that parent is being dropped (in which case
                // children will be empty, as in the check above).
                panic!()
            }
        }
    }
}

/// A weak reference to a scope.
#[derive(Clone)]
pub struct WeakScopeRef {
    pub(super) inner: Weak<ScopeInner>,
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

/// Optimizes removal from parent scope.
#[repr(transparent)]
struct PtrKey;

impl Borrow<PtrKey> for WeakScopeRef {
    fn borrow(&self) -> &PtrKey {
        // SAFETY: PtrKey is a ZST so we aren't creating a reference to invalid memory.
        unsafe { &*(self.inner.as_ptr() as *const PtrKey) }
    }
}

impl PartialEq for PtrKey {
    fn eq(&self, other: &Self) -> bool {
        self as *const _ == other as *const _
    }
}

impl Eq for PtrKey {}

impl hash::Hash for PtrKey {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        (self as *const PtrKey).hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestExecutor;
    use std::pin::pin;

    #[test]
    fn spawn_works_on_root_scope() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope();
        let mut task = pin!(scope.spawn(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(1));
    }

    #[test]
    fn spawn_works_on_new_child() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let mut task = pin!(scope.spawn(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(1));
    }

    #[test]
    fn scope_drop_cancels_tasks() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let mut task = pin!(scope.spawn(async { 1 }));
        drop(scope);
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Pending);
    }

    #[test]
    fn tasks_do_not_spawn_on_cancelled_scopes() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        drop(scope);
        let mut task = pin!(scope_ref.spawn(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Pending);
    }

    #[test]
    fn spawn_works_on_child_and_grandchild() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        let grandchild = child.new_child();
        let mut child_task = pin!(child.spawn(async { 1 }));
        let mut grandchild_task = pin!(grandchild.spawn(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut child_task), Poll::Ready(1));
        assert_eq!(executor.run_until_stalled(&mut grandchild_task), Poll::Ready(1));
    }

    #[test]
    fn spawn_drop_cancels_child_and_grandchild_tasks() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        let grandchild = child.new_child();
        let mut child_task = pin!(child.spawn(async { 1 }));
        let mut grandchild_task = pin!(grandchild.spawn(async { 1 }));
        drop(scope);
        assert_eq!(executor.run_until_stalled(&mut child_task), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut grandchild_task), Poll::Pending);
    }
}
