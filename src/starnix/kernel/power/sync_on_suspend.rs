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

pub struct PowerSyncOnSuspendFile;

impl PowerSyncOnSuspendFile {
    pub fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self {}))
    }
}

impl FileOps for PowerSyncOnSuspendFile {
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
        let enable_sync_on_suspend = std::str::from_utf8(&bytes)
            .map_err(|_| errno!(EINVAL))?
            .trim()
            .parse::<u32>()
            .map_err(|_| errno!(EINVAL))?;
        match enable_sync_on_suspend {
            0 | 1 => current_task
                .kernel()
                .suspend_resume_manager
                .set_sync_on_suspend(enable_sync_on_suspend != 0),
            _ => return error!(EINVAL),
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
        let content = format!(
            "{}\n",
            if current_task.kernel().suspend_resume_manager.sync_on_suspend_enabled() {
                "1"
            } else {
                "0"
            }
        );
        data.write(content[offset..].as_bytes())
    }
}
