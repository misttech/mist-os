// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::socket_tunnel_file::SocketTunnelFile;
use starnix_core::device::kobject::Device;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    fs_node_impl_dir_readonly, DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;

pub struct NanohubCommsDirectory {
    base_dir: DeviceDirectory,
}

impl NanohubCommsDirectory {
    pub fn new(device: Device) -> Self {
        Self { base_dir: DeviceDirectory::new(device) }
    }

    fn create_file_ops_entries() -> Vec<VecDirectoryEntry> {
        let mut entries = DeviceDirectory::create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"firmware_name".into(),
            inode: None,
        });
        entries
    }
}

impl FsNodeOps for NanohubCommsDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(Self::create_file_ops_entries()))
    }

    fn lookup(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            b"firmware_name" => Ok(node.fs().create_node(
                current_task,
                SocketTunnelFile::new(
                    b"/sys/devices/virtual/nanohub/nanohub_comms/firmware_name".into(),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o440), FsCred::root()),
            )),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}
