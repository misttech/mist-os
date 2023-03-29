// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    exec::{Lifecycle, Start, Stop},
    fuchsia_async as fasync,
    futures::{channel::oneshot, join, lock::Mutex, Future},
    std::sync::Arc,
    thiserror::Error,
};

pub struct Task<Fut>
where
    Fut: Future + Send + 'static,
    Fut::Output: Clone + Send + 'static,
{
    fut: Fut,
}

/// `RunningTask` represents an asynchronous task that is running, and will
/// produce `T` if ran to completion.
///
/// If the `RunningTask` is dropped, the wrapped asynchronous task will be
/// canceled as soon as possible.
pub struct RunningTask<T>
where
    T: Clone + Send + 'static,
{
    task: Option<fasync::Task<()>>,
    done: Mutex<ColdReceiver<T>>,
}

/// The reason the task exited:
///
/// - `Some(T)` means the task ran to completion with a return value.
/// - `None` means the task was canceled before it finished.
pub type ExitReason<T> = Option<T>;

/// A `Receiver<T>` that caches the result of the receive action.
enum ColdReceiver<T: Clone + Send> {
    Waiting(oneshot::Receiver<T>),
    Obtained(ExitReason<T>),
}

impl<T> ColdReceiver<T>
where
    T: Clone + Send + 'static,
{
    fn new(r: oneshot::Receiver<T>) -> ColdReceiver<T> {
        ColdReceiver::Waiting(r)
    }

    async fn recv(&mut self) -> Result<ExitReason<T>, AlreadyStopped> {
        match self {
            ColdReceiver::Waiting(receiver) => {
                let reason = match receiver.await {
                    Ok(value) => Some(value),
                    Err(_) => None,
                };
                *self = ColdReceiver::Obtained(reason.clone());
                return Ok(reason);
            }
            ColdReceiver::Obtained(reason) => Ok(reason.clone()),
        }
    }
}

/// `ArcError` is a convenient wrapper around an underlying error type to
/// implement `Clone`. One may use this to broadcast their task exit
/// information to others listening on the `on_exit` function.
#[derive(Error, Debug, Clone)]
#[error(transparent)]
pub struct ArcError(Arc<anyhow::Error>);

impl From<anyhow::Error> for ArcError {
    fn from(value: anyhow::Error) -> Self {
        ArcError(Arc::new(value))
    }
}

#[async_trait]
impl<Fut> Start for Task<Fut>
where
    Fut: Future + Send + 'static,
    Fut::Output: Clone + Send + 'static,
{
    type Error = AlreadyStopped;
    type Stop = RunningTask<Fut::Output>;

    async fn start(self) -> Result<Self::Stop, Self::Error> {
        let (mark_done, done) = oneshot::channel();
        Ok(RunningTask {
            task: Some(fasync::Task::spawn(async move {
                let output = self.fut.await;
                // If the receiver is dropped, that means the user has initiated
                // task cancellation. We may reach here if the task is being
                // canceled remotely. We ignore errors here since this task is
                // going away soon.
                let _ = mark_done.send(output);
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
impl<T> Stop for RunningTask<T>
where
    T: Clone + Send + 'static,
{
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
impl<T> Lifecycle for RunningTask<T>
where
    T: Clone + Send + 'static,
{
    type Exit = ExitReason<T>;

    async fn on_exit(&self) -> Result<Self::Exit, Self::Error> {
        self.done.lock().await.recv().await
    }
}

/// Creates a `Task` that runs the given asynchronous future on `start`.
/// The started runnable may be `stop`-ed to cancel the task.
pub fn create_task<Fut>(fut: Fut) -> Task<Fut>
where
    Fut: Future + Send + 'static,
    Fut::Output: Clone + Send + 'static,
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
        assert_eq!(on_exit_1?, Some(()));
        assert_eq!(on_exit_2?, Some(()));
        assert_eq!(on_exit_3?, Some(()));

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
        assert_eq!(on_exit_1?, None);
        assert_eq!(on_exit_2?, None);
        assert_eq!(on_exit_3?, None);
        Ok(())
    }

    #[derive(Error, Debug, Clone, PartialEq, Eq)]
    #[error("test error")]
    struct TestError(u64);

    #[fuchsia::test]
    async fn on_exit_ran_to_completion_with_error() -> Result<()> {
        let task = create_task(async move {
            let err: Result<(), ArcError> = Err(ArcError::from(anyhow::Error::from(TestError(42))));
            futures::future::ready(err).await?;
            Ok(())
        });
        let mut running_task = task.start().await?;
        let on_exit = running_task.on_exit().await?;
        let task_result: Result<(), ArcError> = on_exit.expect("must run to completion");
        let err = task_result.expect_err("task must fail with error");
        let test_error = err.0.downcast_ref::<TestError>().unwrap();
        assert_eq!(*test_error, TestError(42));

        running_task.stop().await?;
        Ok(())
    }
}
