// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    exec::{Lifecycle, Start, Stop},
    fuchsia_async as fasync,
    futures::{channel::oneshot, join, lock::Mutex, Future},
};

pub struct Task<Fut>
where
    Fut: Future<Output = ()> + Send + 'static,
{
    fut: Fut,
}

/// `RunningTask` represents an asynchronous task that is running.
///
/// If the `RunningTask` is dropped, the wrapped asynchronous task will be
/// canceled as soon as possible.
pub struct RunningTask {
    task: Option<fasync::Task<()>>,
    done: Mutex<ColdReceiver>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ExitReason {
    RanToCompletion,
    Canceled,
}

/// A `Receiver<()>` that caches the result of the receive action.
enum ColdReceiver {
    Waiting(oneshot::Receiver<()>),
    Obtained(ExitReason),
}

impl ColdReceiver {
    fn new(r: oneshot::Receiver<()>) -> ColdReceiver {
        ColdReceiver::Waiting(r)
    }

    async fn recv(&mut self) -> Result<ExitReason, AlreadyStopped> {
        match self {
            ColdReceiver::Waiting(receiver) => {
                let reason = match receiver.await {
                    Ok(()) => ExitReason::RanToCompletion,
                    Err(_) => ExitReason::Canceled,
                };
                *self = ColdReceiver::Obtained(reason);
                return Ok(reason);
            }
            ColdReceiver::Obtained(reason) => Ok(*reason),
        }
    }
}

#[async_trait]
impl<Fut> Start for Task<Fut>
where
    Fut: Future<Output = ()> + Send + 'static,
{
    type Error = AlreadyStopped;
    type Stop = RunningTask;

    async fn start(self) -> Result<Self::Stop, Self::Error> {
        let (mark_done, done) = oneshot::channel();
        Ok(RunningTask {
            task: Some(fasync::Task::spawn(async move {
                self.fut.await;
                // If the receiver is dropped, that means the user has initiated
                // task cancellation. We may reach here if the task is executing
                // on a `SendExecutor`. We ignore errors here since this task is
                // going away soon.
                let _ = mark_done.send(());
            })),
            done: Mutex::new(ColdReceiver::new(done)),
        })
    }
}

/// The task is already stopped, hence cannot be stopped again.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct AlreadyStopped {}

impl std::fmt::Display for AlreadyStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The Task was already stopped.")
    }
}

impl std::error::Error for AlreadyStopped {}

#[async_trait]
impl Stop for RunningTask {
    type Error = AlreadyStopped;

    async fn stop(&mut self) -> Result<(), Self::Error> {
        match self.task.take() {
            Some(t) => {
                t.cancel().await;
            }
            None => return Err(AlreadyStopped {}),
        };
        Ok(())
    }
}

#[async_trait]
impl Lifecycle for RunningTask {
    type Exit = ExitReason;

    async fn on_exit(&self) -> Result<Self::Exit, Self::Error> {
        self.done.lock().await.recv().await
    }
}

/// Creates a `Task` that runs the given asynchronous future on `start`.
/// The started runnable may be `stop`-ed to cancel the task.
pub fn create_task<Fut>(fut: Fut) -> Task<Fut>
where
    Fut: Future<Output = ()> + Send + 'static,
{
    Task { fut }
}

#[cfg(test)]
mod task_tests {
    use super::*;
    use anyhow::Result;
    use futures::channel::oneshot;

    #[fuchsia::test]
    async fn run_task() -> Result<()> {
        let (sender, receiver) = oneshot::channel::<i32>();
        let task = create_task(async move {
            sender.send(42).expect("sending one value must succeed");
        });
        let mut running_task = task.start().await?;
        assert_eq!(receiver.await.expect("must not be canceled"), 42);
        running_task.stop().await?;
        Ok(())
    }

    #[fuchsia::test]
    async fn stop_twice() -> Result<()> {
        let task = create_task(async move {
            // Block forever.
            futures::future::pending::<()>().await;
        });
        let mut running_task = task.start().await?;

        // Stop the task which must be still running.
        running_task.stop().await?;

        // Stopping a second time is an error.
        assert_eq!(running_task.stop().await, Err(AlreadyStopped {}));

        Ok(())
    }

    #[fuchsia::test]
    async fn on_exit() -> Result<()> {
        let task = create_task(async move {});
        let mut running_task = task.start().await?;

        // Start many watchers.
        let on_exit_1 = running_task.on_exit();
        let on_exit_2 = running_task.on_exit();
        let on_exit_3 = running_task.on_exit();

        // They should all run to completion.
        let (on_exit_1, on_exit_2, on_exit_3) = join!(on_exit_1, on_exit_2, on_exit_3);
        assert_eq!(on_exit_1?, ExitReason::RanToCompletion);
        assert_eq!(on_exit_2?, ExitReason::RanToCompletion);
        assert_eq!(on_exit_3?, ExitReason::RanToCompletion);

        // This should be no-op since the async task already exited itself.
        running_task.stop().await?;
        Ok(())
    }

    #[fuchsia::test]
    async fn on_exit_canceled() -> Result<()> {
        let task = create_task(async move {
            // Block forever.
            futures::future::pending::<()>().await;
        });
        let mut running_task = task.start().await?;

        // Stop the task which must be still running.
        running_task.stop().await?;

        // Start many watchers.
        let on_exit_1 = running_task.on_exit();
        let on_exit_2 = running_task.on_exit();
        let on_exit_3 = running_task.on_exit();

        // They should all be canceled.
        let (on_exit_1, on_exit_2, on_exit_3) = join!(on_exit_1, on_exit_2, on_exit_3);
        assert_eq!(on_exit_1?, ExitReason::Canceled);
        assert_eq!(on_exit_2?, ExitReason::Canceled);
        assert_eq!(on_exit_3?, ExitReason::Canceled);
        Ok(())
    }
}
