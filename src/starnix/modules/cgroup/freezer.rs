// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Cow;
use std::sync::Weak;

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{BytesFile, BytesFileOps, FsNodeOps};
use starnix_sync::Mutex;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};

use crate::cgroup::{Cgroup, CgroupOps};

#[derive(Clone, Debug)]
enum FreezerState {
    Frozen,
    Thawed,
}

impl std::fmt::Display for FreezerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezerState::Frozen => write!(f, "1"),
            FreezerState::Thawed => write!(f, "0"),
        }
    }
}

pub struct FreezerFile {
    state: Mutex<FreezerState>,
    cgroup: Weak<Cgroup>,
}

impl FreezerFile {
    pub fn new_node(cgroup: Weak<Cgroup>) -> impl FsNodeOps {
        BytesFile::new_node(Self { state: Mutex::new(FreezerState::Thawed), cgroup })
    }
}

impl BytesFileOps for FreezerFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let state_str = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let state = match state_str.trim() {
            "1" => FreezerState::Frozen,
            "0" => FreezerState::Thawed,
            _ => return error!(EINVAL),
        };
        let cgroup = self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))?;
        match state {
            FreezerState::Frozen => {
                cgroup.freeze(current_task)?;
            }
            FreezerState::Thawed => {
                cgroup.thaw();
            }
        }
        *self.state.lock() = state;

        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let state_str = format!("{}\n", self.state.lock());
        Ok(state_str.as_bytes().to_owned().into())
    }
}
