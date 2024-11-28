// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::TargetEvent;
use anyhow::Result;
use emulator_instance::EmulatorInstances;
use fuchsia_async::Task;
use futures::channel::mpsc::UnboundedSender;
use std::path::PathBuf;

pub(crate) struct EmulatorWatcher {
    // Task for the drain loop
    drain_task: Option<Task<()>>,
}

impl EmulatorWatcher {
    pub(crate) fn new(
        instance_root: PathBuf,
        sender: UnboundedSender<Result<TargetEvent>>,
    ) -> Result<Self> {
        let emu_instances = EmulatorInstances::new(instance_root.clone());
        let existing = emulator_instance::get_all_targets(&emu_instances)?;
        for i in existing {
            let handle = i.try_into();
            if let Ok(h) = handle {
                let _ = sender.unbounded_send(Ok(TargetEvent::Added(h)));
            }
        }
        let mut res = Self { drain_task: None };

        // Emulator (and therefore notify thread) lifetime should last as long as the task,
        // because it is moved into the loop
        let mut watcher = emulator_instance::start_emulator_watching(instance_root)?;
        let task = Task::local(async move {
            loop {
                if let Some(act) = watcher.emulator_target_detected().await {
                    let event = act.try_into();
                    if let Ok(e) = event {
                        let _ = sender.unbounded_send(Ok(e));
                    }
                }
            }
        });
        res.drain_task.replace(task);
        Ok(res)
    }
}
