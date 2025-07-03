// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::{Kernel, TaskStateCode};
use starnix_core::vfs::pseudo::dynamic_file::{DynamicFile, DynamicFileBuf, DynamicFileSource};
use starnix_core::vfs::FsNodeOps;
use starnix_logging::track_stub;
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;

use std::sync::Weak;

#[derive(Clone)]
pub struct LoadavgFile(Weak<Kernel>);
impl LoadavgFile {
    pub fn new_node(kernel: &Kernel) -> impl FsNodeOps {
        DynamicFile::new_node(Self(kernel.weak_self.clone()))
    }
}

impl DynamicFileSource for LoadavgFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let (runnable_tasks, existing_tasks, last_pid) = {
            let kernel = self.0.upgrade().ok_or_else(|| errno!(EIO))?;
            let pid_table = kernel.pids.read();

            let curr_tids = pid_table.task_ids();
            let mut runnable_tasks = 0;
            for pid in &curr_tids {
                let weak_task = pid_table.get_task(*pid);
                if let Some(task) = weak_task.upgrade() {
                    if task.state_code() == TaskStateCode::Running {
                        runnable_tasks += 1;
                    }
                };
            }

            let existing_tasks = pid_table.process_ids().len() + curr_tids.len();
            (runnable_tasks, existing_tasks, pid_table.last_pid())
        };

        track_stub!(TODO("https://fxbug.dev/322874486"), "/proc/loadavg load stats");
        writeln!(sink, "0.50 0.50 0.50 {}/{} {}", runnable_tasks, existing_tasks, last_pid)?;
        Ok(())
    }
}
