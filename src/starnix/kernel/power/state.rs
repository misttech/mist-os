// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use itertools::Itertools;

use crate::{
    task::CurrentTask,
    vfs::{
        fileops_impl_seekable, FileObject, FileOps, FsNodeOps, InputBuffer, OutputBuffer,
        SimpleFileNode,
    },
};
use fidl_fuchsia_power_broker::PowerLevel;
use starnix_sync::{FileOpsCore, Locked, WriteOps};
use starnix_uapi::{errno, error, errors::Errno};

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub enum SuspendState {
    /// Suspend-to-disk
    ///
    /// This state offers the greatest energy savings.
    Disk,
    /// Suspend-to-Ram
    ///
    /// This state, if supported, offers significant power savings as everything in the system is
    /// put into a low-power state, except for memory.
    Ram,
    /// Standby
    ///
    /// This state, if supported, offers moderate, but real, energy savings, while providing a
    /// relatively straightforward transition back to the working state.
    ///
    Standby,
    /// Suspend-To-Idle
    ///
    /// This state is a generic, pure software, light-weight, system sleep state.
    Idle,
}

impl SuspendState {
    fn to_str(&self) -> &'static str {
        match self {
            SuspendState::Disk => "disk",
            SuspendState::Ram => "mem",
            SuspendState::Idle => "freeze",
            SuspendState::Standby => "standby",
        }
    }
}

impl TryFrom<&str> for SuspendState {
    type Error = Errno;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Ok(match value {
            "disk" => SuspendState::Disk,
            "mem" => SuspendState::Ram,
            "standby" => SuspendState::Standby,
            "freeze" => SuspendState::Idle,
            _ => return error!(EINVAL),
        })
    }
}

impl From<SuspendState> for PowerLevel {
    fn from(value: SuspendState) -> Self {
        match value {
            SuspendState::Disk => 0,
            SuspendState::Ram => 1,
            SuspendState::Standby => 2,
            SuspendState::Idle => 3,
        }
    }
}

pub struct PowerStateFile;

impl PowerStateFile {
    pub fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self {}))
    }
}

impl FileOps for PowerStateFile {
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
        let state_str = std::str::from_utf8(&bytes).map_err(|_| errno!(EINVAL))?;
        let state: SuspendState = state_str.try_into()?;

        let power_manager = &current_task.kernel().suspend_resume_manager;
        let supported_states = power_manager.suspend_states();
        if !supported_states.contains(&state) {
            return error!(EINVAL);
        }
        power_manager.suspend(state)?;

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
        let states = current_task.kernel().suspend_resume_manager.suspend_states();
        let content = states.iter().map(SuspendState::to_str).join(" ") + "\n";
        data.write(content[offset..].as_bytes())
    }
}
