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
use starnix_sync::{Locked, ReadOps, WriteOps};
use starnix_uapi::{errno, error, errors::Errno};

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub enum SuspendState {
    /// Suspend-to-Ram
    ///
    /// This state, if supported, offers significant power savings as everything in the system is
    /// put into a low-power state, except for memory.
    Ram,
    /// Suspend-To-Idle
    ///
    /// This state is a generic, pure software, light-weight, system sleep state.
    Idle,
    /// Suspend-to-disk
    Disk,
    /// Power-On Suspend
    Standby,
}

impl SuspendState {
    fn to_str(&self) -> &'static str {
        match self {
            SuspendState::Ram => "mem",
            SuspendState::Idle => "freeze",
            SuspendState::Disk => "disk",
            SuspendState::Standby => "standby",
        }
    }
}

impl TryFrom<&str> for SuspendState {
    type Error = Errno;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Ok(match value {
            "mem" => SuspendState::Ram,
            "freeze" => SuspendState::Idle,
            "disk" => SuspendState::Disk,
            "standby" => SuspendState::Standby,
            _ => return error!(EINVAL),
        })
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

        let power_manager = &current_task.kernel().power_manager;
        let supported_states = power_manager.suspend_states();
        if !supported_states.contains(&state) {
            return error!(EINVAL);
        }
        power_manager.suspend(state)?;

        Ok(bytes.len())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, ReadOps>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let states = current_task.kernel().power_manager.suspend_states();
        let content = states.iter().map(SuspendState::to_str).join(" ") + "\n";
        data.write(content[offset..].as_bytes())
    }
}
