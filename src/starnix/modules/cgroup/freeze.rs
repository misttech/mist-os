// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of `cgroup.freeze` file.
//!
//! This file only exists on non-root cgroups. Writing "1" freezes this cgroup all of its
//! descendants, writing "0" unfreezes the cgroup, but it can still be in a frozen state if any of
//! its ancestors are frozen. Reading cgroup.freeze produces the previously written value, or 0 by
//! default. cgroup.events should be used to check the effective freezer state of the cgroup.
//!
//! Full details at https://docs.kernel.org/admin-guide/cgroup-v2.html#core-interface-files

use std::borrow::Cow;
use std::sync::{Arc, Weak};

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{BytesFile, BytesFileOps, FsNodeOps};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};

use crate::cgroup::{Cgroup, CgroupOps, FreezerState};

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

pub struct FreezeFile {
    cgroup: Weak<Cgroup>,
}

impl FreezeFile {
    pub fn new_node(cgroup: Weak<Cgroup>) -> impl FsNodeOps {
        BytesFile::new_node(Self { cgroup })
    }

    fn cgroup(&self) -> Result<Arc<Cgroup>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl BytesFileOps for FreezeFile {
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
        let state_str = format!("{}\n", self.cgroup()?.get_freezer_state().self_freezer_state);
        Ok(state_str.as_bytes().to_owned().into())
    }
}
