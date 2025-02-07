// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::on_shutdown::OnShutdown;
use crate::task::{with_new_current_task, CurrentTask, Task};
use crossbeam_channel::{SendError, Sender, TrySendError};
use futures::channel::oneshot;
use futures::TryFutureExt;
use starnix_logging::{log_debug, log_error};
use starnix_sync::{Locked, Mutex, Unlocked};
use starnix_types::ownership::{release_after, WeakRef};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::ffi::CString;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

type BoxedClosure = Box<dyn FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) + Send + 'static>;

/// A thread pool that immediately execute any new work sent to it and keep a maximum number of
/// idle threads.
#[derive(Debug)]
pub struct DynamicThreadSpawner {
    state: Arc<Mutex<DynamicThreadSpawnerState>>,
    /// The weak system task to create the kernel thread associated with each thread.
    system_task: WeakRef<Task>,

    /// Broker of the threadpool shutdown signal.
    on_shutdown: OnShutdown,

    /// Whether we're shutting down. If a thread tries to spawn a new task while we're shutting
    /// down, panic instead of deadlocking.
    shutting_down: AtomicBool,
}

#[derive(Debug)]
struct DynamicThreadSpawnerState {
    /// A persistent thread that is used to create new thread. This ensures that threads are
    /// created from the initial starnix process and are not tied to a specific task.
    /// Stored as an Option to allow dropping it ahead of the full struct.
    persistent_thread: Option<RunningThread>,
    threads: Vec<RunningThread>,
    idle_threads: u8,
    max_idle_threads: u8,
}

impl DynamicThreadSpawner {
    pub fn new(max_idle_threads: u8, system_task: WeakRef<Task>, on_shutdown: OnShutdown) -> Self {
        let state = Arc::new(Mutex::new(DynamicThreadSpawnerState {
            persistent_thread: Some(RunningThread::new_persistent(system_task.clone())),
            threads: vec![],
            idle_threads: 0,
            max_idle_threads,
        }));
        Self { state, system_task, on_shutdown, shutting_down: AtomicBool::new(false) }
    }

    /// Shut down the thread spawner, joining all of the threads.
    pub fn shut_down(&self) {
        log_debug!("shutting down thread spawner");
        self.shutting_down.store(true, Ordering::Release);

        let _threads_to_drop = {
            // Threads may get wedged while dropping if the state lock is held.
            let mut state = self.state.lock();
            let mut threads = state.threads.drain(..).collect::<Vec<_>>();
            threads.extend(state.persistent_thread.take());
            threads
        };

        self.on_shutdown.notify();
    }

    /// Run the given closure on a thread and returns a Future that will resolve to the return
    /// value of the closure.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread. When this method returns, it is guaranteed that a thread is
    /// responsible to start running the closure.
    ///
    /// # Shutdown
    ///
    /// Tasks run on the threads spawned by this function must be prepared to exit when Starnix
    /// shuts down. See the `OnShutdown` type available through the `Kernel` struct to receive a
    /// notification that it should shut down.
    ///
    /// # Panics
    ///
    /// If called while the spawner is already shutting down.
    pub fn spawn_and_get_result<R, F>(&self, f: F) -> impl Future<Output = Result<R, Errno>>
    where
        R: Send + 'static,
        F: FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) -> R + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel::<R>();
        self.spawn(move |locked, current_task| {
            let _ = sender.send(f(locked, current_task));
        });
        receiver.map_err(|_| errno!(EINTR))
    }

    /// Run the given closure on a thread and block to get the result.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread.
    ///
    /// # Shutdown
    ///
    /// Tasks run on the threads spawned by this function must be prepared to exit when Starnix
    /// shuts down. See the `OnShutdown` type available through the `Kernel` struct to receive a
    /// notification that it should shut down.
    ///
    /// # Panics
    ///
    /// If called while the spawner is already shutting down.
    pub fn spawn_and_get_result_sync<R, F>(&self, f: F) -> Result<R, Errno>
    where
        R: Send + 'static,
        F: FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) -> R + Send + 'static,
    {
        let (sender, receiver) = crossbeam_channel::bounded::<R>(1);
        self.spawn(move |locked, current_task| {
            let _ = sender.send(f(locked, current_task));
        });
        receiver.recv().map_err(|_| errno!(EINTR))
    }

    /// Run the given closure on a thread.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread. When this method returns, it is guaranteed that a thread is
    /// responsible to start running the closure.
    ///
    /// # Shutdown
    ///
    /// Tasks run on the threads spawned by this function must be prepared to exit when Starnix
    /// shuts down. See the `OnShutdown` type available through the `Kernel` struct to receive a
    /// notification that it should shut down.
    ///
    /// # Panics
    ///
    /// If called while the spawner is already shutting down.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) + Send + 'static,
    {
        assert!(
            !self.shutting_down.load(Ordering::Acquire),
            "cannot spawn new tasks while shutting down"
        );
        // Check whether a thread already exists to handle the request.
        let mut function: BoxedClosure = Box::new(f);
        let mut state = self.state.lock();
        if state.idle_threads > 0 {
            let mut i = 0;
            while i < state.threads.len() {
                // Increases `i` immediately, so that it can be decreased it the thread must be
                // dropped.
                let thread_index = i;
                i += 1;
                match state.threads[thread_index].try_dispatch(function) {
                    Ok(_) => {
                        // The dispatch succeeded.
                        state.idle_threads -= 1;
                        return;
                    }
                    Err(TrySendError::Full(f)) => {
                        // The thread is busy.
                        function = f;
                    }
                    Err(TrySendError::Disconnected(f)) => {
                        // The receiver is disconnected, it means the thread has terminated, drop it.
                        state.idle_threads -= 1;
                        state.threads.remove(thread_index);
                        i -= 1;
                        function = f;
                    }
                }
            }
        }

        // A new thread must be created. It needs to be done from the persistent thread.
        let (sender, receiver) = crossbeam_channel::bounded::<RunningThread>(0);
        let dispatch_function: BoxedClosure = Box::new({
            let state = self.state.clone();
            let system_task = self.system_task.clone();
            move |_, _| {
                sender
                    .send(RunningThread::new(state, system_task, function))
                    .expect("receiver must not be dropped");
            }
        });
        state
            .persistent_thread
            .as_ref()
            .expect("can't be called while shutting down")
            .dispatch(dispatch_function)
            .expect("persistent thread should not have ended.");
        state.threads.push(receiver.recv().expect("persistent thread should not have ended."));
    }
}

#[derive(Debug)]
struct RunningThread {
    thread: Option<JoinHandle<()>>,
    sender: Option<Sender<BoxedClosure>>,
}

impl RunningThread {
    fn new(
        state: Arc<Mutex<DynamicThreadSpawnerState>>,
        system_task: WeakRef<Task>,
        f: BoxedClosure,
    ) -> Self {
        let (sender, receiver) = crossbeam_channel::bounded::<BoxedClosure>(0);
        let thread = Some(
            std::thread::Builder::new()
                .name("kthread-dynamic-worker".to_string())
                .spawn(move || {
                    // It's ok to create a new lock context here, since we are on a new thread.
                    let mut locked = unsafe { Unlocked::new() };
                    let result =
                        with_new_current_task(&mut locked, &system_task, |locked, current_task| {
                            let _shutdown_guard =
                                current_task.kernel().on_shutdown.register_task(&current_task.task);
                            while let Ok(f) = receiver.recv() {
                                f(locked, &current_task);
                                // Apply any delayed releasers.
                                current_task.trigger_delayed_releaser(locked);
                                let mut state = state.lock();
                                state.idle_threads += 1;
                                if state.idle_threads > state.max_idle_threads {
                                    // If the number of idle thread is greater than the max, the
                                    // thread terminates.  This disconnects the receiver, which will
                                    // ensure that the thread will be joined and remove from the list
                                    // of available threads the next time the pool tries to use it.
                                    return;
                                }
                            }
                        });
                    if let Err(e) = result {
                        log_error!("Unable to create a kernel thread: {e:?}");
                    }
                })
                .expect("able to create threads"),
        );
        let result = Self { thread, sender: Some(sender) };
        // The dispatch cannot fail because the thread can only finish after having executed at
        // least one task, and this is the first task ever dispatched to it.
        result
            .sender
            .as_ref()
            .expect("sender should never be None")
            .send(f)
            .expect("Dispatch cannot fail");
        result
    }

    fn new_persistent(system_task: WeakRef<Task>) -> Self {
        // The persistent thread doesn't need to do any rendez-vous when received task.
        let (sender, receiver) = crossbeam_channel::bounded::<BoxedClosure>(20);
        let thread = Some(
            std::thread::Builder::new()
                .name("kthread-persistent-worker".to_string())
                .spawn(move || {
                    // It's ok to create a new lock context here, since we are on a new thread.
                    let mut locked = unsafe { Unlocked::new() };
                    let current_task = {
                        let Some(system_task) = system_task.upgrade() else {
                            return;
                        };
                        match CurrentTask::create_kernel_thread(
                            &mut locked,
                            &system_task,
                            CString::new("kthreadd").unwrap(),
                        ) {
                            Ok(task) => task,
                            Err(e) => {
                                log_error!("Unable to create a kernel thread: {e:?}");
                                return;
                            }
                        }
                    };
                    release_after!(current_task, &mut locked, {
                        while let Ok(f) = receiver.recv() {
                            f(&mut locked, &current_task);

                            // Apply any delayed releasers.
                            current_task.trigger_delayed_releaser(&mut locked);
                        }
                    });
                })
                .expect("able to create threads"),
        );
        Self { thread, sender: Some(sender) }
    }

    fn try_dispatch(&self, f: BoxedClosure) -> Result<(), TrySendError<BoxedClosure>> {
        self.sender.as_ref().expect("sender should never be None").try_send(f)
    }

    fn dispatch(&self, f: BoxedClosure) -> Result<(), SendError<BoxedClosure>> {
        self.sender.as_ref().expect("sender should never be None").send(f)
    }
}

impl Drop for RunningThread {
    fn drop(&mut self) {
        self.sender = None;
        match self.thread.take() {
            Some(thread) => thread.join().expect("Thread should join."),
            _ => panic!("Thread should never be None"),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{create_kernel_and_task, AutoReleasableTask};

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
    async fn run_simple_task() {
        let (_task, spawner) = build_spawner(2);
        spawner.spawn(|_, _| {});
    }

    #[fuchsia::test]
    async fn run_10_tasks() {
        let (_task, spawner) = build_spawner(2);
        for _ in 0..10 {
            spawner.spawn(|_, _| {});
        }
    }

    #[fuchsia::test]
    async fn blocking_task_do_not_prevent_further_processing() {
        let (_task, spawner) = build_spawner(1);

        let pair = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        for _ in 0..10 {
            let pair2 = Arc::clone(&pair);
            spawner.spawn(move |_, _| {
                let (lock, cvar) = &*pair2;
                let mut cont = lock.lock().unwrap();
                while !*cont {
                    cont = cvar.wait(cont).unwrap();
                }
            });
        }

        assert_eq!(
            spawner.spawn_and_get_result_sync(move |_, _| {
                {
                    let (lock, cvar) = &*pair;
                    let mut cont = lock.lock().unwrap();
                    *cont = true;
                    cvar.notify_all();
                }
            }),
            Ok(())
        );
    }

    #[fuchsia::test]
    async fn run_spawn_and_get_result() {
        let (_task, spawner) = build_spawner(2);
        assert_eq!(spawner.spawn_and_get_result(|_, _| 3).await, Ok(3));
    }
}
