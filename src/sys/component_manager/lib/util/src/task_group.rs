// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_async as fasync,
    futures::{lock::Mutex, Future},
    std::sync::Arc,
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
