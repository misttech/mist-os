// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::task::CurrentTask;
use crate::vfs::{BytesFile, BytesFileOps, FsNodeOps};

use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};
use std::borrow::Cow;

pub struct PowerWakeLockFile;

impl PowerWakeLockFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for PowerWakeLockFile {
    /// Writing a string activates a "wakeup source" preventing the system from
    /// entering a low-power state.
    ///
    /// 1. Simple string (no whitespace): Activates or creates a wakeup source with that name.
    /// 2. String with whitespace: The first part (before the whitespace) is the wakeup source name.
    ///    The second part is a timeout in nanoseconds, after which the wakeup source is
    ///    automatically deactivated.
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let lock_str = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        // TODO(https://fxbug.dev/322893982): Support the timeout option.
        let clean_lock_str = lock_str.split('\n').next().unwrap_or("");
        current_task.kernel().suspend_resume_manager.add_lock(clean_lock_str);
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let wake_locks = current_task.kernel().suspend_resume_manager.active_wake_locks();
        let content = wake_locks.join(" ") + "\n";
        Ok(content.as_bytes().to_owned().into())
    }
}

pub struct PowerWakeUnlockFile;

impl PowerWakeUnlockFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for PowerWakeUnlockFile {
    /// Writing a string to this file deactivates the wakeup source with that name.
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let lock_str = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let clean_lock_str = lock_str.split('\n').next().unwrap_or("");
        if !current_task.kernel().suspend_resume_manager.remove_lock(clean_lock_str) {
            return error!(EPERM);
        }
        Ok(())
    }

    /// Returns a space-separated list of inactive wakeup source names previously created
    /// via `PowerWakeLockFile`.
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let wake_locks = current_task.kernel().suspend_resume_manager.inactive_wake_locks();
        let content = wake_locks.join(" ") + "\n";
        Ok(content.as_bytes().to_owned().into())
    }
}
