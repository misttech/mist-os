// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::proc::ProcSysNetDev;
use crate::task::CurrentTask;
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    unbounded_seek, DirectoryEntryType, DirentSink, FileObject, FileOps, FileSystemHandle, FsNode,
    FsNodeHandle, FsNodeOps, FsStr, FsString, SeekTarget, StaticDirectoryBuilder,
};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, off_t};
use std::collections::HashMap;
use std::sync::Arc;

struct NetstackDevice {
    /// The device-specific directories that are found under `/proc/sys/net`.
    proc_sys_net: Option<ProcSysNetDev>,
    /// The device-specific node found under `/sys/class.net`.
    sys_class_net: Option<FsNodeHandle>,
}

/// Keeps track of network devices and their [`NetstackDevice`].
#[derive(Default)]
pub struct NetstackDevices {
    entries: Mutex<HashMap<FsString, NetstackDevice>>,
}

impl NetstackDevices {
    pub fn add_dev(
        &self,
        current_task: &CurrentTask,
        name: &str,
        proc_fs: Option<&FileSystemHandle>,
        sys_fs: Option<&FileSystemHandle>,
    ) {
        // procfs or sysfs may not be mounted.
        let proc_sys_net = proc_fs.map(|fs| ProcSysNetDev::new(current_task, fs));
        let sys_class_net = sys_fs.map(|sys_fs| {
            // nodes in `/sys/class/net` are normally symlinks into
            // `/sys/devices`. However, currently known use-cases only enumerate
            // the nodes in `/sys/class/net` so we enable that use-case with the
            // workaround here where we create empty directories.
            track_stub!(TODO("https://fxbug.dev/297438880"), "/sys/class/net");
            StaticDirectoryBuilder::new(sys_fs).build(current_task)
        });

        let mut entries = self.entries.lock();
        entries.insert(name.into(), NetstackDevice { proc_sys_net, sys_class_net });
    }

    pub fn remove_dev(&self, name: &str) {
        let mut entries = self.entries.lock();
        let name: FsString = name.into();
        let _: Option<NetstackDevice> = entries.remove(&name);
    }
}

/// An implementation of a directory holding netstack interface-specific
/// directories such as those found under `/proc/sys/net` and `/sys/class/net`.
pub struct NetstackDevicesDirectory {
    dir_fn: fn(&NetstackDevice) -> Option<&FsNodeHandle>,
}

impl NetstackDevicesDirectory {
    pub fn new_proc_sys_net_ipv4_conf() -> Arc<Self> {
        Self::new(|d| d.proc_sys_net.as_ref().map(ProcSysNetDev::get_ipv4_conf))
    }

    pub fn new_proc_sys_net_ipv4_neigh() -> Arc<Self> {
        Self::new(|d| d.proc_sys_net.as_ref().map(ProcSysNetDev::get_ipv4_neigh))
    }

    pub fn new_proc_sys_net_ipv6_conf() -> Arc<Self> {
        Self::new(|d| d.proc_sys_net.as_ref().map(ProcSysNetDev::get_ipv6_conf))
    }

    pub fn new_proc_sys_net_ipv6_neigh() -> Arc<Self> {
        Self::new(|d| d.proc_sys_net.as_ref().map(ProcSysNetDev::get_ipv6_neigh))
    }

    pub fn new_sys_class_net() -> Arc<Self> {
        Self::new(|d| d.sys_class_net.as_ref())
    }

    fn new(dir_fn: fn(&NetstackDevice) -> Option<&FsNodeHandle>) -> Arc<Self> {
        Arc::new(Self { dir_fn })
    }
}

impl FsNodeOps for Arc<NetstackDevicesDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let kernel = current_task.kernel();
        let entries = kernel.netstack_devices.entries.lock();
        entries.get(name).and_then(self.dir_fn).map(Arc::clone).ok_or_else(|| {
            errno!(
                ENOENT,
                format!(
                    "looking for {name} in {:?}",
                    entries.keys().map(ToString::to_string).collect::<Vec<_>>()
                )
            )
        })
    }
}

impl FileOps for NetstackDevicesDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        unbounded_seek(current_offset, target)
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Skip through the entries until the current offset is reached.
        // Subtract 2 from the offset to account for `.` and `..`.
        let kernel = current_task.kernel();
        let entries = kernel.netstack_devices.entries.lock();
        for (name, node) in entries.iter().skip(sink.offset() as usize - 2) {
            let Some(node) = (self.dir_fn)(node) else { continue };
            sink.add(
                node.node_id,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(node.info().mode),
                name.as_ref(),
            )?;
        }
        Ok(())
    }
}
