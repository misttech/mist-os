// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types used to coordinate shutdown with kernel threads.

use crate::task::Task;
use futures::future::{select, Either};
use futures::pin_mut;
use starnix_logging::log_trace;
use starnix_sync::Mutex;
use starnix_types::ownership::{OwnedRef, WeakRef};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

#[derive(Clone, Debug)]
pub struct OnShutdown(Arc<Mutex<ShutdownState>>);

impl OnShutdown {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Default::default())))
    }

    /// Notify all registrations that shutdown is happening.
    pub fn notify(&self) {
        self.0.lock().notify();
    }

    /// Returns a future that will drive the provided future either to completion or until shutdown.
    ///
    /// Returned future yields `Some(...)` if the future completes before shutdown, yields None
    /// otherwise.
    pub fn wrap_future<'a, F: Future<Output = T> + 'a, T>(
        &self,
        f: F,
    ) -> impl Future<Output = Option<T>> + 'a {
        let (send, recv) = futures::channel::oneshot::channel();
        let key = GuardKey::next();
        log_trace!(key:?; "registering async channel for message on shutdown");
        self.0.lock().async_channels.insert(key, send);
        let registration = OnShutdownFutRegistration { key, state: Arc::downgrade(&self.0) };
        async move {
            // Need to keep this alive while the future is still being polled.
            let _registration = registration;
            pin_mut!(recv);
            pin_mut!(f);
            match select(recv, f).await {
                Either::Left(_shutdown) => None,
                Either::Right((ret, _)) => Some(ret),
            }
        }
    }

    /// Returns a channel that wraps the provided channel and cancels usage if shutdown is pending.
    pub fn wrap_channel<T>(
        &self,
        inner: crossbeam_channel::Receiver<T>,
    ) -> OnShutdownReceiverWrapper<T> {
        log_trace!("registering sync channel receiver for message on shutdown");
        let mut state = self.0.lock();
        state.sync_count += 1;
        OnShutdownReceiverWrapper {
            state: Arc::downgrade(&self.0),
            inner,
            on_shutdown: state.sync_recv.clone(),
        }
    }

    /// Returns a channel that wraps the provided channel and cancels usage if shutdown is pending.
    pub fn wrap_channel_sender<T>(
        &self,
        inner: crossbeam_channel::Sender<T>,
    ) -> OnShutdownSenderWrapper<T> {
        log_trace!("registering sync channel sender for message on shutdown");
        let mut state = self.0.lock();
        state.sync_count += 1;
        OnShutdownSenderWrapper {
            state: Arc::downgrade(&self.0),
            inner,
            on_shutdown: state.sync_recv.clone(),
        }
    }

    /// Register a task with shutdown so that any blocking waits on the task are interrupted
    /// when shutdown begins.
    ///
    /// Most code should not need this as it is automatically performed by the thread spawner.
    pub fn register_task(&self, task: &OwnedRef<Task>) -> OnShutdownTaskGuard {
        let key = GuardKey::next();
        log_trace!(key:?; "registering task for interrupt on shutdown");
        self.0.lock().tasks.insert(key, OwnedRef::downgrade(task));
        OnShutdownTaskGuard { key, state: Arc::downgrade(&self.0) }
    }

    /// Register a `port` with shutdown so that it will be sent `shutdown_packet` when kernel
    /// threads should shut down.
    pub fn register_port_for_user_packet(
        &self,
        port: &Arc<zx::Port>,
        shutdown_packet: zx::UserPacket,
    ) -> OnShutdownPortGuard {
        let key = GuardKey::next();
        log_trace!(key:?; "registering port for user packet on shutdown");
        self.0.lock().ports.insert(key, (Arc::downgrade(port), shutdown_packet));
        OnShutdownPortGuard { key, state: Arc::downgrade(&self.0) }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GuardKey(u64);

impl GuardKey {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

struct OnShutdownFutRegistration {
    key: GuardKey,
    state: Weak<Mutex<ShutdownState>>,
}

impl Drop for OnShutdownFutRegistration {
    fn drop(&mut self) {
        log_trace!(key:? = self.key; "unregistering async channel shutdown guard");
        if let Some(state) = self.state.upgrade() {
            state.lock().async_channels.remove(&self.key);
        }
    }
}

pub struct OnShutdownReceiverWrapper<T> {
    inner: crossbeam_channel::Receiver<T>,
    on_shutdown: crossbeam_channel::Receiver<()>,
    state: Weak<Mutex<ShutdownState>>,
}

impl<T> OnShutdownReceiverWrapper<T> {
    pub fn recv(&self) -> Option<Result<T, crossbeam_channel::RecvError>> {
        crossbeam_channel::select! {
            recv(self.inner) -> res => Some(res),
            recv(self.on_shutdown) -> _ => None,
        }
    }
}

impl<T> Drop for OnShutdownReceiverWrapper<T> {
    fn drop(&mut self) {
        log_trace!("decrementing sync channel receiver shutdown count");
        if let Some(state) = self.state.upgrade() {
            state.lock().sync_count -= 1;
        }
    }
}

pub struct OnShutdownSenderWrapper<T> {
    inner: crossbeam_channel::Sender<T>,
    on_shutdown: crossbeam_channel::Receiver<()>,
    state: Weak<Mutex<ShutdownState>>,
}

impl<T> OnShutdownSenderWrapper<T> {
    pub fn send(&self, val: T) -> Option<Result<(), crossbeam_channel::SendError<T>>> {
        crossbeam_channel::select! {
            send(self.inner, val) -> res => Some(res),
            recv(self.on_shutdown) -> _ => None,
        }
    }
}

impl<T> Drop for OnShutdownSenderWrapper<T> {
    fn drop(&mut self) {
        log_trace!("decrementing sync channel sender shutdown count");
        if let Some(state) = self.state.upgrade() {
            state.lock().sync_count -= 1;
        }
    }
}

#[must_use = "must be kept alive to stay registered for shutdown"]
pub struct OnShutdownTaskGuard {
    key: GuardKey,
    state: Weak<Mutex<ShutdownState>>,
}

impl Drop for OnShutdownTaskGuard {
    fn drop(&mut self) {
        log_trace!(key:? = self.key; "unregistering task shutdown guard");
        if let Some(state) = self.state.upgrade() {
            state.lock().tasks.remove(&self.key);
        }
    }
}

#[must_use = "must be kept alive to stay registered for shutdown"]
pub struct OnShutdownPortGuard {
    key: GuardKey,
    state: Weak<Mutex<ShutdownState>>,
}

impl Drop for OnShutdownPortGuard {
    fn drop(&mut self) {
        log_trace!(key:? = self.key; "unregistering port shutdown guard");
        if let Some(state) = self.state.upgrade() {
            state.lock().ports.remove(&self.key);
        }
    }
}

#[derive(Debug)]
pub struct ShutdownState {
    sync_send: crossbeam_channel::Sender<()>,
    sync_recv: crossbeam_channel::Receiver<()>,
    sync_count: usize,
    async_channels: HashMap<GuardKey, futures::channel::oneshot::Sender<()>>,
    tasks: HashMap<GuardKey, WeakRef<Task>>,
    ports: HashMap<GuardKey, (Weak<zx::Port>, zx::UserPacket)>,
}

impl Default for ShutdownState {
    fn default() -> Self {
        let (sync_send, sync_recv) = crossbeam_channel::unbounded();
        Self {
            sync_send,
            sync_recv,
            sync_count: 0,
            async_channels: Default::default(),
            tasks: Default::default(),
            ports: Default::default(),
        }
    }
}

impl ShutdownState {
    fn notify(&mut self) {
        for (key, async_channel) in std::mem::take(&mut self.async_channels) {
            log_trace!(key:?; "notifying async channel shutdown is underway");
            async_channel.send(()).ok();
        }
        for _ in 0..self.sync_count {
            log_trace!("notifying sync channel that shutdown is underway");
            self.sync_send.send(()).ok();
        }
        for (key, task) in std::mem::take(&mut self.tasks) {
            log_trace!(key:?; "signaling task that shutdown is underway");
            if let Some(task) = task.upgrade() {
                task.interrupt();
            }
        }
        for (key, (port, packet)) in std::mem::take(&mut self.ports) {
            log_trace!(key:?; "queueing user packet to indicate shutdown is underway");
            if let Some(port) = port.upgrade() {
                port.queue(&zx::Packet::from_user_packet(0, 0, packet)).ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_thread_spawner::DynamicThreadSpawner;
    use crate::testing::{create_kernel_and_task, AutoReleasableTask};
    use starnix_logging::log_debug;
    use starnix_sync::InterruptibleEvent;

    fn build_spawner(max_idle_threads: u8) -> (AutoReleasableTask, DynamicThreadSpawner) {
        let (kernel, task) = create_kernel_and_task();
        let spawner = DynamicThreadSpawner::new(
            max_idle_threads,
            task.weak_task(),
            kernel.on_shutdown.clone(),
        );
        (task, spawner)
    }

    #[fuchsia::test]
    async fn fut_guard_cleans_up() {
        let on_shutdown = OnShutdown(Default::default());
        assert_eq!(on_shutdown.0.lock().async_channels.len(), 0);

        let fut = on_shutdown.wrap_future(futures::future::pending::<()>());
        assert_eq!(on_shutdown.0.lock().async_channels.len(), 1);

        drop(fut);
        assert_eq!(on_shutdown.0.lock().async_channels.len(), 0);
    }

    #[fuchsia::test]
    fn channel_guard_cleans_up() {
        let on_shutdown = OnShutdown(Default::default());
        assert_eq!(on_shutdown.0.lock().sync_count, 0);

        let (send, recv) = crossbeam_channel::unbounded::<()>();
        let recv = on_shutdown.wrap_channel(recv);
        assert_eq!(on_shutdown.0.lock().sync_count, 1);
        let send = on_shutdown.wrap_channel_sender(send);
        assert_eq!(on_shutdown.0.lock().sync_count, 2);

        drop(recv);
        assert_eq!(on_shutdown.0.lock().sync_count, 1);
        drop(send);
        assert_eq!(on_shutdown.0.lock().sync_count, 0);
    }

    #[fuchsia::test]
    fn guard_keys_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10000 {
            assert!(seen.insert(GuardKey::next()));
        }
    }

    #[fuchsia::test]
    async fn shut_down_sync_receiver() {
        let (_task, spawner) = build_spawner(2);
        let (_never_send, never_recv) = crossbeam_channel::unbounded::<()>();
        let (before_shutdown_send, before_shutdown_recv) = crossbeam_channel::unbounded();
        spawner.spawn(move |_, current_task| {
            let on_shutdown = current_task.kernel().on_shutdown.wrap_channel(never_recv);
            before_shutdown_send.send(()).unwrap();
            log_debug!("waiting for shutdown channel");
            assert_eq!(on_shutdown.recv(), None);
        });
        before_shutdown_recv.recv().unwrap();
        spawner.shut_down();
    }

    #[fuchsia::test]
    async fn shut_down_sync_sender() {
        let (_task, spawner) = build_spawner(2);
        let (try_never_send, _never_recv) = crossbeam_channel::bounded::<()>(0);
        let (before_shutdown_send, before_shutdown_recv) = crossbeam_channel::unbounded();
        spawner.spawn(move |_, current_task| {
            let until_shutdown =
                current_task.kernel().on_shutdown.wrap_channel_sender(try_never_send);
            before_shutdown_send.send(()).unwrap();
            log_debug!("waiting for shutdown channel");
            assert_eq!(until_shutdown.send(()), None);
        });
        before_shutdown_recv.recv().unwrap();
        spawner.shut_down();
    }

    #[fuchsia::test]
    async fn shut_down_async() {
        let (_task, spawner) = build_spawner(2);
        let (before_shutdown_send, before_shutdown_recv) = crossbeam_channel::unbounded();
        spawner.spawn(move |_, current_task| {
            fuchsia_async::LocalExecutor::new().run_singlethreaded(async move {
                let on_shutdown =
                    current_task.kernel().on_shutdown.wrap_future(futures::future::pending::<()>());
                before_shutdown_send.send(()).unwrap();
                log_debug!("waiting for shutdown channel");
                on_shutdown.await;
            });
        });
        before_shutdown_recv.recv().unwrap();
        spawner.shut_down();
    }

    #[fuchsia::test]
    async fn shut_down_task() {
        let (_task, spawner) = build_spawner(2);
        let (before_shutdown_send, before_shutdown_recv) = crossbeam_channel::unbounded();
        spawner.spawn(move |_, current_task| {
            before_shutdown_send.send(()).unwrap();
            let event = InterruptibleEvent::new();
            let guard = event.begin_wait();
            log_debug!("waiting for shutdown signal");
            current_task.block_until(guard, zx::MonotonicInstant::INFINITE).unwrap_err();
        });
        before_shutdown_recv.recv().unwrap();
        spawner.shut_down();
    }

    #[fuchsia::test]
    async fn shut_down_port() {
        let (_task, spawner) = build_spawner(2);
        let (before_shutdown_send, before_shutdown_recv) = crossbeam_channel::unbounded();
        spawner.spawn(move |_, current_task| {
            let port = Arc::new(zx::Port::create());
            let _task_guard = current_task
                .kernel()
                .on_shutdown
                .register_port_for_user_packet(&port, zx::UserPacket::default());
            before_shutdown_send.send(()).unwrap();
            log_debug!("waiting for shutdown packet");
            assert_eq!(
                port.wait(zx::MonotonicInstant::INFINITE).unwrap().contents(),
                zx::PacketContents::User(zx::UserPacket::default()),
            );
        });
        before_shutdown_recv.recv().unwrap();
        spawner.shut_down();
    }
}
