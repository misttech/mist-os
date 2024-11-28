// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{Device, KObjectBased, KObjectHandle, UEventFsNode};
use crate::task::CurrentTask;
use crate::vfs::buffers::InputBuffer;
use crate::vfs::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    fs_node_impl_not_dir, BytesFile, DirectoryEntryType, DynamicFile, DynamicFileBuf,
    DynamicFileSource, FileObject, FileOps, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    StubEmptyFile, VecDirectory, VecDirectoryEntry, DEFAULT_BYTES_PER_BLOCK,
};
use starnix_logging::{bug_ref, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};
use std::sync::Weak;

pub struct DeviceDirectory {
    device: Device,
}

impl DeviceDirectory {
    pub fn new(device: Device) -> Self {
        Self { device }
    }

    fn device_type(&self) -> DeviceType {
        self.device.metadata.device_type
    }

    pub fn create_file_ops_entries() -> Vec<VecDirectoryEntry> {
        // TODO(https://fxbug.dev/42072346): Add power and subsystem nodes.
        vec![
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: "dev".into(),
                inode: None,
            },
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: "uevent".into(),
                inode: None,
            },
        ]
    }

    pub fn kobject(&self) -> KObjectHandle {
        self.device.kobject()
    }
}

impl FsNodeOps for DeviceDirectory {
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
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            b"dev" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(format!("{}\n", self.device_type()).into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"uevent" => Ok(node.fs().create_node(
                current_task,
                UEventFsNode::new(self.device.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            )),
            _ => error!(ENOENT),
        }
    }
}

pub trait BlockDeviceInfo: Send + Sync {
    fn size(&self) -> Result<usize, Errno>;
}

pub struct BlockDeviceDirectory {
    base_dir: DeviceDirectory,
    block_info: Weak<dyn BlockDeviceInfo>,
}

impl BlockDeviceDirectory {
    pub fn new(device: Device, block_info: Weak<dyn BlockDeviceInfo>) -> Self {
        Self { base_dir: DeviceDirectory::new(device), block_info }
    }
}

impl BlockDeviceDirectory {
    pub fn create_file_ops_entries() -> Vec<VecDirectoryEntry> {
        // Start with the entries provided by the base directory and then add our own.
        let mut entries = DeviceDirectory::create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::DIR,
            name: b"queue".into(),
            inode: None,
        });
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: "size".into(),
            inode: None,
        });
        entries
    }

    fn kobject(&self) -> KObjectHandle {
        self.base_dir.kobject()
    }
}

impl FsNodeOps for BlockDeviceDirectory {
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
            b"queue" => Ok(node.fs().create_node(
                current_task,
                BlockDeviceQueueDirectory::new(self.kobject()),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            b"size" => Ok(node.fs().create_node(
                current_task,
                BlockDeviceSizeFile::new_node(self.block_info.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}

struct BlockDeviceQueueDirectory {
    _handle: KObjectHandle,
}

impl BlockDeviceQueueDirectory {
    fn new(kobject: KObjectHandle) -> Self {
        Self { _handle: kobject }
    }
}

impl FsNodeOps for BlockDeviceQueueDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(vec![
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: b"nr_requests".into(),
                inode: None,
            },
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: b"read_ahead_kb".into(),
                inode: None,
            },
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: b"scheduler".into(),
                inode: None,
            },
        ]))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            b"nr_requests" => Ok(node.fs().create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/sys/block/DEVICE/queue/nr_requests",
                    bug_ref!("https://fxbug.dev/322906857"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            )),
            b"read_ahead_kb" => Ok(node.fs().create_node(
                current_task,
                ReadAheadKbNode,
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            )),
            b"scheduler" => Ok(node.fs().create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/sys/block/DEVICE/queue/scheduler",
                    bug_ref!("https://fxbug.dev/322907749"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            )),
            _ => {
                error!(ENOENT)
            }
        }
    }
}

#[derive(Clone)]
struct BlockDeviceSizeFile {
    block_info: Weak<dyn BlockDeviceInfo>,
}

impl BlockDeviceSizeFile {
    pub fn new_node(block_info: Weak<dyn BlockDeviceInfo>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { block_info })
    }
}

impl DynamicFileSource for BlockDeviceSizeFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let size = self.block_info.upgrade().ok_or_else(|| errno!(EINVAL))?.size()?;
        let size_blocks = size / DEFAULT_BYTES_PER_BLOCK;
        writeln!(sink, "{}", size_blocks)?;
        Ok(())
    }
}

#[derive(Clone)]
struct ReadAheadKbSource;

impl DynamicFileSource for ReadAheadKbSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        writeln!(sink, "0").map(|_| ())
    }
}

struct ReadAheadKbNode;

impl FsNodeOps for ReadAheadKbNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ReadAheadKbFile { dynamic_file: DynamicFile::new(ReadAheadKbSource) }))
    }
}

struct ReadAheadKbFile {
    dynamic_file: DynamicFile<ReadAheadKbSource>,
}

impl FileOps for ReadAheadKbFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let updated = data.read_all()?;
        track_stub!(TODO("https://fxbug.dev/297295673"), "updating read_ahead_kb");
        Ok(updated.len())
    }
}
