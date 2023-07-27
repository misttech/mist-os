// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::super::timer::TimerHeap;
use super::common::{with_local_timer_heap, ExecutorTime, Inner};
use futures::{
    future::{self, FutureObj},
    FutureExt,
};
use parking_lot::{Condvar, Mutex};
use std::{
    fmt,
    future::Future,
    mem,
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
    usize,
};

/// A multi-threaded port-based executor for Fuchsia OS. Requires that tasks scheduled on it
/// implement `Send` so they can be load balanced between worker threads.
///
/// Having a `SendExecutor` in scope allows the creation and polling of zircon objects, such as
/// [`fuchsia_async::Channel`].
///
/// # Panics
///
/// `SendExecutor` will panic on drop if any zircon objects attached to it are still alive. In other
/// words, zircon objects backed by a `SendExecutor` must be dropped before it.
pub struct SendExecutor {
    /// The inner executor state.
    inner: Arc<Inner>,
    /// Worker thread handles
    threads: Vec<thread::JoinHandle<()>>,
}

impl fmt::Debug for SendExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendExecutor").field("port", &self.inner.port).finish()
    }
}

impl SendExecutor {
    /// Create a new multi-threaded executor.
    #[allow(deprecated)]
    pub fn new(num_threads: usize) -> Self {
        let inner = Arc::new(Inner::new(
            ExecutorTime::RealTime,
            /* is_local */ false,
            num_threads.try_into().expect("no more than 256 threads are supported"),
        ));
        inner.clone().set_local(TimerHeap::default());
        Self { inner, threads: Vec::default() }
    }

    /// Run `future` to completion, using this thread and `num_threads` workers in a pool to
    /// poll active tasks.
    // The debugger looks for this function on the stack, so if its (fully-qualified) name changes,
    // the debugger needs to be updated.
    // LINT.IfChange
    pub fn run<F>(&mut self, future: F) -> F::Output
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        assert!(self.inner.is_real_time(), "Error: called `run` on an executor using fake time");

        let pair = Arc::new((Mutex::new(None), Condvar::new()));
        let pair2 = pair.clone();

        // Spawn a future which will set the result upon completion.
        Inner::spawn_main(
            &self.inner,
            FutureObj::new(Box::new(future.then(move |fut_result| {
                let (lock, cvar) = &*pair2;
                let mut result = lock.lock();
                *result = Some(fut_result);
                cvar.notify_one();
                future::ready(())
            }))),
        );

        // Start worker threads, handing off timers from the current thread.
        self.inner.done.store(false, Ordering::SeqCst);
        with_local_timer_heap(|timer_heap| {
            let timer_heap = mem::replace(timer_heap, TimerHeap::default());
            self.create_worker_threads(Some(timer_heap));
        });

        // Wait until the signal the future has completed.
        let (lock, cvar) = &*pair;
        let mut result = lock.lock();
        if result.is_none() {
            let mut last_polled = 0;
            let mut last_tasks_ready = false;
            loop {
                // This timeout is chosen to be quite high since it impacts all processes that have
                // multi-threaded async executors, and it exists to workaround arguably misbehaving
                // users (see the comment below).
                const TIMEOUT: Duration = Duration::from_millis(250);
                cvar.wait_until(&mut result, Instant::now() + TIMEOUT);
                if result.is_some() {
                    break;
                }
                let polled = self.inner.polled.load(Ordering::Relaxed);
                let tasks_ready = !self.inner.ready_tasks.is_empty();
                if polled == last_polled && last_tasks_ready && tasks_ready {
                    // If this log message is printed, it most likely means that a task has blocked
                    // making a reentrant synchronous call that doesn't involve a port message being
                    // processed by this same executor. This can arise even if you would expect
                    // there to normally be other port messages involved. One example (that has
                    // actually happened): spawn a task to service a fuchsia.io connection, then try
                    // and synchronously connect to that service. If the task hasn't had a chance to
                    // run, then the async channel might not be registered with the executor, and so
                    // sending messages to the channel doesn't trigger a port message. Typically,
                    // the way to solve these issues is to run the service in a different executor
                    // (which could be the same or a different process).
                    eprintln!("Tasks might be stalled!");
                    self.inner.wake_one_thread();
                }
                last_polled = polled;
                last_tasks_ready = tasks_ready;
            }
        }

        // Spin down worker threads
        self.join_all();

        // Unwrap is fine because of the check to `is_none` above.
        result.take().unwrap()
    }

    /// Add `self.num_threads` worker threads to the executor's thread pool.
    /// `timers`: timers from the "main" thread which would otherwise be lost.
    fn create_worker_threads(&mut self, mut timers: Option<TimerHeap>) {
        for _ in 0..self.inner.num_threads {
            let inner = self.inner.clone();
            let timers = timers.take().unwrap_or(TimerHeap::default());
            self.threads.push(thread::spawn(move || {
                inner.clone().set_local(timers);
                inner.worker_lifecycle::</* UNTIL_STALLED: */ false>();
            }));
        }
    }

    fn join_all(&mut self) {
        self.inner.mark_done();

        // Join the worker threads
        for thread in self.threads.drain(..) {
            thread.join().expect("Couldn't join worker thread.");
        }
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> super::instrumentation::Snapshot {
        self.inner.collector.snapshot()
    }
}

impl Drop for SendExecutor {
    fn drop(&mut self) {
        self.join_all();
        self.inner.on_parent_drop();
    }
}

// TODO(fxbug.dev/76583) test SendExecutor with unit tests

#[cfg(test)]
mod tests {
    use {
        super::SendExecutor,
        crate::{Task, Timer},
        fuchsia_zircon::DurationNum,
        futures::channel::oneshot,
        std::sync::{Arc, Condvar, Mutex},
    };

    #[test]
    fn test_stalled_triggers_wake_up() {
        SendExecutor::new(2).run(async {
            // The timer will only fire on one thread, so use one so we can get to a point where
            // only one thread is running.
            Timer::new(10.millis()).await;

            let (tx, rx) = oneshot::channel();
            let pair = Arc::new((Mutex::new(false), Condvar::new()));
            let pair2 = pair.clone();

            let _task = Task::spawn(async move {
                // Send a notification to the other task.
                tx.send(()).unwrap();
                // Now block the thread waiting for the result.
                let (lock, cvar) = &*pair;
                let mut done = lock.lock().unwrap();
                while !*done {
                    done = cvar.wait(done).unwrap();
                }
            });

            rx.await.unwrap();
            let (lock, cvar) = &*pair2;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        });
    }
}
