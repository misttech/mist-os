// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Task;

use futures::{channel::mpsc, future::BoxFuture, StreamExt};

/// Errors that can be returned by this crate.
#[derive(Debug, thiserror::Error)]
enum Error {
    /// Return when a task cannot be added to a [`TaskGroup`] or [`TaskSink`].
    #[error("Failed to add Task: {0}")]
    GroupDropped(#[from] mpsc::TrySendError<Task<()>>),
}

/// Allows the user to spawn multiple Tasks and await them as a unit.
///
/// Tasks can be added to this group using [`TaskGroup::add`].
/// All pending tasks in the group can be awaited using [`TaskGroup::join`].
pub struct TaskGroup {
    sink: TaskSink,
    // A future that waits for all tasks sent on `sink` to complete.
    // `sink` writes tasks to a channel and this future drains tasks from the channel
    // using an unbounded loop. Therefore, `sink` must be dropped to close the channel and
    // allow this future to complete.
    done: BoxFuture<'static, ()>,
}

impl TaskGroup {
    /// Creates a new TaskGroup.
    ///
    /// The TaskGroup can be used to await an arbitrary number of Tasks and may
    /// consume an arbitrary amount of memory.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded::<Task<()>>();
        let sink = TaskSink::new(tx);
        let done = Box::pin(async move {
            rx.for_each_concurrent(None, |task| task).await;
        });
        Self { sink, done }
    }

    /// Adds a Task to this TaskGroup.
    pub fn add(&mut self, task: Task<()>) {
        // This only panics if the receiver is closed. TaskSink is private, so the only
        // way to close the receiver is with TaskGroup::join() which consumes this TaskGroup.
        // Therefore this method can't be called after the receiver is closed and should
        // never panic.
        self.sink.try_add(task).unwrap();
    }

    /// Waits for all Tasks in this TaskGroup to finish.
    ///
    /// Call this only after all Tasks have been added.
    pub async fn join(self) {
        // Close the sink to ensure the receiving end of the channel terminates.
        drop(self.sink);
        self.done.await;
    }
}

/// Adds tasks to a remote [`TaskGroup`].
///
/// This sink must be dropped before the corresponding done future on the original
/// [`TaskGroup`] can complete.
struct TaskSink {
    sender: mpsc::UnboundedSender<Task<()>>,
}

impl TaskSink {
    fn new(sender: mpsc::UnboundedSender<Task<()>>) -> Self {
        Self { sender }
    }

    /// Adds a task to this sink.
    pub fn try_add(&mut self, task: Task<()>) -> Result<(), Error> {
        self.sender.unbounded_send(task).map_err(Error::GroupDropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SendExecutor;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    // Notifies a channel when dropped, signifying completion of some operation.
    #[derive(Clone)]
    struct DoneSignaler {
        done: mpsc::UnboundedSender<()>,
    }
    impl Drop for DoneSignaler {
        fn drop(&mut self) {
            self.done.unbounded_send(()).unwrap();
            self.done.disconnect();
        }
    }

    // Waits for a group of `DoneSignalers` to signal completion.
    // Create as many `DoneSignaler` objects as needed with `WaitGroup::add_one` and
    // call `wait` to wait for all of them to be dropped.
    struct WaitGroup {
        tx: mpsc::UnboundedSender<()>,
        rx: mpsc::UnboundedReceiver<()>,
    }

    impl WaitGroup {
        fn new() -> Self {
            let (tx, rx) = mpsc::unbounded();
            Self { tx, rx }
        }

        fn add_one(&self) -> DoneSignaler {
            DoneSignaler { done: self.tx.clone() }
        }

        async fn wait(self) {
            drop(self.tx);
            self.rx.collect::<()>().await;
        }
    }

    #[test]
    fn test_task_group_join_waits_for_tasks() {
        let task_count = 20;

        SendExecutor::new(task_count).run(async move {
            let mut task_group = TaskGroup::new();
            let value = Arc::new(AtomicU64::new(0));

            for _ in 0..task_count {
                let value = value.clone();
                let task = Task::spawn(async move {
                    value.fetch_add(1, Ordering::Relaxed);
                });
                task_group.add(task);
            }

            task_group.join().await;
            assert_eq!(value.load(Ordering::Relaxed), task_count as u64);
        });
    }

    #[test]
    fn test_task_group_empty_join_completes() {
        SendExecutor::new(1).run(async move {
            TaskGroup::new().join().await;
        });
    }

    #[test]
    fn test_task_group_added_tasks_are_cancelled_on_drop() {
        let wait_group = WaitGroup::new();
        let task_count = 10;

        SendExecutor::new(task_count).run(async move {
            let mut task_group = TaskGroup::new();
            for _ in 0..task_count {
                let done_signaler = wait_group.add_one();

                // Never completes but drops `done_signaler` when cancelled.
                let task = Task::spawn(async move {
                    // Take ownership of done_signaler.
                    let _done_signaler = done_signaler;
                    std::future::pending::<()>().await;
                });

                task_group.add(task);
            }

            drop(task_group);
            wait_group.wait().await;
            // If we get here, all tasks were cancelled.
        });
    }
}
