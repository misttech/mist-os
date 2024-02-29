// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Core traits and types that support running asynchronous objects (streams,
//! tasks, etc.) until stalled.

use fuchsia_async as fasync;
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot::{self, Receiver, Sender},
    Future,
};
use std::{
    marker::PhantomData,
    sync::{Arc, Weak},
    task::{Context, Waker},
};
use zx::Duration;

/// Types that implement [`Unbind`] hold the resources behind a stream or a future,
/// and may be unbound into some data and handle that can then be escrowed.
pub trait Unbind: Send + 'static {
    /// The result of unbinding.
    ///
    /// Typically, these are data and resources that can be escrowed by
    /// `component_manager` and passed back to the component when some conditions
    /// (such as readable signal) are satisfied.
    type Item: Send;

    /// Convert the value into an [`Item`].
    fn unbind(self) -> Self::Item;
}

/// A handle used by an asynchronous task, which can be a stream or a future or
/// other things, to report its progression status. [`Progress`] is returned
/// when creating a [`StallDetector`].
///
/// [`State`] is some task specific state that supports escrowing.
///
/// The ownership of [`State`] will ping-pong between the task and the
/// [`StallDetector`]. [`StallDetector`] may escrow away the [`State`] state,
/// and the task will realize that the next time it's polled. See the methods
/// on [`Progress`] for details.
pub struct Progress<State>
where
    State: Unbind,
{
    weak: WeakHandle<State>,
    _inner: PhantomData<State>,
}

impl<State> Progress<State>
where
    State: Unbind,
{
    /// Indicate that the task is stalled, and surrender the state.
    /// Later, if the corresponding future or stream is polled again,
    /// `resume` should be called to retrieve the state.
    ///
    /// If the state is unbound, the task will be woken using a waker
    /// obtained from `cx`.
    pub fn stalled(&self, state: State, cx: &mut Context<'_>) {
        self.weak.stalled(state, cx);
    }

    /// Attempt to resume the task. If the state has been unbound, this will
    /// return `None`. The async stream or future should then immediately
    /// complete because its task specific state has been consumed.
    pub fn resume(&self) -> Option<State> {
        self.weak.resume()
    }
}

impl<State> Clone for Progress<State>
where
    State: Unbind,
{
    fn clone(&self) -> Self {
        Self { weak: self.weak.clone(), _inner: PhantomData::<State> {} }
    }
}

impl<State> Drop for Progress<State>
where
    State: Unbind,
{
    /// The task is abandoned or completed, and will never make any more progress.
    /// This is such that completed futures and streams will be not longer considered ready.
    fn drop(&mut self) {
        self.weak.complete()
    }
}

/// A weak reference to a [`StallDetector`].
struct WeakHandle<State>
where
    State: Unbind,
{
    inner: Weak<Mutex<Inner<State>>>,
}

impl<State> Clone for WeakHandle<State>
where
    State: Unbind,
{
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<State> WeakHandle<State>
where
    State: Unbind,
{
    fn stalled(&self, state: State, cx: &mut Context<'_>) {
        let Some(inner) = self.inner.upgrade() else { return };
        inner.lock().stalled(state, self.clone(), cx);
    }

    fn resume(&self) -> Option<State> {
        let Some(inner) = self.inner.upgrade() else { return None };
        let option = inner.lock().resume(self.clone());
        option
    }

    fn complete(&self) {
        let Some(inner) = self.inner.upgrade() else { return };
        inner.lock().complete(self.clone());
    }

    fn timer_elapsed(&self) {
        let Some(inner) = self.inner.upgrade() else { return };
        inner.lock().timer_elapsed();
    }
}

/// [`StallDetector`] tracks a task until it has stalled or completed.
pub struct StallDetector<State>
where
    State: Unbind,
{
    strong: Arc<Mutex<Inner<State>>>,
    receiver: Receiver<Option<<State as Unbind>::Item>>,
}

impl<State> StallDetector<State>
where
    State: Unbind,
{
    /// Creates a [`StallDetector`] and a handle for reporting the progression
    /// of an async task.
    pub fn new(debounce_interval: Duration) -> (Self, Progress<State>) {
        let (sender, receiver) = oneshot::channel();
        let (strong, progress) = Inner::new(debounce_interval, sender);
        (StallDetector { strong, receiver }, progress)
    }

    /// [`wait`] waits until the task has stalled or completed. If the task has
    /// stalled, it additionally runs a timer with duration `debounce_interval`.
    /// If the task still remains stalled after the timer completes, it will
    /// unbind the state out of the task and return it. If the task completed,
    /// it will return `None`.
    pub fn wait(self) -> impl Future<Output = Option<<State as Unbind>::Item>> {
        async move {
            let _strong = self.strong;
            self.receiver.await.unwrap()
        }
    }
}

/// The protected innards of [`StallDetector`].
struct Inner<State>
where
    State: Unbind,
{
    debounce_interval: Duration,
    ready: bool,
    task_state: Option<State>,
    task_waker: Option<Waker>,
    idle_timer: Option<fasync::Task<()>>,
    notify_stalled: Option<Sender<Option<<State as Unbind>::Item>>>,
}

impl<State> Inner<State>
where
    State: Unbind,
{
    fn new(
        debounce_interval: Duration,
        notify_stalled: Sender<Option<<State as Unbind>::Item>>,
    ) -> (Arc<Mutex<Self>>, Progress<State>) {
        let inner = Self {
            debounce_interval,
            ready: false,
            task_state: None,
            task_waker: None,
            idle_timer: None,
            notify_stalled: Some(notify_stalled),
        };
        let strong = Arc::new(Mutex::new(inner));
        let weak = WeakHandle { inner: Arc::downgrade(&strong) };
        strong.lock().ready(weak.clone());
        let progress = Progress { weak, _inner: PhantomData::<State> {} };
        (strong, progress)
    }

    fn ready(&mut self, handle: WeakHandle<State>) {
        if self.unbound() {
            return;
        }
        assert!(!self.ready);
        self.ready = true;
        self.check_all_stalled(handle);
    }

    fn stalled(&mut self, state: State, handle: WeakHandle<State>, cx: &mut Context<'_>) {
        if self.unbound() {
            return;
        }
        assert!(self.ready);
        self.ready = false;
        self.task_state = Some(state);
        self.task_waker = Some(cx.waker().clone());
        self.check_all_stalled(handle);
    }

    fn resume(&mut self, handle: WeakHandle<State>) -> Option<State> {
        // If timer already elapsed, never resume. We are shutting down.
        if self.unbound() {
            None
        } else {
            self.ready(handle.clone());
            let escrow = self.task_state.take().unwrap();
            self.check_all_stalled(handle);
            Some(escrow)
        }
    }

    fn complete(&mut self, handle: WeakHandle<State>) {
        if self.unbound() {
            return;
        }
        self.ready = false;
        self.check_all_stalled(handle);
    }

    fn check_all_stalled(&mut self, handle: WeakHandle<State>) {
        if self.unbound() {
            return;
        }
        if !self.ready {
            // Start a timer to wait for continued idling.
            let debounce_interval = self.debounce_interval;
            self.idle_timer = Some(fasync::Task::spawn(async move {
                fasync::Timer::new(debounce_interval).await;
                handle.timer_elapsed();
            }));
        } else {
            // Clear the timer if any.
            self.idle_timer = None;
        }
    }

    fn timer_elapsed(&mut self) {
        if self.unbound() {
            return;
        }
        if !self.ready {
            let item = std::mem::replace(&mut self.task_state, None).map(|unbind| unbind.unbind());
            _ = self.notify_stalled.take().unwrap().send(item);
            // Wake the task, which will usually notice that the state is gone
            // when it calls `progress.resume()`, and will then complete.
            self.task_waker.take().map(|waker| waker.wake());
        }
    }

    fn unbound(&self) -> bool {
        self.notify_stalled.is_none()
    }
}
