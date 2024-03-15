// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        fileops_impl_seekable, FileObject, FileOps, FsNodeOps, InputBuffer, OutputBuffer,
        SimpleFileNode,
    },
};
use starnix_sync::{FileOpsCore, Locked, WriteOps};
use starnix_uapi::{errno, error, errors::Errno};

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
        SimpleFileNode::new(move || Ok(Self {}))
    }
}

impl FileOps for PowerWakeupCountFile {
    fileops_impl_seekable!();

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        let bytes = data.read_all()?;
        let expected_count = std::str::from_utf8(&bytes)
            .map_err(|_| errno!(EINVAL))?
            .trim()
            .parse::<u64>()
            .map_err(|_| errno!(EINVAL))?;
        let real_count = current_task.kernel().suspend_resume_manager.suspend_stats().wakeup_count;
        if expected_count != real_count {
            return error!(EINVAL);
        }

        Ok(bytes.len())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let wakeup_count =
            current_task.kernel().suspend_resume_manager.suspend_stats().wakeup_count;
        let content = format!("{}\n", wakeup_count);
        data.write(content[offset..].as_bytes())
    }
}
