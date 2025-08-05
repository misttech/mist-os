// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task_metrics::measurement::Measurement;
use crate::task_metrics::runtime_stats_source::RuntimeStatsSource;
use crate::task_metrics::task_info::TaskInfo;
use fuchsia_inspect as inspect;
use fuchsia_sync::Mutex;
use std::fmt::Debug;
use std::sync::Arc;

/// Tracks the tasks associated to some component and provides utilities for measuring them.
pub struct ComponentStats<T: RuntimeStatsSource + Debug> {
    tasks: Vec<Arc<Mutex<TaskInfo<T>>>>,
}

impl<T: 'static + RuntimeStatsSource + Debug + Send + Sync> ComponentStats<T> {
    /// Creates a new `ComponentStats` and starts taking measurements.
    pub fn new() -> Self {
        Self { tasks: vec![] }
    }

    /// Associate a task with this component.
    pub fn add_task(&mut self, task: Arc<Mutex<TaskInfo<T>>>) {
        self.tasks.push(task);
    }

    /// A `ComponentStats` is alive when:
    /// - It has not started measuring yet: this means we are still waiting for the diagnostics
    ///   data to arrive from the runner, or
    /// - Any of its tasks are alive.
    pub fn is_alive(&self) -> bool {
        let mut any_task_alive = false;
        for task in &self.tasks {
            if task.lock().is_alive() {
                any_task_alive = true;
            }
        }
        any_task_alive
    }

    /// Takes a runtime info measurement and records it. Drops old ones if the maximum amount is
    /// exceeded.
    ///
    /// Applies to tasks which have TaskState::Alive or TaskState::Terminated.
    pub fn measure(&mut self) -> Measurement {
        let mut result = Measurement::default();
        for task in self.tasks.iter_mut() {
            if let Some(measurement) = task.lock().measure_if_no_parent() {
                result += measurement;
            }
        }

        result
    }

    /// This produces measurements for tasks which have TaskState::TerminatedAndMeasured
    /// but also have measurement data for the past hour.
    pub fn measure_tracked_dead_tasks(&self) -> Measurement {
        let mut result = Measurement::default();

        for task in self.tasks.iter() {
            let locked_task = task.lock();

            // this implies that `clean_stale()` will take the measurement
            if locked_task.measurements.no_true_measurements() {
                continue;
            }

            if let Some(m) = locked_task.exited_cpu() {
                result += m;
            }
        }

        result
    }

    /// Removes all tasks that are not alive.
    ///
    /// Returns the koids of the ones that were deleted and the sum of the final measurements
    /// of the dead tasks. The measurement produced is of Tasks with
    /// TaskState::TerminatedAndMeasured.
    pub fn clean_stale(&mut self) -> (Vec<zx::sys::zx_koid_t>, Measurement) {
        let mut deleted_koids = vec![];
        let mut final_tasks = vec![];
        let mut exited_cpu_time = Measurement::default();
        while let Some(task) = self.tasks.pop() {
            let (is_alive, koid) = {
                let task_guard = task.lock();
                (task_guard.is_alive(), task_guard.koid())
            };

            if is_alive {
                final_tasks.push(task);
            } else {
                deleted_koids.push(koid);
                let locked_task = task.lock();
                if let Some(m) = locked_task.exited_cpu() {
                    exited_cpu_time += m;
                }
            }
        }
        self.tasks = final_tasks;
        (deleted_koids, exited_cpu_time)
    }

    pub fn remove_by_koids(&mut self, remove: &[zx::sys::zx_koid_t]) {
        let mut final_tasks = vec![];
        while let Some(task) = self.tasks.pop() {
            let task_koid = task.lock().koid();
            if !remove.contains(&task_koid) {
                final_tasks.push(task)
            }
        }

        self.tasks = final_tasks;
    }

    pub fn gather_dead_tasks(&self) -> Vec<(zx::BootInstant, Arc<Mutex<TaskInfo<T>>>)> {
        let mut dead_tasks = vec![];
        for task in &self.tasks {
            if let Some(t) = task.lock().most_recent_measurement() {
                dead_tasks.push((t, task.clone()));
            }
        }

        dead_tasks
    }

    /// Writes the stats to inspect under the given node. Returns the number of tasks that were
    /// written.
    pub fn record_to_node(&self, node: &inspect::Node) -> u64 {
        for task in &self.tasks {
            task.lock().record_to_node(&node);
        }
        self.tasks.len() as u64
    }

    #[cfg(test)]
    pub fn total_measurements(&self) -> usize {
        let mut sum = 0;
        for task in &self.tasks {
            sum += task.lock().total_measurements();
        }
        sum
    }

    #[cfg(test)]
    pub fn tasks_mut(&mut self) -> &mut [Arc<Mutex<TaskInfo<T>>>] {
        &mut self.tasks
    }
}
