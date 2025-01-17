// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Cow;
use std::sync::{Arc, Weak};

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{BytesFile, BytesFileOps, FsNodeOps};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};

use crate::cgroup::{Cgroup, CgroupOps};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FreezerState {
    Thawed,
    Frozen,
}

impl std::fmt::Display for FreezerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezerState::Frozen => write!(f, "1"),
            FreezerState::Thawed => write!(f, "0"),
        }
    }
}

impl Default for FreezerState {
    fn default() -> Self {
        FreezerState::Thawed
    }
}

pub struct FreezerFile {
    cgroup: Weak<Cgroup>,
}

impl FreezerFile {
    pub fn new_node(cgroup: Weak<Cgroup>) -> impl FsNodeOps {
        BytesFile::new_node(Self { cgroup })
    }

    fn cgroup(&self) -> Result<Arc<Cgroup>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl BytesFileOps for FreezerFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let state_str = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let cgroup = self.cgroup()?;
        match state_str.trim() {
            "1" => Ok(cgroup.freeze()),
            "0" => Ok(cgroup.thaw()),
            _ => error!(EINVAL),
        }
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let state_str = format!("{}\n", self.cgroup()?.get_status().self_freezer_state);
        Ok(state_str.as_bytes().to_owned().into())
    }
}
