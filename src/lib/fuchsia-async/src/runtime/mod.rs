// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
use self::fuchsia as implementation;

#[cfg(all(not(target_os = "fuchsia"), not(target_arch = "wasm32")))]
mod portable;
#[cfg(all(not(target_os = "fuchsia"), not(target_arch = "wasm32")))]
use self::portable as implementation;

#[cfg(all(not(target_os = "fuchsia"), target_arch = "wasm32"))]
mod stub;
#[cfg(all(not(target_os = "fuchsia"), target_arch = "wasm32"))]
use self::stub as implementation;

// Exports common to all target os.
pub use implementation::executor::{
    LocalExecutor, MonotonicDuration, MonotonicInstant, SendExecutor, TestExecutor,
};
pub use implementation::task::{unblock, JoinHandle, Task};
pub use implementation::timer::Timer;

#[cfg(not(target_arch = "wasm32"))]
mod task_group;
#[cfg(not(target_arch = "wasm32"))]
pub use task_group::*;

// Fuchsia specific exports.
#[cfg(target_os = "fuchsia")]
pub use self::fuchsia::{
    executor::{BootInstant, EHandle, PacketReceiver, ReceiverRegistration},
    timer::Interval,
};

/// Structured concurrency API for fuchsia-async.
///
/// See the [`Scope`] documentation for details.
pub mod scope {
    pub use super::implementation::scope::{Scope, ScopeRef};

    #[cfg(target_os = "fuchsia")]
    pub use super::implementation::scope::Join;
}

pub use scope::{Scope, ScopeRef};

use futures::prelude::*;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

/// An extension trait to provide `after_now` on `zx::MonotonicDuration`.
pub trait DurationExt {
    /// Return a `MonotonicInstant` which is a `MonotonicDuration` after the current time.
    /// `duration.after_now()` is equivalent to `MonotonicInstant::after(duration)`.
    ///
    /// This method requires that an executor has been set up.
    fn after_now(self) -> MonotonicInstant;
}

/// The time when a Timer should wakeup.
pub trait WakeupTime {
    /// Create a timer based on this time.
    ///
    /// This is allowed to be inaccurate, but the inaccuracy must make the wakeup time later,
    /// never earlier.
    fn into_timer(self) -> Timer;
}

#[cfg(target_os = "fuchsia")]
impl WakeupTime for std::time::Duration {
    fn into_timer(self) -> Timer {
        EHandle::local().mono_timers().new_timer(MonotonicInstant::now() + self.into())
    }
}

#[cfg(not(target_os = "fuchsia"))]
impl WakeupTime for std::time::Duration {
    fn into_timer(self) -> Timer {
        Timer(async_io::Timer::at(MonotonicInstant::now() + self.into()))
    }
}

#[cfg(target_os = "fuchsia")]
impl WakeupTime for MonotonicDuration {
    fn into_timer(self) -> Timer {
        EHandle::local().mono_timers().new_timer(MonotonicInstant::after(self))
    }
}

#[cfg(target_os = "fuchsia")]
impl WakeupTime for zx::BootDuration {
    fn into_timer(self) -> Timer {
        EHandle::local().boot_timers().new_timer(BootInstant::after(self))
    }
}

impl DurationExt for std::time::Duration {
    fn after_now(self) -> MonotonicInstant {
        MonotonicInstant::now() + self.into()
    }
}

/// A trait which allows futures to be easily wrapped in a timeout.
pub trait TimeoutExt: Future + Sized {
    /// Wraps the future in a timeout, calling `on_timeout` to produce a result
    /// when the timeout occurs.
    fn on_timeout<WT, OT>(self, time: WT, on_timeout: OT) -> OnTimeout<Self, OT>
    where
        WT: WakeupTime,
        OT: FnOnce() -> Self::Output,
    {
        OnTimeout { timer: time.into_timer(), future: self, on_timeout: Some(on_timeout) }
    }

    /// Wraps the future in a stall-guard, calling `on_stalled` to produce a result
    /// when the future hasn't been otherwise polled within the `timeout`.
    /// This is a heuristic - spurious wakeups will keep the detection from triggering,
    /// and moving all work to external tasks or threads with force the triggering early.
    fn on_stalled<OS>(self, timeout: std::time::Duration, on_stalled: OS) -> OnStalled<Self, OS>
    where
        OS: FnOnce() -> Self::Output,
    {
        OnStalled {
            timer: timeout.into_timer(),
            future: self,
            timeout,
            on_stalled: Some(on_stalled),
        }
    }
}

impl<F: Future + Sized> TimeoutExt for F {}

pin_project! {
    /// A wrapper for a future which will complete with a provided closure when a timeout occurs.
    #[derive(Debug)]
    #[must_use = "futures do nothing unless polled"]
    pub struct OnTimeout<F, OT> {
        #[pin]
        timer: Timer,
        #[pin]
        future: F,
        on_timeout: Option<OT>,
    }
}

impl<F: Future, OT> Future for OnTimeout<F, OT>
where
    OT: FnOnce() -> F::Output,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if let Poll::Ready(item) = this.future.poll(cx) {
            return Poll::Ready(item);
        }
        if let Poll::Ready(()) = this.timer.poll(cx) {
            let ot = this.on_timeout.take().expect("polled withtimeout after completion");
            let item = (ot)();
            return Poll::Ready(item);
        }
        Poll::Pending
    }
}

pin_project! {
    /// A wrapper for a future who's steady progress is monitored and will complete with the
    /// provided closure if no progress is made before the timeout.
    #[derive(Debug)]
    #[must_use = "futures do nothing unless polled"]
    pub struct OnStalled<F, OS> {
        #[pin]
        timer: Timer,
        #[pin]
        future: F,
        timeout: std::time::Duration,
        on_stalled: Option<OS>,
    }
}

impl<F: Future, OS> Future for OnStalled<F, OS>
where
    OS: FnOnce() -> F::Output,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if let Poll::Ready(item) = this.future.poll(cx) {
            return Poll::Ready(item);
        }
        match this.timer.as_mut().poll(cx) {
            Poll::Ready(()) => {}
            Poll::Pending => {
                this.timer.set(this.timeout.into_timer());
                ready!(this.timer.as_mut().poll(cx));
            }
        }
        Poll::Ready((this.on_stalled.take().expect("polled after completion"))())
    }
}

#[cfg(test)]
mod task_tests {

    use super::*;
    use futures::channel::oneshot;

    fn run(f: impl Send + 'static + Future<Output = ()>) {
        const TEST_THREADS: usize = 2;
        SendExecutor::new(TEST_THREADS).run(f)
    }

    #[test]
    fn can_detach() {
        run(async move {
            let (tx_started, rx_started) = oneshot::channel();
            let (tx_continue, rx_continue) = oneshot::channel();
            let (tx_done, rx_done) = oneshot::channel();
            {
                // spawn a task and detach it
                // the task will wait for a signal, signal it received it, and then wait for another
                Task::spawn(async move {
                    tx_started.send(()).unwrap();
                    rx_continue.await.unwrap();
                    tx_done.send(()).unwrap();
                })
                .detach();
            }
            // task is detached, have a short conversation with it
            rx_started.await.unwrap();
            tx_continue.send(()).unwrap();
            rx_done.await.unwrap();
        });
    }

    #[test]
    fn can_join() {
        // can we spawn, then join a task
        run(async move {
            assert_eq!(42, Task::spawn(async move { 42u8 }).await);
        })
    }

    #[test]
    fn can_join_unblock() {
        // can we poll a blocked task
        run(async move {
            assert_eq!(42, unblock(|| 42u8).await);
        })
    }

    #[test]
    fn can_join_unblock_local() {
        // can we poll a blocked task in a local executor
        LocalExecutor::new().run_singlethreaded(async move {
            assert_eq!(42, unblock(|| 42u8).await);
        });
    }

    #[test]
    #[should_panic]
    // TODO(https://fxbug.dev/42169733): delete the below
    #[cfg_attr(feature = "variant_asan", ignore)]
    fn unblock_fn_panics() {
        run(async move {
            unblock(|| panic!("bad")).await;
        })
    }

    #[test]
    fn can_join_local() {
        // can we spawn, then join a task locally
        LocalExecutor::new().run_singlethreaded(async move {
            assert_eq!(42, Task::local(async move { 42u8 }).await);
        })
    }

    #[test]
    fn can_cancel() {
        run(async move {
            let (_tx_start, rx_start) = oneshot::channel::<()>();
            let (tx_done, rx_done) = oneshot::channel();
            // Start and immediately cancel the task (by dropping it).
            let _ = Task::spawn(async move {
                rx_start.await.unwrap();
                tx_done.send(()).unwrap();
            });
            // we should see an error on receive
            rx_done.await.expect_err("done should not be sent");
        })
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;
    use futures::future::Either;
    use std::pin::pin;

    #[test]
    fn shorter_fires_first_instant() {
        use std::time::{Duration, Instant};
        let mut exec = LocalExecutor::new();
        let now = Instant::now();
        let shorter = pin!(Timer::new(now + Duration::from_millis(100)));
        let longer = pin!(Timer::new(now + Duration::from_secs(1)));
        match exec.run_singlethreaded(future::select(shorter, longer)) {
            Either::Left((_, _)) => {}
            Either::Right((_, _)) => panic!("wrong timer fired"),
        }
    }

    #[cfg(target = "fuchsia")]
    #[test]
    fn can_use_zx_duration() {
        let mut exec = LocalExecutor::new();
        let start = Instant::now();
        let timer = Timer::new(MonotonicDuration::from_millis(100));
        exec.run_singlethreaded(timer);
        let end = Instant::now();
        assert!(end - start > std::time::Duration::from_millis(100));
    }

    #[test]
    fn can_detect_stalls() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        use std::time::Duration;
        let runs = Arc::new(AtomicU64::new(0));
        assert_eq!(
            {
                let runs = runs.clone();
                LocalExecutor::new().run_singlethreaded(
                    async move {
                        let mut sleep = Duration::from_millis(1);
                        loop {
                            Timer::new(sleep).await;
                            sleep *= 2;
                            runs.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    .on_stalled(Duration::from_secs(1), || 1u8),
                )
            },
            1u8
        );
        assert!(runs.load(Ordering::SeqCst) >= 9);
    }
}
