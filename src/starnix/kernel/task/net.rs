// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{fs::proc::ProcSysNetDev, fs::*, lock::Mutex, task::*, types::*};
use std::{collections::HashMap, sync::Arc};

struct NetstackDevice {
    /// The device-specific directories that are found under `/proc/sys/net`.
    proc_sys_net: ProcSysNetDev,
}

/// Keeps track of network devices and their [`NetstackDevice`].
#[derive(Default)]
pub struct NetstackDevices {
    entries: Mutex<HashMap<FsString, NetstackDevice>>,
}

impl NetstackDevices {
    pub fn add_dev(&self, name: &str, proc_fs: Option<&FileSystemHandle>) {
        // procfs may not be mounted.
        if let Some(proc_fs) = proc_fs {
            let mut entries = self.entries.lock();
            entries
                .insert(name.into(), NetstackDevice { proc_sys_net: ProcSysNetDev::new(proc_fs) });
        }
    }

    pub fn remove_dev(&self, name: &str) {
        let mut entries = self.entries.lock();
        let name: FsString = name.into();
        let _: Option<NetstackDevice> = entries.remove(&name);
    }
}

/// An implementation of a directory holding netstack interface-specific
/// directories such as those found under `/proc/sys/net`.
pub struct NetstackDevicesDirectory {
    inner: Arc<NetstackDevices>,
    dir_fn: fn(&NetstackDevice) -> &Arc<FsNode>,
}

impl NetstackDevicesDirectory {
    pub fn new_proc_sys_net_ipv4_neigh(inner: Arc<NetstackDevices>) -> Arc<Self> {
        Self::new(inner, |d| ProcSysNetDev::get_ipv4_neigh(&d.proc_sys_net))
    }

    pub fn new_proc_sys_net_ipv6_conf(inner: Arc<NetstackDevices>) -> Arc<Self> {
        Self::new(inner, |d| ProcSysNetDev::get_ipv6_conf(&d.proc_sys_net))
    }

    pub fn new_proc_sys_net_ipv6_neigh(inner: Arc<NetstackDevices>) -> Arc<Self> {
        Self::new(inner, |d| ProcSysNetDev::get_ipv6_neigh(&d.proc_sys_net))
    }

    fn new(inner: Arc<NetstackDevices>, dir_fn: fn(&NetstackDevice) -> &Arc<FsNode>) -> Arc<Self> {
        Arc::new(Self { inner, dir_fn })
    }
}

impl FsNodeOps for Arc<NetstackDevicesDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        let entries = self.inner.entries.lock();
        entries.get(name).map(self.dir_fn).map(Arc::clone).ok_or_else(|| {
            errno!(
                ENOENT,
                format!(
                    "looking for {:?} in {:?}",
                    String::from_utf8_lossy(name),
                    entries
                        .keys()
                        .map(|e| String::from_utf8_lossy(e).to_string())
                        .collect::<Vec<String>>()
                )
            )
        })
    }
}

impl FileOps for Arc<NetstackDevicesDirectory> {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        unbounded_seek(current_offset, target)
    }

    fn readdir(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Skip through the entries until the current offset is reached.
        // Subtract 2 from the offset to account for `.` and `..`.
        let entries = self.inner.entries.lock();
        for (name, node) in entries.iter().skip(sink.offset() as usize - 2) {
            let node = (self.dir_fn)(node);
            sink.add(
                node.node_id,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(node.info().mode),
                name,
            )?;
        }
        Ok(())
    }
}
