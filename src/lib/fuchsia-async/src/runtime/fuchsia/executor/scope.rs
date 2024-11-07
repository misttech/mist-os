// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::super::task::JoinHandle;
use super::common::{Executor, Task};
use crate::atomic_future::{AtomicFuture, CancelAndDetachResult};
use crate::condition::{Condition, ConditionGuard, WakerEntry};
use crate::EHandle;
use pin_project_lite::pin_project;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use state::{JoinResult, ScopeState, ScopeWaker, Status};
use std::borrow::Borrow;
use std::collections::hash_map::Entry;
use std::collections::hash_set;
use std::future::{Future, IntoFuture};
use std::mem::{self, ManuallyDrop};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};
use std::{fmt, hash};

//
// # Public API
//

/// A unique handle to a task scope. Cancels the scope on drop.
///
/// Scopes are how fuchsia-async implements [structured concurrency][sc]. Every
/// task is spawned on a scope, and runs until either the task completes or the
/// scope is cancelled. In addition to owning tasks, scopes may own child
/// scopes, forming a nested structure.
///
/// Scopes are usually joined or cancelled when the owning code is done with
/// them. This makes it easier to reason about when a background task might
/// still be running. Note that in multithreaded contexts it is safer to cancel
/// and await a scope explicitly than to drop it, because the destructor is not
/// synchronized with other threads that might be running a task.
///
/// [`Task::spawn`][crate::Task::spawn] and related APIs spawn on the root scope
/// of the executor. New code is encouraged to spawn directly on scopes instead,
/// passing their handles as a way of documenting when a function might spawn
/// tasks that run in the background and reasoning about their side effects.
///
/// [sc]: https://en.wikipedia.org/wiki/Structured_concurrency
#[must_use = "Scopes should be explicitly awaited or cancelled"]
pub struct Scope {
    // LINT.IfChange
    inner: ScopeRef,
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
}

impl Scope {
    /// Return a new scope that is a child of the root scope of the executor.
    pub fn new() -> Scope {
        EHandle::local().root_scope().new_child()
    }

    /// Create a child scope.
    pub fn new_child(&self) -> Scope {
        self.inner.new_child()
    }

    /// Create a [`ScopeRef`] that may be used to spawn tasks on this scope.
    pub fn make_ref(&self) -> ScopeRef {
        self.inner.clone()
    }

    /// Wait for all tasks in the scope and its children to complete.
    ///
    /// Note that you can await a scope directly. `scope.join().await` is a more
    /// explicit form of `scope.await`.
    pub fn join(self) -> Join {
        Join::new(self)
    }

    /// Cancel all tasks in the scope and its children recursively.
    ///
    /// Once the returned future resolves, no task on the scope will be polled
    /// again.
    ///
    /// When a scope is cancelled it immediately stops accepting tasks. Handles
    /// of tasks spawned on the scope will pend forever.
    ///
    /// Dropping the `Scope` object is equivalent to calling this method and
    /// discarding the returned future. Awaiting the future is preferred because
    /// it eliminates the possibility of a task poll completing on another
    /// thread after the scope object has been dropped, which can sometimes
    /// result in surprising behavior.
    pub fn cancel(self) -> impl Future<Output = ()> {
        self.inner.cancel_all_tasks();
        Join::new(self)
    }

    /// Detach the scope, allowing its tasks to continue running in the
    /// background.
    ///
    /// Tasks of a detached scope are still subject to join and cancel
    /// operations on parent scopes.
    pub fn detach(self) {
        // Use ManuallyDrop to destructure self, because Rust doesn't allow this
        // for types which implement Drop.
        let this = ManuallyDrop::new(self);
        // SAFETY: this.inner is obviously valid, and we don't access `this`
        // after moving.
        mem::drop(unsafe { std::ptr::read(&this.inner) });
    }
}

/// Cancel the scope and all of its tasks. Prefer using the [`Scope::cancel`]
/// or [`Scope::join`] methods.
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

impl IntoFuture for Scope {
    type Output = ();

    type IntoFuture = Join;

    fn into_future(self) -> Self::IntoFuture {
        self.join()
    }
}

impl Deref for Scope {
    type Target = ScopeRef;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Borrow<ScopeRef> for Scope {
    fn borrow(&self) -> &ScopeRef {
        &*self
    }
}

pin_project! {
    /// Join handle for a [`Scope`].
    ///
    /// This is a future that resolves when all tasks on the scope are complete
    /// or have been cancelled.
    ///
    /// When this object is dropped, the scope and all tasks in it are
    /// cancelled.
    //
    // Note: The drop property is only true when S = Scope; it does not apply to
    // other (internal) uses of this struct.
    pub struct Join<S = Scope> {
        scope: S,
        #[pin]
        waker_entry: WakerEntry<ScopeState>,
    }
}

impl<S> Join<S> {
    fn new(scope: S) -> Self {
        Self { scope, waker_entry: WakerEntry::new() }
    }
}

impl Join {
    /// Cancel the scope. The future will resolve when all tasks have finished
    /// polling.
    ///
    /// See [`Scope::cancel`] for more details.
    pub fn cancel(self: Pin<&mut Self>) -> impl Future<Output = ()> + '_ {
        self.scope.inner.cancel_all_tasks();
        self
    }
}

impl<S> Future for Join<S>
where
    S: Borrow<ScopeRef>,
{
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut state = Borrow::borrow(&*this.scope).lock();
        if state.has_tasks() {
            state.add_waker(this.waker_entry, cx.waker().clone());
            Poll::Pending
        } else {
            state.close();
            Poll::Ready(())
        }
    }
}

/// A reference to a scope, which may be used to spawn tasks.
///
/// ## Ownership and cycles
///
/// Tasks running on a `Scope` may hold a `ScopeRef` to that scope. This does
/// not create an ownership cycle because the task will drop the `ScopeRef`
/// once it completes or is cancelled.
///
/// Naturally, scopes containing tasks that never complete and that are never
/// cancelled will never be freed. Holding a `ScopeRef` does not contribute to
/// this problem.
#[derive(Clone)]
pub struct ScopeRef {
    // LINT.IfChange
    inner: Arc<ScopeInner>,
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
}

impl ScopeRef {
    /// Spawn a new task on the scope.
    // This does not have the must_use attribute because it's common to detach and the lifetime of
    // the task is bound to the scope: when the scope is dropped, the task will be cancelled.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
        JoinHandle::new(self.clone(), self.executor().spawn(self, AtomicFuture::new(future, false)))
    }

    /// Spawn a new task on the scope of a thread local executor.
    ///
    /// NOTE: This is not supported with a [`SendExecutor`][crate::SendExecutor]
    /// and will cause a runtime panic. Use [`ScopeRef::spawn`] instead.
    pub fn spawn_local(&self, future: impl Future<Output = ()> + 'static) -> JoinHandle<()> {
        JoinHandle::new(self.clone(), self.executor().spawn_local(self, future, false))
    }

    /// Like `spawn`, but for tasks that return a result.  NOTE: Unlike `spawn`, when tasks are
    /// dropped, the future will be *cancelled*.
    pub fn compute<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> crate::Task<T> {
        JoinHandle::new(self.clone(), self.executor().spawn(self, AtomicFuture::new(future, false)))
            .into()
    }

    /// Like `spawn`, but for tasks that return a result.  NOTE: Unlike `spawn`, when tasks are
    /// dropped, the future will be *cancelled*.
    ///
    /// NOTE: This is not supported with a [`SendExecutor`][crate::SendExecutor]
    /// and will cause a runtime panic. Use [`ScopeRef::spawn`] instead.
    pub fn compute_local<T: 'static>(
        &self,
        future: impl Future<Output = T> + 'static,
    ) -> crate::Task<T> {
        JoinHandle::new(self.clone(), self.executor().spawn_local(self, future, false)).into()
    }

    pub(super) fn root(executor: Arc<Executor>) -> ScopeRef {
        ScopeRef {
            inner: Arc::new(ScopeInner {
                executor,
                state: Condition::new(ScopeState::new(None, Status::default())),
            }),
        }
    }

    /// Create a child scope.
    pub fn new_child(&self) -> Scope {
        let mut state = self.lock();
        let child = ScopeRef {
            inner: Arc::new(ScopeInner {
                executor: self.inner.executor.clone(),
                state: Condition::new(ScopeState::new(Some(self.clone()), state.status())),
            }),
        };
        let weak = child.downgrade();
        state.insert_child(weak.clone());
        Scope { inner: child }
    }

    /// Cancel all the scope's tasks.
    pub fn cancel(self) -> impl Future<Output = ()> {
        self.cancel_all_tasks();
        Join::new(self)
    }

    // Joining the scope could be allowed from a ScopeRef, but the use case
    // seems less common and more bug prone than cancelling. Someone might want
    // to cancel a scope from within the scope. But it's impossible to join a
    // scope from within a task on that scope; the task and scope would spend
    // forever waiting on each other. As a minor additional point, seeing calls
    // to `.join()` on a ScopeRef might cause a reader to think they have a
    // Scope.

    /// Wait for there to be no tasks. This is racy: as soon as this returns it is possible for
    /// another task to have been spawned on this scope.
    pub async fn on_no_tasks(&self) {
        self.inner
            .state
            .when(|state| if state.has_tasks() { Poll::Pending } else { Poll::Ready(()) })
            .await;
    }

    /// Wake all the scope's tasks so their futures will be polled again.
    pub fn wake_all(&self) {
        self.lock().wake_all();
    }
}

impl fmt::Debug for ScopeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope").finish()
    }
}

//
// # Internal API
//

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

// This module exists as a privacy boundary so that we can make sure any
// operation that might cause the scope to finish also wakes its waker.
mod state {
    use super::*;

    pub struct ScopeState {
        pub parent: Option<ScopeRef>,
        // LINT.IfChange
        children: HashSet<WeakScopeRef>,
        all_tasks: HashMap<usize, Arc<Task>>,
        // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
        /// Wakers/results for joining each task.
        pub join_results: HashMap<usize, JoinResult>,
        /// The number of children that transitively contain tasks, plus one for
        /// this scope if it directly contains tasks.
        subscopes_with_tasks: u32,
        status: Status,
    }

    pub enum JoinResult {
        Waker(Waker),
        Result(Arc<Task>),
    }

    #[repr(u8)] // So zxdb can read the status.
    #[derive(Default, Debug, Clone, Copy)]
    pub enum Status {
        #[default]
        Open,
        Closed,
        Cancelled,
    }

    impl Status {
        pub fn can_spawn(&self) -> bool {
            match self {
                Status::Open => true,
                Status::Closed | Status::Cancelled => false,
            }
        }
    }

    impl ScopeState {
        pub fn new(parent: Option<ScopeRef>, status: Status) -> Self {
            Self {
                parent,
                children: Default::default(),
                all_tasks: Default::default(),
                join_results: Default::default(),
                subscopes_with_tasks: 0,
                status,
            }
        }

        pub fn all_tasks(&self) -> &HashMap<usize, Arc<Task>> {
            &self.all_tasks
        }

        /// Attempts to add a task to the scope. Returns false if the scope cannot accept a task.
        #[must_use]
        pub fn insert_task(&mut self, id: usize, task: Arc<Task>) -> bool {
            if !self.status.can_spawn() {
                return false;
            }
            if self.all_tasks.is_empty() && !self.register_first_task() {
                return false;
            }
            let existing = self.all_tasks.insert(id, task);
            assert!(existing.is_none());
            true
        }

        pub fn children(&self) -> &HashSet<WeakScopeRef> {
            &self.children
        }

        pub fn insert_child(&mut self, child: WeakScopeRef) {
            self.children.insert(child);
        }

        pub fn remove_child(&mut self, child: &PtrKey) {
            let found = self.children.remove(child);
            // This should always succeed unless the scope is being dropped
            // (in which case children will be empty).
            assert!(found || self.children.is_empty());
        }

        pub fn status(&self) -> Status {
            self.status
        }

        pub fn might_have_running_tasks(&self) -> bool {
            self.status.can_spawn()
        }

        pub fn close(&mut self) {
            self.status = Status::Closed;
        }

        pub fn set_cancelled(&mut self) {
            self.status = Status::Cancelled;
        }

        pub fn has_tasks(&self) -> bool {
            self.subscopes_with_tasks > 0
        }

        pub fn wake_all(&self) {
            for (_, task) in &self.all_tasks {
                task.wake();
            }
        }

        /// Registers our first task with the parent scope.
        ///
        /// Returns false if the scope is not allowed to accept a task.
        #[must_use]
        fn register_first_task(&mut self) -> bool {
            if !self.status.can_spawn() {
                return false;
            }
            let can_spawn = match &self.parent {
                Some(parent) => {
                    // If our parent already knows we have tasks, we can always
                    // spawn. Otherwise, we have to recurse.
                    self.subscopes_with_tasks > 0 || parent.lock().register_first_task()
                }
                None => true,
            };
            if can_spawn {
                self.subscopes_with_tasks += 1;
                debug_assert!(self.subscopes_with_tasks as usize <= self.children.len() + 1);
            };
            can_spawn
        }

        fn on_last_task_removed(
            this: &mut ConditionGuard<'_, ScopeState>,
            num_wakers_hint: usize,
            wakers: &mut Vec<Waker>,
        ) {
            debug_assert!(this.subscopes_with_tasks > 0);
            this.subscopes_with_tasks -= 1;
            if this.subscopes_with_tasks > 0 {
                wakers.reserve(num_wakers_hint);
                return;
            }

            match &this.parent {
                Some(parent) => {
                    Self::on_last_task_removed(
                        &mut parent.lock(),
                        num_wakers_hint + this.waker_count(),
                        wakers,
                    );
                }
                None => wakers.reserve(num_wakers_hint),
            };
            wakers.extend(this.drain_wakers());
        }
    }

    #[derive(Default)]
    struct WakeVec(Vec<Waker>);

    impl Drop for WakeVec {
        fn drop(&mut self) {
            for waker in self.0.drain(..) {
                waker.wake();
            }
        }
    }

    // WakeVec *must* come after the guard because we want the guard to be dropped first.
    pub struct ScopeWaker<'a>(ConditionGuard<'a, ScopeState>, WakeVec);

    impl<'a> From<ConditionGuard<'a, ScopeState>> for ScopeWaker<'a> {
        fn from(value: ConditionGuard<'a, ScopeState>) -> Self {
            Self(value, WakeVec::default())
        }
    }

    impl ScopeWaker<'_> {
        pub fn take_task(&mut self, id: usize) -> Option<Arc<Task>> {
            let task = self.all_tasks.remove(&id);
            if task.is_some() {
                self.on_task_removed(0);
            }
            task
        }

        pub fn task_did_finish(&mut self, id: usize) {
            if let Some(task) = self.all_tasks.remove(&id) {
                self.on_task_removed(1);
                if !task.future.is_detached() {
                    match self.join_results.entry(id) {
                        Entry::Occupied(mut o) => {
                            let JoinResult::Waker(waker) =
                                std::mem::replace(o.get_mut(), JoinResult::Result(task))
                            else {
                                // It can't be JoinResult::Result because this function is the only
                                // function that sets that, and `task_did_finish` won't get called
                                // twice.
                                unreachable!()
                            };
                            self.1 .0.push(waker);
                        }
                        Entry::Vacant(v) => {
                            v.insert(JoinResult::Result(task));
                        }
                    }
                }
            }
        }

        pub fn set_cancelled_and_drain(
            &mut self,
        ) -> (
            HashMap<usize, Arc<Task>>,
            HashMap<usize, JoinResult>,
            hash_set::Drain<'_, WeakScopeRef>,
        ) {
            self.status = Status::Cancelled;
            let all_tasks = std::mem::take(&mut self.all_tasks);
            let join_results = std::mem::take(&mut self.join_results);
            if !all_tasks.is_empty() {
                self.on_task_removed(0)
            }
            let children = self.children.drain();
            (all_tasks, join_results, children)
        }

        fn on_task_removed(&mut self, num_wakers_hint: usize) {
            if self.all_tasks.is_empty() {
                ScopeState::on_last_task_removed(&mut self.0, num_wakers_hint, &mut self.1 .0)
            }
        }
    }

    impl<'a> Deref for ScopeWaker<'a> {
        type Target = ConditionGuard<'a, ScopeState>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl DerefMut for ScopeWaker<'_> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
}

struct ScopeInner {
    executor: Arc<Executor>,
    state: Condition<ScopeState>,
}

impl Drop for ScopeInner {
    fn drop(&mut self) {
        // SAFETY: PtrKey is a ZST so we aren't creating a reference to invalid memory.
        // This also complies with the correctness requirements of
        // HashSet::remove because the implementations of Hash and Eq match
        // between PtrKey and WeakScopeRef.
        let key = unsafe { &*(self as *const _ as *const PtrKey) };
        if let Some(parent) = &self.state.lock().parent {
            let mut parent_state = parent.lock();
            parent_state.remove_child(key);
        }
    }
}

impl ScopeRef {
    fn lock(&self) -> ConditionGuard<'_, ScopeState> {
        self.inner.state.lock()
    }

    fn downgrade(&self) -> WeakScopeRef {
        WeakScopeRef { inner: Arc::downgrade(&self.inner) }
    }

    #[inline(always)]
    pub(crate) fn executor(&self) -> &Arc<Executor> {
        &self.inner.executor
    }

    /// Marks the task as detached.
    pub(crate) fn detach(&self, task_id: usize) {
        let mut state = self.lock();
        if let Some(task) = state.all_tasks().get(&task_id) {
            task.future.detach();
        }
        state.join_results.remove(&task_id);
    }

    /// Cancels the task.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `R` is the correct type.
    pub(crate) unsafe fn cancel_task<R>(&self, task_id: usize) -> Option<R> {
        let mut state = self.lock();
        if let Some(JoinResult::Result(task)) = state.join_results.remove(&task_id) {
            return task.future.take_result();
        }
        state.all_tasks().get(&task_id).and_then(|task| {
            if task.future.cancel() {
                self.inner.executor.ready_tasks.push(task.clone());
            }
            task.future.take_result()
        })
    }

    /// Cancels and detaches the task.
    pub(crate) fn cancel_and_detach(&self, task_id: usize) {
        let mut state = ScopeWaker::from(self.lock());
        state.join_results.remove(&task_id);
        if let Some(task) = state.all_tasks().get(&task_id) {
            match task.future.cancel_and_detach() {
                CancelAndDetachResult::Done => {
                    state.take_task(task_id);
                }
                CancelAndDetachResult::AddToRunQueue => {
                    self.inner.executor.ready_tasks.push(task.clone());
                }
                CancelAndDetachResult::Pending => {}
            }
        };
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
        let mut state = self.lock();
        match state.join_results.entry(task_id) {
            Entry::Occupied(mut o) => match o.get_mut() {
                JoinResult::Waker(waker) => *waker = cx.waker().clone(),
                JoinResult::Result(task) => {
                    if let Some(result) = task.future.take_result() {
                        o.remove();
                        return Poll::Ready(result);
                    }
                    // The task has been cancelled so all we can do is forever return pending.
                }
            },
            Entry::Vacant(v) => {
                v.insert(JoinResult::Waker(cx.waker().clone()));
            }
        }
        Poll::Pending
    }

    /// Polls for the task to be cancelled.
    pub(crate) unsafe fn poll_cancelled<R>(
        &self,
        task_id: usize,
        cx: &mut Context<'_>,
    ) -> Poll<Option<R>> {
        let mut state = self.lock();
        match state.join_results.entry(task_id) {
            Entry::Occupied(mut o) => match o.get_mut() {
                JoinResult::Waker(waker) => *waker = cx.waker().clone(),
                JoinResult::Result(task) => {
                    let result = task.future.take_result();
                    o.remove();
                    return Poll::Ready(result);
                }
            },
            Entry::Vacant(v) => {
                v.insert(JoinResult::Waker(cx.waker().clone()));
            }
        }
        Poll::Pending
    }

    #[must_use]
    pub(super) fn insert_task(&self, id: usize, task: Arc<Task>) -> bool {
        self.lock().insert_task(id, task)
    }

    /// Drops the specified task.
    ///
    /// The main task by the single-threaded executor might not be 'static, so we use this to drop
    /// the task and make sure we meet lifetime guarantees.  Note that removing the task from our
    /// task list isn't sufficient; we must make sure the future running in the task is dropped.
    ///
    /// # Safety
    ///
    /// This is unsafe because of the call to `drop_future_unchecked` which requires that no
    /// thread is currently polling the task.
    pub(super) unsafe fn drop_task_unchecked(&self, task_id: usize) {
        let mut state = ScopeWaker::from(self.lock());
        let task = state.take_task(task_id);
        if let Some(task) = task {
            task.future.drop_future_unchecked();
        }
    }

    pub(super) fn task_did_finish(&self, id: usize) {
        let mut state = ScopeWaker::from(self.lock());
        state.task_did_finish(id);
    }

    /// Cancels tasks in this scope and all child scopes.
    fn cancel_all_tasks(&self) {
        let mut scopes = vec![self.clone()];
        while let Some(scope) = scopes.pop() {
            let mut state = scope.lock();
            if !state.might_have_running_tasks() {
                // Already cancelled or closed.
                continue;
            }
            for task in state.all_tasks().values() {
                if task.future.cancel() {
                    task.scope.executor().ready_tasks.push(task.clone());
                }
                // Don't bother dropping tasks that are finished; the entire
                // scope is going to be dropped soon anyway.
            }
            // Copy children to a vec so we don't hold the lock for too long.
            scopes.extend(state.children().iter().filter_map(|child| child.upgrade()));
            state.set_cancelled();
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
            let (tasks, join_results) = {
                let mut state = ScopeWaker::from(scope.lock());
                let (tasks, join_results, children) = state.set_cancelled_and_drain();
                scopes.extend(children.filter_map(|child| child.upgrade()));
                (tasks, join_results)
            };
            // Call task destructors once the scope lock is released so we don't risk a deadlock.
            for (_id, task) in tasks {
                task.future.try_drop().expect("Expected drop to succeed");
            }
            std::mem::drop(join_results);
        }
    }
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
    use crate::{EHandle, LocalExecutor, SendExecutor, Task, TestExecutor, Timer};
    use assert_matches::assert_matches;
    use fuchsia_sync::{Condvar, Mutex};
    use futures::future::join_all;
    use futures::FutureExt;
    use std::future::{pending, poll_fn};
    use std::pin::{pin, Pin};
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};

    #[derive(Default)]
    struct RemoteControlFuture(Mutex<RCFState>);
    #[derive(Default)]
    struct RCFState {
        resolved: bool,
        waker: Option<Waker>,
    }

    impl Future for &RemoteControlFuture {
        type Output = ();
        fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut this = self.0.lock();
            if this.resolved {
                Poll::Ready(())
            } else {
                this.waker.replace(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    impl RemoteControlFuture {
        fn new() -> Arc<Self> {
            Arc::new(Default::default())
        }

        fn resolve(&self) {
            let mut this = self.0.lock();
            this.resolved = true;
            if let Some(waker) = this.waker.take() {
                waker.wake();
            }
        }

        fn as_future(self: &Arc<Self>) -> impl Future<Output = ()> {
            let this = Arc::clone(self);
            async move { (&*this).await }
        }
    }

    #[test]
    fn compute_works_on_root_scope() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope();
        let mut task = pin!(scope.compute(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(1));
    }

    #[test]
    fn compute_works_on_new_child() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let mut task = pin!(scope.compute(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(1));
    }

    #[test]
    fn scope_drop_cancels_tasks() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let mut task = pin!(scope.compute(async { 1 }));
        drop(scope);
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Pending);
    }

    #[test]
    fn tasks_do_not_spawn_on_cancelled_scopes() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        assert_eq!(executor.run_until_stalled(&mut pin!(scope.cancel())), Poll::Ready(()));
        let mut task = pin!(scope_ref.compute(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Pending);
    }

    #[test]
    fn spawn_works_on_child_and_grandchild() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        let grandchild = child.new_child();
        let mut child_task = pin!(child.compute(async { 1 }));
        let mut grandchild_task = pin!(grandchild.compute(async { 1 }));
        assert_eq!(executor.run_until_stalled(&mut child_task), Poll::Ready(1));
        assert_eq!(executor.run_until_stalled(&mut grandchild_task), Poll::Ready(1));
    }

    #[test]
    fn spawn_drop_cancels_child_and_grandchild_tasks() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        let grandchild = child.new_child();
        let mut child_task = pin!(child.compute(async { 1 }));
        let mut grandchild_task = pin!(grandchild.compute(async { 1 }));
        drop(scope);
        assert_eq!(executor.run_until_stalled(&mut child_task), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut grandchild_task), Poll::Pending);
    }

    #[test]
    fn completed_tasks_are_cleaned_up_after_cancel() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();

        let task1 = scope.spawn(pending::<()>());
        let task2 = scope.spawn(async {});
        assert_eq!(executor.run_until_stalled(&mut pending::<()>()), Poll::Pending);
        assert_eq!(scope.lock().all_tasks().len(), 1);

        // Running the executor after cancelling the task isn't currently
        // necessary, but we might decide to do async cleanup in the future.
        assert_eq!(task1.cancel().now_or_never(), None);
        assert_eq!(task2.cancel().now_or_never(), Some(Some(())));

        assert_eq!(executor.run_until_stalled(&mut pending::<()>()), Poll::Pending);
        assert_eq!(scope.lock().all_tasks().len(), 0);
        assert_eq!(scope.lock().join_results.len(), 0);
    }

    #[test]
    fn join_emtpy_scope() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        assert_eq!(executor.run_until_stalled(&mut pin!(scope.join())), Poll::Ready(()));
    }

    #[test]
    fn task_handle_preserves_access_to_result_after_join_begins() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let mut task = scope.compute(async { 1 });
        scope.spawn(async {});
        let task2 = scope.spawn(pending::<()>());
        // Fuse to stay agnostic as to whether the join completes before or
        // after awaiting the task handle.
        let mut join = pin!(scope.join().fuse());
        let _ = executor.run_until_stalled(&mut join);
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(1));
        let _ = task2.cancel();
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Ready(()));
    }

    #[test]
    fn join_blocks_until_task_is_cancelled() {
        // Scope with one outstanding task handle and one cancelled task.
        // The scope is not complete until the outstanding task handle is cancelled.
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let outstanding_task = scope.spawn(pending::<()>());
        let cancelled_task = scope.spawn(pending::<()>());
        assert_eq!(
            executor.run_until_stalled(&mut pin!(cancelled_task.cancel())),
            Poll::Ready(None)
        );
        let mut join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Pending);
        assert_eq!(
            executor.run_until_stalled(&mut pin!(outstanding_task.cancel())),
            Poll::Ready(None)
        );
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Ready(()));
    }

    #[test]
    fn join_blocks_if_detached_task_never_completes() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        // The default is to detach.
        scope.spawn(pending::<()>());
        let mut join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Pending);
    }

    #[test]
    fn join_scope_blocks_until_spawned_task_completes() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let remote = RemoteControlFuture::new();
        let mut task = scope.spawn(remote.as_future());
        let mut scope_join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
        remote.resolve();
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Ready(()));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(()));
    }

    #[test]
    fn join_scope_blocks_until_detached_task_of_detached_child_completes() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        let remote = RemoteControlFuture::new();
        child.spawn(remote.as_future());
        let mut scope_join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut pin!(child.on_no_tasks())), Poll::Pending);
        child.detach();
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
        remote.resolve();
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Ready(()));
    }

    #[test]
    fn join_scope_blocks_when_blocked_child_is_detached() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        child.spawn(pending());
        let mut scope_join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut pin!(child.on_no_tasks())), Poll::Pending);
        child.detach();
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
    }

    #[test]
    fn join_scope_completes_when_blocked_child_is_cancelled() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        child.spawn(pending());
        let mut scope_join = pin!(scope.join());
        {
            let mut child_join = pin!(child.join());
            assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
            assert_eq!(executor.run_until_stalled(&mut child_join), Poll::Pending);
        }
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Ready(()));
    }

    #[test]
    fn detached_scope_can_spawn() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        scope.detach();
        assert_eq!(executor.run_until_stalled(&mut scope_ref.compute(async { 1 })), Poll::Ready(1));
    }

    #[test]
    fn dropped_scope_cannot_spawn() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        drop(scope);
        assert_eq!(executor.run_until_stalled(&mut scope_ref.compute(async { 1 })), Poll::Pending);
    }

    #[test]
    fn dropped_scope_with_running_task_cannot_spawn() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        let _running_task = scope_ref.spawn(pending::<()>());
        drop(scope);
        assert_eq!(executor.run_until_stalled(&mut scope_ref.compute(async { 1 })), Poll::Pending);
    }

    #[test]
    fn joined_scope_cannot_spawn() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        let mut scope_join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Ready(()));
        assert_eq!(executor.run_until_stalled(&mut scope_ref.compute(async { 1 })), Poll::Pending);
    }

    #[test]
    fn joining_scope_with_running_task_can_spawn() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        let _running_task = scope_ref.spawn(pending::<()>());
        let mut scope_join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut scope_ref.compute(async { 1 })), Poll::Ready(1));
    }

    #[test]
    fn joined_scope_child_cannot_spawn() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        let child_before_join = scope.new_child();
        assert_eq!(
            executor.run_until_stalled(&mut child_before_join.compute(async { 1 })),
            Poll::Ready(1)
        );
        let mut scope_join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut scope_join), Poll::Ready(()));
        let child_after_join = scope_ref.new_child();
        let grandchild_after_join = child_before_join.new_child();
        assert_eq!(
            executor.run_until_stalled(&mut child_before_join.compute(async { 1 })),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut child_after_join.compute(async { 1 })),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut grandchild_after_join.compute(async { 1 })),
            Poll::Pending
        );
    }

    #[test]
    fn can_join_child_first() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        assert_eq!(executor.run_until_stalled(&mut child.compute(async { 1 })), Poll::Ready(1));
        assert_eq!(executor.run_until_stalled(&mut pin!(child.join())), Poll::Ready(()));
        assert_eq!(executor.run_until_stalled(&mut pin!(scope.join())), Poll::Ready(()));
    }

    #[test]
    fn can_join_parent_first() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        assert_eq!(executor.run_until_stalled(&mut child.compute(async { 1 })), Poll::Ready(1));
        assert_eq!(executor.run_until_stalled(&mut pin!(scope.join())), Poll::Ready(()));
        assert_eq!(executor.run_until_stalled(&mut pin!(child.join())), Poll::Ready(()));
    }

    #[test]
    fn task_in_parent_scope_can_join_child() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let child = scope.new_child();
        let remote = RemoteControlFuture::new();
        child.spawn(remote.as_future());
        scope.spawn(async move { child.join().await });
        let mut join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Pending);
        remote.resolve();
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Ready(()));
    }

    #[test]
    fn join_completes_while_completed_task_handle_is_held() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let mut task = scope.compute(async { 1 });
        scope.spawn(async {});
        let mut join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Ready(()));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Ready(1));
    }

    #[test]
    fn cancel_completes_while_task_holds_scope_ref() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let scope_ref = scope.make_ref();
        let mut task = scope.compute(async move {
            loop {
                pending::<()>().await; // never returns
                scope_ref.spawn(async {});
            }
        });

        // Join should not complete because the task never does.
        let mut join = pin!(scope.join());
        assert_eq!(executor.run_until_stalled(&mut join), Poll::Pending);

        let mut cancel = pin!(join.cancel());
        assert_eq!(executor.run_until_stalled(&mut cancel), Poll::Ready(()));
        assert_eq!(executor.run_until_stalled(&mut task), Poll::Pending);
    }

    #[test]
    fn cancel_from_scope_ref_inside_task() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        {
            // Spawn a task that never finishes until the scope is cancelled.
            scope.spawn(pending::<()>());

            let mut no_tasks = pin!(scope.on_no_tasks());
            assert_eq!(executor.run_until_stalled(&mut no_tasks), Poll::Pending);

            let scope_ref = scope.make_ref();
            scope.spawn(async move {
                scope_ref.cancel().await;
                panic!("cancel() should never complete");
            });

            assert_eq!(executor.run_until_stalled(&mut no_tasks), Poll::Ready(()));
        }
        assert_eq!(scope.join().now_or_never(), Some(()));
    }

    #[test]
    fn can_spawn_from_non_executor_thread() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().clone();
        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();
        let _ = std::thread::spawn(move || {
            scope.spawn(async move {
                done_clone.store(true, Ordering::Relaxed);
            })
        })
        .join();
        let _ = executor.run_until_stalled(&mut pending::<()>());
        assert!(done.load(Ordering::Relaxed));
    }

    #[test]
    fn scope_tree() {
        // A
        //  \
        //   B
        //  / \
        // C   D
        let mut executor = TestExecutor::new();
        let a = executor.root_scope().new_child();
        let b = a.new_child();
        let c = b.new_child();
        let d = b.new_child();
        let a_remote = RemoteControlFuture::new();
        let c_remote = RemoteControlFuture::new();
        let d_remote = RemoteControlFuture::new();
        a.spawn(a_remote.as_future());
        c.spawn(c_remote.as_future());
        d.spawn(d_remote.as_future());
        let mut a_join = pin!(a.join());
        let mut b_join = pin!(b.join());
        let mut d_join = pin!(d.join());
        assert_eq!(executor.run_until_stalled(&mut a_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut b_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut d_join), Poll::Pending);
        d_remote.resolve();
        assert_eq!(executor.run_until_stalled(&mut a_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut b_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut d_join), Poll::Ready(()));
        c_remote.resolve();
        assert_eq!(executor.run_until_stalled(&mut a_join), Poll::Pending);
        assert_eq!(executor.run_until_stalled(&mut b_join), Poll::Ready(()));
        a_remote.resolve();
        assert_eq!(executor.run_until_stalled(&mut a_join), Poll::Ready(()));
        let mut c_join = pin!(c.join());
        assert_eq!(executor.run_until_stalled(&mut c_join), Poll::Ready(()));
    }

    #[test]
    fn on_no_tasks() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();
        let _task1 = scope.spawn(std::future::ready(()));
        let task2 = scope.spawn(pending::<()>());

        let mut on_no_tasks = pin!(scope.on_no_tasks());

        assert!(executor.run_until_stalled(&mut on_no_tasks).is_pending());

        let _ = task2.cancel();

        let on_no_tasks2 = pin!(scope.on_no_tasks());
        let on_no_tasks3 = pin!(scope.on_no_tasks());

        assert_matches!(
            executor.run_until_stalled(&mut join_all([on_no_tasks, on_no_tasks2, on_no_tasks3])),
            Poll::Ready(_)
        );
    }

    #[test]
    fn wake_all() {
        let mut executor = TestExecutor::new();
        let scope = executor.root_scope().new_child();

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

        let _ = executor.run_until_stalled(&mut pending::<()>());

        let mut start_count = poll_count.load(Ordering::Relaxed);

        for _ in 0..2 {
            scope.wake_all();
            let _ = executor.run_until_stalled(&mut pending::<()>());
            assert_eq!(poll_count.load(Ordering::Relaxed), start_count + 2);
            start_count += 2;
        }
    }

    #[test]
    fn on_no_tasks_race() {
        fn sleep_random() {
            use rand::Rng;
            std::thread::sleep(std::time::Duration::from_micros(
                rand::thread_rng().gen_range(0..10),
            ));
        }
        for _ in 0..2000 {
            let mut executor = SendExecutor::new(2);
            let scope = executor.root_scope().new_child();
            scope.spawn(async {
                sleep_random();
            });
            executor.run(async move {
                sleep_random();
                scope.on_no_tasks().await;
            });
        }
    }

    async fn yield_to_executor() {
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

    #[test]
    fn test_detach() {
        let mut e = LocalExecutor::new();
        e.run_singlethreaded(async {
            let counter = Arc::new(AtomicU32::new(0));

            {
                let counter = counter.clone();
                Task::spawn(async move {
                    for _ in 0..5 {
                        yield_to_executor().await;
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .detach();
            }

            while counter.load(Ordering::Relaxed) != 5 {
                yield_to_executor().await;
            }
        });

        assert!(e.ehandle.root_scope.lock().join_results.is_empty());
    }

    #[test]
    fn test_cancel() {
        let mut e = LocalExecutor::new();
        e.run_singlethreaded(async {
            let ref_count = Arc::new(());
            // First, just drop the task.
            {
                let ref_count = ref_count.clone();
                let _ = Task::spawn(async move {
                    let _ref_count = ref_count;
                    let _: () = std::future::pending().await;
                });
            }

            while Arc::strong_count(&ref_count) != 1 {
                yield_to_executor().await;
            }

            // Now try explicitly cancelling.
            let task = {
                let ref_count = ref_count.clone();
                Task::spawn(async move {
                    let _ref_count = ref_count;
                    let _: () = std::future::pending().await;
                })
            };

            assert_eq!(task.cancel().await, None);
            while Arc::strong_count(&ref_count) != 1 {
                yield_to_executor().await;
            }

            // Now cancel a task that has already finished.
            let task = {
                let ref_count = ref_count.clone();
                Task::spawn(async move {
                    let _ref_count = ref_count;
                })
            };

            // Wait for it to finish.
            while Arc::strong_count(&ref_count) != 1 {
                yield_to_executor().await;
            }

            assert_eq!(task.cancel().await, Some(()));
        });

        assert!(e.ehandle.root_scope.lock().join_results.is_empty());
    }

    #[test]
    fn test_cancel_waits() {
        let mut executor = SendExecutor::new(2);
        let running = Arc::new((Mutex::new(false), Condvar::new()));
        let task = {
            let running = running.clone();
            executor.root_scope().compute(async move {
                *running.0.lock() = true;
                running.1.notify_all();
                std::thread::sleep(std::time::Duration::from_millis(10));
                *running.0.lock() = false;
                "foo"
            })
        };
        executor.run(async move {
            {
                let mut guard = running.0.lock();
                while !*guard {
                    running.1.wait(&mut guard);
                }
            }
            assert_eq!(task.cancel().await, Some("foo"));
            assert!(!*running.0.lock());
        });
    }

    fn test_clean_up(callback: impl FnOnce(Task<()>) + Send + 'static) {
        let mut executor = SendExecutor::new(2);
        let running = Arc::new((Mutex::new(false), Condvar::new()));
        let can_quit = Arc::new((Mutex::new(false), Condvar::new()));
        let task = {
            let running = running.clone();
            let can_quit = can_quit.clone();
            executor.root_scope().compute(async move {
                *running.0.lock() = true;
                running.1.notify_all();
                {
                    let mut guard = can_quit.0.lock();
                    while !*guard {
                        can_quit.1.wait(&mut guard);
                    }
                }
                *running.0.lock() = false;
            })
        };
        executor.run(async move {
            {
                let mut guard = running.0.lock();
                while !*guard {
                    running.1.wait(&mut guard);
                }
            }

            callback(task);

            *can_quit.0.lock() = true;
            can_quit.1.notify_all();

            let ehandle = EHandle::local();
            let scope = ehandle.root_scope();

            // The only way of testing for this is to poll.
            while scope.lock().all_tasks().len() > 1 || scope.lock().join_results.len() > 0 {
                Timer::new(std::time::Duration::from_millis(1)).await;
            }

            assert!(!*running.0.lock());
        });
    }

    #[test]
    fn test_dropped_cancel_cleans_up() {
        test_clean_up(|task| {
            let cancel_fut = std::pin::pin!(task.cancel());
            let waker = futures::task::noop_waker();
            assert!(cancel_fut.poll(&mut Context::from_waker(&waker)).is_pending());
        });
    }

    #[test]
    fn test_dropped_task_cleans_up() {
        test_clean_up(|task| {
            std::mem::drop(task);
        });
    }

    #[test]
    fn test_detach_cleans_up() {
        test_clean_up(|task| {
            task.detach();
        });
    }
}
