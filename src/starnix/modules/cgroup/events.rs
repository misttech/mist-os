// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of `cgroup.events` file.
//!
//! This file is read-only, and only exists on non-root cgroups. Reading cgroup.events produces
//! whether the cgroup or any of its descendants are populated, and whether the cgroup or any of its
//! ancestors are frozen (referred to as the effective freezer state).
//!
//! Full details at https://docs.kernel.org/admin-guide/cgroup-v2.html#core-interface-files

use std::borrow::Cow;
use std::sync::{Arc, Weak};

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    FileObject, FileOps, FsNodeOps, InputBuffer, OutputBuffer, SimpleFileNode,
};
use starnix_core::{fileops_impl_noop_sync, fileops_impl_seekable};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::vfs::FdEvents;

use crate::cgroup::{Cgroup, CgroupOps};

pub struct EventsFile {
    cgroup: Weak<Cgroup>,
}

impl EventsFile {
    pub fn new_node(cgroup: Weak<Cgroup>) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self { cgroup: cgroup.clone() }))
    }

    fn cgroup(&self) -> Result<Arc<Cgroup>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl FileOps for EventsFile {
    fileops_impl_seekable!();
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        starnix_uapi::error!(EINVAL)
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let cgroup = self.cgroup()?;
        let events_str = format!(
            "populated {}\nfrozen {}\n",
            cgroup.is_populated() as u8,
            cgroup.get_freezer_state().effective_freezer_state
        );
        let content: Cow<'_, [u8]> = events_str.as_bytes().to_owned().into();
        if offset >= content.len() {
            return Ok(0);
        }
        data.write(&content[offset..])
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::POLLPRI | FdEvents::POLLERR)
    }
}
