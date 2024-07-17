// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{parse_unsigned_file, serialize_u64_file, BytesFile, BytesFileOps, FsNodeOps};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use std::borrow::Cow;

/// This file allows user space to put the system into a sleep state while taking into account the
/// concurrent arrival of wakeup events.
/// * Reading from it returns the current number of registered wakeup events and it blocks if some
/// wakeup events are being processed when the file is read from.
/// * Writing to it will only succeed if the current number of wakeup events is equal to the written
/// value and, if successful, will make the kernel abort a subsequent transition to a sleep state
/// if any wakeup events are reported after the write has returned.
pub struct PowerWakeupCountFile;

impl PowerWakeupCountFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for PowerWakeupCountFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let expected_count: u64 = parse_unsigned_file(&data)?;
        let real_count = current_task.kernel().suspend_resume_manager.suspend_stats().wakeup_count;
        if expected_count != real_count {
            return error!(EINVAL);
        }
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let wakeup_count =
            current_task.kernel().suspend_resume_manager.suspend_stats().wakeup_count;
        Ok(serialize_u64_file(wakeup_count).into())
    }
}
