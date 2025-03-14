// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{AtomicFutureHandle, Bomb, Meta, DONE, RESULT_TAKEN};
use crate::{JoinHandle, ScopeHandle, Task};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

/// `SpawnableFuture` is a boxed future that can be spawned without incurring any more allocations
/// i.e. it doesn't end up with the double boxing that you end up with if you try and spawn `Box<dyn
/// Future>`.  It can be used in place of `BoxFuture` although it carries more overhead than
/// `BoxFuture`, so it shouldn't be used if it isn't going to be spawned on a scope.
/// `SpawnableFuture` implements `Future` but the future will not be running as a separate task if
/// used this way.  If polled and then later spawned, the spawned task will be polled again and any
/// waker recorded when polled prior to spawning will be impotent.
pub struct SpawnableFuture<'a, O>(AtomicFutureHandle<'a>, PhantomData<O>);

impl<O> Unpin for SpawnableFuture<'_, O> {}

impl<'a, O> SpawnableFuture<'a, O> {
    /// Creates a new spawnable future. To spawn the future on a scope, use either `spawn_on` or
    /// `compute_on`.
    pub fn new<F: Future<Output = O> + Send + 'a>(future: F) -> Self
    where
        O: Send + 'a,
    {
        Self(AtomicFutureHandle::new(None, 0, future), PhantomData)
    }

    fn meta(&mut self) -> &mut Meta {
        // SAFETY: This is safe because we know there is only one reference to the handle.
        unsafe { &mut *self.0 .0.as_mut() }
    }
}

impl SpawnableFuture<'static, ()> {
    /// Spawns the future on the specified scope.
    pub fn spawn_on(mut self, scope: ScopeHandle) -> JoinHandle<()> {
        let meta = self.meta();
        meta.scope = Some(scope.clone());
        let task_id = scope.executor().next_task_id();
        meta.id = task_id;
        scope.insert_task(self.0, false);
        JoinHandle::new(scope, task_id)
    }
}

impl<O> SpawnableFuture<'static, O> {
    /// Like `spawn` but for tasks that return a result.  See `Scope::compute` for drop semantics.
    pub fn compute_on(mut self, scope: ScopeHandle) -> Task<O> {
        let meta = self.meta();
        meta.scope = Some(scope.clone());
        let task_id = scope.executor().next_task_id();
        meta.id = task_id;
        scope.insert_task(self.0, false);
        JoinHandle::new(scope, task_id).into()
    }
}

impl<O> Future for SpawnableFuture<'_, O> {
    type Output = O;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // We cannot recover from panics.
        let bomb = Bomb;

        let meta = self.meta();
        let result = unsafe { (meta.vtable.poll)(meta.into(), cx) };

        std::mem::forget(bomb);

        ready!(result);

        let result = unsafe { ((meta.vtable.get_result)(meta.into()) as *const O).read() };
        *meta.state.get_mut() = DONE | RESULT_TAKEN;

        Poll::Ready(result)
    }
}

#[cfg(test)]
mod tests {
    use super::SpawnableFuture;
    use crate::{Scope, SendExecutor};
    use std::future::poll_fn;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;
    use std::task::Poll;

    #[test]
    fn test_spawnable_future() {
        let mut executor = SendExecutor::new(2);
        executor.run(async move {
            let counter = Arc::new(AtomicU64::new(0));
            let counter2 = Arc::clone(&counter);
            let mut task1 = SpawnableFuture::new(async move {
                let () = poll_fn(|_cx| {
                    if counter2.fetch_add(1, Relaxed) == 1 {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
            });
            let old = counter.load(Relaxed);
            assert!(futures::poll!(&mut task1).is_pending());
            assert_eq!(counter.load(Relaxed), old + 1);

            task1.spawn_on(Scope::current()).await;

            assert_eq!(counter.load(Relaxed), old + 2);
        });
    }
}
