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
    /// To add a future that is not [`Send`] to this TaskGroup, use [`TaskGroup::add`].
    ///
    /// # Panics
    ///
    /// `spawn` may panic if not called in the context of an executor (e.g.
    /// within a call to `run` or `run_singlethreaded`).
    pub async fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let mut task_group = self.task_group.lock().await;
        task_group.as_mut().unwrap().spawn(future);
    }

    /// Waits for all Tasks in this TaskGroup to finish.
    ///
    /// Call this only after all Tasks have been added.
    pub async fn join(self) {
        let mut task_group = self.task_group.lock().await;
        let task_group = task_group.take().unwrap();
        task_group.join().await;
    }
}
