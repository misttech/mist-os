// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    lock::Mutex,
    types::{errno, Errno},
};
use futures::{channel::oneshot, TryFutureExt};
use std::{
    future::Future,
    sync::{
        mpsc::{sync_channel, SendError, SyncSender, TrySendError},
        Arc,
    },
    thread::JoinHandle,
};

type BoxedClosure = Box<dyn FnOnce() + Send + 'static>;

/// A thread pool that immediately execute any new work sent to it and keep a maximum number of
/// idle threads.
#[derive(Debug)]
pub struct DynamicThreadPool {
    state: Arc<Mutex<DynamicThreadPoolState>>,
    /// A persistent thread that is used to create new thread. This ensures that threads are
    /// created from the initial starnix process and are not tied to a specific task.
    persistent_thread: RunningThread,
}

#[derive(Debug, Default)]
struct DynamicThreadPoolState {
    threads: Vec<RunningThread>,
    idle_threads: u8,
    max_idle_threads: u8,
}

impl DynamicThreadPool {
    pub fn new(max_idle_threads: u8) -> Self {
        Self {
            state: Arc::new(Mutex::new(DynamicThreadPoolState {
                max_idle_threads,
                ..Default::default()
            })),
            persistent_thread: RunningThread::new_persistent(),
        }
    }

    /// Dispatch the given closure to the thread pool and returns a Future that will resolve to the
    /// return value of the closure.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread. When this method returns, it is guaranteed that a thread is
    /// responsible to start running the closure.
    pub fn dispatch_and_get_result<R, F>(&self, f: F) -> impl Future<Output = Result<R, Errno>>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel::<R>();
        self.dispatch(move || {
            let _ = sender.send(f());
        });
        receiver.map_err(|_| errno!(EINTR))
    }

    /// Dispatch the given closure to the thread pool.
    ///
    /// This method will use an idle thread in the pool if one is available, otherwise it will
    /// start a new thread. When this method returns, it is guaranteed that a thread is
    /// responsible to start running the closure.
    pub fn dispatch<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
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
        let self_state = self.state.clone();
        let dispatch_function: BoxedClosure = Box::new(move || {
            sender
                .send(RunningThread::new(self_state, function))
                .expect("receiver must not be dropped");
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
    fn new(state: Arc<Mutex<DynamicThreadPoolState>>, f: BoxedClosure) -> Self {
        let (sender, receiver) = sync_channel::<BoxedClosure>(0);
        let thread = Some(std::thread::spawn(move || {
            while let Ok(f) = receiver.recv() {
                f();
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
        }));
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

    fn new_persistent() -> Self {
        // The persistent thread doesn't need to do any rendez-vous when received task.
        let (sender, receiver) = sync_channel::<BoxedClosure>(20);
        let thread = Some(std::thread::spawn(move || {
            while let Ok(f) = receiver.recv() {
                f();
            }
        }));
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

    #[fuchsia::test]
    fn run_simple_task() {
        let pool = DynamicThreadPool::new(2);
        pool.dispatch(|| {});
    }

    #[fuchsia::test]
    fn run_10_tasks() {
        let pool = DynamicThreadPool::new(2);
        for _ in 0..10 {
            pool.dispatch(|| {});
        }
    }

    #[fuchsia::test]
    fn blocking_task_do_not_prevent_further_processing() {
        let pool = DynamicThreadPool::new(1);

        let pair = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        for _ in 0..10 {
            let pair2 = Arc::clone(&pair);
            pool.dispatch(move || {
                let (lock, cvar) = &*pair2;
                let mut cont = lock.lock().unwrap();
                while !*cont {
                    cont = cvar.wait(cont).unwrap();
                }
            });
        }

        let executed = Arc::new(Mutex::new(false));
        let executed_clone = executed.clone();
        pool.dispatch(move || {
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
        pool.dispatch(move || {});
        pool.dispatch(move || {});

        // Drop the pool. This will wait for all thread to finish.
        std::mem::drop(pool);
        assert!(*executed.lock());
    }

    #[fuchsia::test]
    async fn run_dispatch_and_get_result() {
        let pool = DynamicThreadPool::new(2);
        assert_eq!(pool.dispatch_and_get_result(|| 3).await, Ok(3));
    }
}
