// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides utilities for working with futures.

use std::pin::Pin;

use futures::{future::FusedFuture, task, Future};

/// Future for the [`FutureExt::replace_value`] method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct ReplaceValue<Fut: Future<Output = ()>, T> {
    future: Fut,
    value: Option<T>,
}

/// An extension trait for [`futures::Future`] that provides specialized adapters.
pub trait FutureExt: Future<Output = ()> {
    /// Map this future's output to a different type, returning a new future of
    /// the resulting type.
    ///
    /// This function is similar to futures::FutureExt::map except:
    ///
    /// - it takes a value instead of a closure
    ///
    /// - it returns a type that can be named
    ///
    /// This function is useful when a mapped future is needed and boxing is not
    /// desired.
    fn replace_value<T>(self, value: T) -> ReplaceValue<Self, T>
    where
        Self: Sized,
    {
        ReplaceValue::new(self, value)
    }
}

impl<Fut: Future<Output = ()>, T> ReplaceValue<Fut, T> {
    fn new(future: Fut, value: T) -> Self {
        Self { future, value: Some(value) }
    }
}

impl<T: ?Sized + Future<Output = ()>> FutureExt for T {}

impl<Fut: Future<Output = ()> + Unpin, T: Unpin> Future for ReplaceValue<Fut, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        let Self { future, value } = self.get_mut();
        let () = futures::ready!(Pin::new(future).poll(cx));
        task::Poll::Ready(
            value.take().expect("ReplaceValue must not be polled after it returned `Poll::Ready`"),
        )
    }
}

impl<Fut: Future<Output = ()> + Unpin, T: Unpin> FusedFuture for ReplaceValue<Fut, T> {
    fn is_terminated(&self) -> bool {
        self.value.is_none()
    }
}

/// A future that yields to the executor only once.
///
/// This future returns [`Poll::Pending`] the first time it's polled after
/// waking the context waker. This effectively yields the currently running task
/// to the executor, but puts it back in the executor's ready task queue.
///
/// Example:
/// ```
/// loop {
///   let read = read_big_thing().await;
///
///   while let Some(x) = read.next() {
///     process_one_thing(x);
///     YieldToExecutorOnce::new().await;
///   }
/// }
/// ```
#[derive(Default)]
pub struct YieldToExecutorOnce(YieldToExecutorOnceInner);

#[derive(Default)]
enum YieldToExecutorOnceInner {
    #[default]
    NotPolled,
    Ready,
    Terminated,
}

impl YieldToExecutorOnce {
    /// Creates a new `YieldToExecutorOnce`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Future for YieldToExecutorOnce {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        let Self(inner) = self.get_mut();
        match *inner {
            YieldToExecutorOnceInner::NotPolled => {
                *inner = YieldToExecutorOnceInner::Ready;
                // Wake the executor before returning pending. We only want to yield
                // once.
                cx.waker().wake_by_ref();
                task::Poll::Pending
            }
            YieldToExecutorOnceInner::Ready => {
                *inner = YieldToExecutorOnceInner::Terminated;
                task::Poll::Ready(())
            }
            YieldToExecutorOnceInner::Terminated => {
                panic!("polled future after completion");
            }
        }
    }
}

impl FusedFuture for YieldToExecutorOnce {
    fn is_terminated(&self) -> bool {
        let Self(inner) = self;
        match inner {
            YieldToExecutorOnceInner::Ready | YieldToExecutorOnceInner::NotPolled => false,
            YieldToExecutorOnceInner::Terminated => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use fuchsia_async as fasync;

    #[fasync::run_singlethreaded(test)]
    async fn replace_value_trivial() {
        use super::FutureExt as _;

        let value = "hello world";
        assert_eq!(futures::future::ready(()).replace_value(value).await, value);
    }

    #[test]
    fn replace_value_is_terminated() {
        use super::FutureExt as _;
        use futures::future::{FusedFuture as _, FutureExt as _};

        let fut = &mut futures::future::ready(()).replace_value(());
        assert!(!fut.is_terminated());
        assert_eq!(fut.now_or_never(), Some(()));
        assert!(fut.is_terminated());
    }

    #[test]
    fn yield_to_executor_once() {
        use futures::{future::FusedFuture as _, FutureExt as _};

        let (waker, count) = futures_test::task::new_count_waker();
        let mut context = std::task::Context::from_waker(&waker);
        let mut fut = super::YieldToExecutorOnce::new();

        assert!(!fut.is_terminated());
        assert_eq!(count, 0);
        assert_eq!(fut.poll_unpin(&mut context), std::task::Poll::Pending);
        assert!(!fut.is_terminated());
        assert_eq!(count, 1);
        assert_eq!(fut.poll_unpin(&mut context), std::task::Poll::Ready(()));
        assert!(fut.is_terminated());
        // The waker is never hit again.
        assert_eq!(count, 1);
    }
}
