// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::TargetEvent;
use anyhow::Result;
use fastboot_file_discovery::{get_fastboot_devices, FastbootFileWatcher};
use futures::channel::mpsc::UnboundedSender;
use std::path::PathBuf;

pub(crate) struct FastbootWatcher {
    // Task for the drain loop
    _watcher: FastbootFileWatcher,
}

impl FastbootWatcher {
    pub(crate) fn new(
        instance_root: PathBuf,
        sender: UnboundedSender<TargetEvent>,
    ) -> Result<Self> {
        let existing = get_fastboot_devices(&instance_root)?;
        for device in existing {
            let event = fastboot_file_discovery::FastbootEvent::Discovered(device);
            let handle = event.into();
            let _ = sender.unbounded_send(handle);
        }

        // FastbootFile (and therefore notify thread) lifetime should last as long as the task,
        // because it is moved into the loop
        let watcher = fastboot_file_discovery::recommended_watcher(
            move |res: fastboot_file_discovery::FastbootEvent| {
                // Translate the result to a TargetEvent
                log::trace!("discovery watcher got fastboot file event: {:#?}", res);
                let event = res.into();
                let _ = sender.unbounded_send(event);
            },
            instance_root,
        )?;
        Ok(Self { _watcher: watcher })
    }
}
