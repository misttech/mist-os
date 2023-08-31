// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_async as fasync,
    futures::{lock::Mutex, Future},
    std::sync::{Arc, Weak},
};

/// A simple wrapper for `TaskGroup` that stores the `TaskGroup` in an `Arc` so it can be passed
/// between threads.
#[derive(Debug, Clone)]
pub struct TaskGroup {
    task_group: Arc<Mutex<Option<fasync::TaskGroup>>>,
}

impl TaskGroup {
    pub fn new() -> Self {
        Self { task_group: Arc::new(Mutex::new(Some(fasync::TaskGroup::new()))) }
    }

    /// Creates a new WeakTaskGroup from this group.
    pub fn as_weak(&self) -> WeakTaskGroup {
        WeakTaskGroup { task_group: Arc::downgrade(&self.task_group) }
    }

    /// Spawns a new task in this TaskGroup.
    ///
    /// If `join` has been called on a clone of this TaskGroup, `spawn` will drop the task instead.
    ///
    /// # Panics
    ///
    /// `spawn` may panic if not called in the context of an executor (e.g.
    /// within a call to `run` or `run_singlethreaded`).
    pub async fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let mut task_group = self.task_group.lock().await;
        if let Some(task_group) = task_group.as_mut() {
            task_group.spawn(future);
        }
    }

    /// Waits for all Tasks in this TaskGroup to finish. Prevents future tasks from being spawned
    /// if there's another task that holds a clone of this TaskGroup.
    pub async fn join(self) {
        let mut task_group_lock = self.task_group.lock().await;
        if let Some(task_group) = task_group_lock.take() {
            drop(task_group_lock);
            task_group.join().await;
        }
    }
}

/// Holds a weak reference to the internal `TaskGroup`, and can spawn futures on it as long as the
/// reference is still valid. If a task group is to hold a future that wants to spawn other tasks
/// on the same group, this future should hold a WeakTaskGroup so that there is no reference cycle
/// between the task group and tasks on the task group.
#[derive(Debug, Clone)]
pub struct WeakTaskGroup {
    task_group: Weak<Mutex<Option<fasync::TaskGroup>>>,
}

impl WeakTaskGroup {
    /// Adds a task to the group this WeakTaskGroup was created from. The task is dropped if there
    /// are no more strong references to the original task group.
    pub async fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        if let Some(task_group) = self.task_group.upgrade() {
            let temp_task_group = TaskGroup { task_group };
            temp_task_group.spawn(future).await;
        }
    }
}
