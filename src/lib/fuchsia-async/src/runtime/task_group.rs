// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Task;

use futures::channel::mpsc;
use futures::Future;

use super::Scope;

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
///
/// New code should prefer to use [`Scope`] instead.
pub struct TaskGroup {
    scope: Scope,
}

impl Default for TaskGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskGroup {
    /// Creates a new TaskGroup.
    ///
    /// The TaskGroup can be used to await an arbitrary number of Tasks and may
    /// consume an arbitrary amount of memory.
    pub fn new() -> Self {
        Self { scope: Scope::new() }
    }

    /// Spawns a new task in this TaskGroup.
    ///
    /// To add a future that is not [`Send`] to this TaskGroup, use [`TaskGroup::local`].
    ///
    /// # Panics
    ///
    /// `spawn` may panic if not called in the context of an executor (e.g.
    /// within a call to `run` or `run_singlethreaded`).
    pub fn spawn(&mut self, future: impl Future<Output = ()> + Send + 'static) {
        self.scope.spawn(future);
    }

    /// Spawns a new task in this TaskGroup.
    ///
    /// # Panics
    ///
    /// `spawn` may panic if not called in the context of a single threaded executor
    /// (e.g. within a call to `run_singlethreaded`).
    pub fn local(&mut self, future: impl Future<Output = ()> + 'static) {
        self.scope.spawn_local(future);
    }

    /// Waits for all Tasks in this TaskGroup to finish.
    ///
    /// Call this only after all Tasks have been added.
    pub async fn join(self) {
        self.scope.on_no_tasks().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SendExecutor;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

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

    // Waits for a group of `impl Drop` to signal completion.
    // Create as many `impl Drop` objects as needed with `WaitGroup::add_one` and
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

        fn add_one(&self) -> impl Drop {
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
                task_group.spawn(async move {
                    value.fetch_add(1, Ordering::Relaxed);
                });
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
                task_group.spawn(async move {
                    // Take ownership of done_signaler.
                    let _done_signaler = done_signaler;
                    std::future::pending::<()>().await;
                });
            }

            drop(task_group);
            wait_group.wait().await;
            // If we get here, all tasks were cancelled.
        });
    }

    #[test]
    fn test_task_group_spawn() {
        let task_count = 3;
        SendExecutor::new(task_count).run(async move {
            let mut task_group = TaskGroup::new();

            // We can spawn tasks from any Future<()> implementation, including...

            // ... naked futures.
            task_group.spawn(std::future::ready(()));

            // ... futures returned from async blocks.
            task_group.spawn(async move {
                std::future::ready(()).await;
            });

            // ... and other tasks.
            task_group.spawn(Task::spawn(std::future::ready(())));

            task_group.join().await;
        });
    }
}
