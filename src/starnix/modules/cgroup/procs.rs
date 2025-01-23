// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of `cgroup.procs` file.
//!
//! Reading cgroup.procs produces all processes IDs that currently belong to the cgroup.
//! Writing a process ID to this file will move the process into this cgroup.
//!
//! Full details at https://docs.kernel.org/admin-guide/cgroup-v2.html#core-interface-files

use std::sync::{Arc, Weak};

use starnix_core::task::{CurrentTask, ProcessEntryRef};
use starnix_core::vfs::{
    AppendLockGuard, DynamicFile, DynamicFileBuf, DynamicFileSource, FileObject, FileOps, FsNode,
    FsNodeOps, InputBuffer,
};
use starnix_core::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, fs_node_impl_not_dir,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::ownership::TempRef;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, pid_t};

use crate::cgroup::CgroupOps;

pub struct ControlGroupNode {
    cgroup: Weak<dyn CgroupOps>,
}

impl ControlGroupNode {
    pub fn new(cgroup: Weak<dyn CgroupOps>) -> Self {
        ControlGroupNode { cgroup }
    }
}

impl FsNodeOps for ControlGroupNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ControlGroupFile::new(self.cgroup.clone())))
    }

    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        Ok(())
    }
}

struct ControlGroupFileSource {
    cgroup: Weak<dyn CgroupOps>,
}

impl ControlGroupFileSource {
    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl DynamicFileSource for ControlGroupFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let cgroup = self.cgroup()?;
        for pid in cgroup.get_pids() {
            write!(sink, "{pid}\n")?;
        }
        Ok(())
    }
}

/// A `ControlGroupFile` currently represents the `cgroup.procs` file for the control group. Writing
/// to this file will add tasks to the control group.
pub struct ControlGroupFile {
    cgroup: Weak<dyn CgroupOps>,
    dynamic_file: DynamicFile<ControlGroupFileSource>,
}

impl ControlGroupFile {
    fn new(cgroup: Weak<dyn CgroupOps>) -> Self {
        Self {
            cgroup: cgroup.clone(),
            dynamic_file: DynamicFile::new(ControlGroupFileSource { cgroup: cgroup.clone() }),
        }
    }

    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl FileOps for ControlGroupFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        let pid_string = std::str::from_utf8(&bytes).map_err(|_| errno!(EINVAL))?;
        let pid = pid_string.trim().parse::<pid_t>().map_err(|_| errno!(ENOENT))?;

        // Check if the pid is a valid task.
        let thread_group = if let Some(ProcessEntryRef::Process(thread_group)) =
            current_task.kernel().pids.read().get_process(pid)
        {
            TempRef::into_static(thread_group)
        } else {
            return error!(EINVAL);
        };

        self.cgroup()?.add_process(pid, &thread_group)?;

        Ok(bytes.len())
    }
}
