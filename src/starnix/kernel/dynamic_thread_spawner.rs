// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::FsContext,
    log_error,
    task::{CurrentTask, Kernel, Task},
};
use futures::{channel::oneshot, TryFutureExt};
use starnix_lock::Mutex;
use starnix_uapi::{
    errno,
    errors::Errno,
    ownership::{release_after, ReleasableByRef},
};
use std::{
    ffi::CString,
    future::Future,
    sync::{
        mpsc::{sync_channel, SendError, SyncSender, TrySendError},
        Arc, Weak,
    },
    thread::JoinHandle,
};

type BoxedClosure = Box<dyn FnOnce(&CurrentTask) + Send + 'static>;

/// A thread pool that immediately execute any new work sent to it and keep a maximum number of
/// idle threads.
#[derive(Debug)]
pub struct DynamicThreadSpawner {
    state: Arc<Mutex<DynamicThreadSpawnerState>>,
    /// The `kernel` to create the kernel task associated with each thread.
    kernel: Weak<Kernel>,
    /// The `FsContext` to create the kernel task associated with each thread.
    fs_context: Arc<FsContext>,
    /// A persistent thread that is used to create new thread. This ensures that threads are
    /// created from the initial starnix process and are not tied to a specific task.
    persistent_thread: RunningThread,
}

#[derive(Debug, Default)]
struct DynamicThreadSpawnerState {
    threads: Vec<RunningThread>,
    idle_threads: u8,
    max_idle_threads: u8,
}

impl DynamicThreadSpawner {
    pub fn new(max_idle_threads: u8, kernel: Weak<Kernel>, fs_context: Arc<FsContext>) -> Self {
        let persistent_thread =
            RunningThread::new_persistent(kernel.clone(), Arc::clone(&fs_context));
        Self {
            state: Arc::new(Mutex::new(DynamicThreadSpawnerState {
                max_idle_threads,
                ..Default::default()
            })),
            kernel,
            fs_context,
            persistent_thread,
        }
    }

    /// Run the given closure on a thread and returns a Future that will resolve to the return
    /// value of the closure.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread. When this method returns, it is guaranteed that a thread is
    /// responsible to start running the closure.
    pub fn spawn_and_get_result<R, F>(&self, f: F) -> impl Future<Output = Result<R, Errno>>
    where
        R: Send + 'static,
        F: FnOnce(&CurrentTask) -> R + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel::<R>();
        self.spawn(move |current_task| {
            let _ = sender.send(f(current_task));
        });
        receiver.map_err(|_| errno!(EINTR))
    }

    /// Run the given closure on a thread.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread. When this method returns, it is guaranteed that a thread is
    /// responsible to start running the closure.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&CurrentTask) + Send + 'static,
    {
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
        let (sender, receiver) = sync_channel::<RunningThread>(0);
        let dispatch_function: BoxedClosure = Box::new({
            let state = self.state.clone();
            let kernel = self.kernel.clone();
            let fs_context = Arc::clone(&self.fs_context);
            move |_| {
                sender
                    .send(RunningThread::new(state, kernel, fs_context, function))
                    .expect("receiver must not be dropped");
            }
        });
        self.persistent_thread
            .dispatch(dispatch_function)
            .expect("persistent thread should not have ended.");
        state.threads.push(receiver.recv().expect("persistent thread should not have ended."));
    }
}

#[derive(Debug)]
struct RunningThread {
    thread: Option<JoinHandle<()>>,
    sender: Option<SyncSender<BoxedClosure>>,
}

impl RunningThread {
    fn new(
        state: Arc<Mutex<DynamicThreadSpawnerState>>,
        kernel: Weak<Kernel>,
        fs_context: Arc<FsContext>,
        f: BoxedClosure,
    ) -> Self {
        let (sender, receiver) = sync_channel::<BoxedClosure>(0);
        let thread = Some(
            std::thread::Builder::new()
                .name("kthread-dynamic-worker".to_string())
                .spawn(move || {
                    let current_task = {
                        let Some(kernel) = kernel.upgrade() else {
                            log_error!("Unable to upgrade kernel.");
                            return;
                        };
                        match Task::create_kernel_task(
                            &kernel,
                            CString::new("[kthreadd]").unwrap(),
                            fs_context,
                        ) {
                            Ok(task) => task,
                            Err(e) => {
                                log_error!("Unable to create a kernel task: {e:?}");
                                return;
                            }
                        }
                    };
                    release_after!(current_task, (), {
                        while let Ok(f) = receiver.recv() {
                            f(&current_task);
                            let mut state = state.lock();
                            state.idle_threads += 1;
                            if state.idle_threads > state.max_idle_threads {
                                // If the number of idle thread is greater than the max, the thread terminates.
                                // This disconnects the receiver, which will ensure that the thread will be
                                // joined and remove from the list of available threads the next time the
                                // pool tries to use it.
                                return;
                            }
                        }
                    });
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

    fn new_persistent(kernel: Weak<Kernel>, fs_context: Arc<FsContext>) -> Self {
        // The persistent thread doesn't need to do any rendez-vous when received task.
        let (sender, receiver) = sync_channel::<BoxedClosure>(20);
        let thread = Some(
            std::thread::Builder::new()
                .name("kthread-persistent-worker".to_string())
                .spawn(move || {
                    let current_task = {
                        let Some(kernel) = kernel.upgrade() else {
                            log_error!("Unable to upgrade kernel.");
                            return;
                        };
                        match Task::create_kernel_task(
                            &kernel,
                            CString::new("[kthreadd]").unwrap(),
                            fs_context,
                        ) {
                            Ok(task) => task,
                            Err(e) => {
                                log_error!("Unable to create a kernel task: {e:?}");
                                return;
                            }
                        }
                    };
                    release_after!(current_task, (), {
                        while let Ok(f) = receiver.recv() {
                            f(&current_task);
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
    use crate::testing::create_kernel_and_task;

    fn build_spawner(max_idle_threads: u8) -> (Arc<Kernel>, DynamicThreadSpawner) {
        let (kernel, task) = create_kernel_and_task();
        let spawner = DynamicThreadSpawner::new(
            max_idle_threads,
            Arc::downgrade(&kernel),
            Arc::clone(task.fs()),
        );
        (kernel, spawner)
    }

    #[fuchsia::test]
    async fn run_simple_task() {
        let (_kernel, spawner) = build_spawner(2);
        spawner.spawn(|_| {});
    }

    #[fuchsia::test]
    async fn run_10_tasks() {
        let (_kernel, spawner) = build_spawner(2);
        for _ in 0..10 {
            spawner.spawn(|_| {});
        }
    }

    #[fuchsia::test]
    async fn blocking_task_do_not_prevent_further_processing() {
        let (_kernel, spawner) = build_spawner(1);

        let pair = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        for _ in 0..10 {
            let pair2 = Arc::clone(&pair);
            spawner.spawn(move |_| {
                let (lock, cvar) = &*pair2;
                let mut cont = lock.lock().unwrap();
                while !*cont {
                    cont = cvar.wait(cont).unwrap();
                }
            });
        }

        let executed = Arc::new(Mutex::new(false));
        let executed_clone = executed.clone();
        spawner.spawn(move |_| {
            {
                let (lock, cvar) = &*pair;
                let mut cont = lock.lock().unwrap();
                *cont = true;
                cvar.notify_all();
            }
            *executed_clone.lock() = true;
        });

        // Wait for some time to ensure some threads have finished running.
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Post a couple of new tasks. As the maximum number of idle threads is 1, it should
        // ensures that finished threads will be cleaned up from the pool.
        spawner.spawn(move |_| {});
        spawner.spawn(move |_| {});

        // Drop the spawner. This will wait for all thread to finish.
        std::mem::drop(spawner);
        assert!(*executed.lock());
    }

    #[fuchsia::test]
    async fn run_spawn_and_get_result() {
        let (_kernel, spawner) = build_spawner(2);
        assert_eq!(spawner.spawn_and_get_result(|_| 3).await, Ok(3));
    }
}
