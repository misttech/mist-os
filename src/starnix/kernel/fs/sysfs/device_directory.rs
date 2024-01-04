// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::kobject::{Device, KObjectBased, KObjectHandle, UEventFsNode},
    fs::sysfs::SysfsOps,
    task::CurrentTask,
    vfs::{
        buffers::InputBuffer, fileops_impl_delegate_read_and_seek, fs_node_impl_dir_readonly,
        fs_node_impl_not_dir, BytesFile, DirectoryEntryType, DynamicFile, DynamicFileBuf,
        DynamicFileSource, FileObject, FileOps, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
        VecDirectory, VecDirectoryEntry,
    },
};
use starnix_logging::not_implemented;
use starnix_uapi::{
    auth::FsCred, device_type::DeviceType, error, errors::Errno, file_mode::mode,
    open_flags::OpenFlags,
};

pub trait DeviceSysfsOps: SysfsOps {
    fn device(&self) -> Device;
}

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

    fn create_file_ops_entries() -> Vec<VecDirectoryEntry> {
        // TODO(https://fxbug.dev/121327): Add power and subsystem nodes.
        vec![
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: b"dev".to_vec(),
                inode: None,
            },
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: b"uevent".to_vec(),
                inode: None,
            },
        ]
    }
}

impl SysfsOps for DeviceDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.device.kobject().clone()
    }
}

impl DeviceSysfsOps for DeviceDirectory {
    fn device(&self) -> Device {
        self.device.clone()
    }
}

impl FsNodeOps for DeviceDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(Self::create_file_ops_entries()))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match name {
            b"dev" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(
                    format!("{}:{}\n", self.device_type().major(), self.device_type().minor())
                        .into_bytes(),
                ),
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

pub struct BlockDeviceDirectory {
    base_dir: DeviceDirectory,
}

impl BlockDeviceDirectory {
    pub fn new(device: Device) -> Self {
        Self { base_dir: DeviceDirectory::new(device) }
    }
}

impl BlockDeviceDirectory {
    pub fn create_file_ops_entries() -> Vec<VecDirectoryEntry> {
        // Start with the entries provided by the base directory and then add our own.
        let mut entries = DeviceDirectory::create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::DIR,
            name: b"queue".to_vec(),
            inode: None,
        });
        entries
    }
}

impl SysfsOps for BlockDeviceDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.base_dir.kobject()
    }
}

impl DeviceSysfsOps for BlockDeviceDirectory {
    fn device(&self) -> Device {
        self.base_dir.device()
    }
}

impl FsNodeOps for BlockDeviceDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(Self::create_file_ops_entries()))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match name {
            b"queue" => Ok(node.fs().create_node(
                current_task,
                BlockDeviceQueueDirectory::new(self.kobject()),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            _ => self.base_dir.lookup(node, current_task, name),
        }
    }
}

struct BlockDeviceQueueDirectory(KObjectHandle);

impl BlockDeviceQueueDirectory {
    fn new(kobject: KObjectHandle) -> Self {
        Self(kobject)
    }
}

impl FsNodeOps for BlockDeviceQueueDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(vec![VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"read_ahead_kb".to_vec(),
            inode: None,
        }]))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match name {
            b"read_ahead_kb" => Ok(node.fs().create_node(
                current_task,
                ReadAheadKbNode,
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            )),
            _ => {
                error!(ENOENT)
            }
        }
    }
}

// TODO(https://fxbug.dev/76801) connect this to an actual readahead implementation
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

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let updated = data.read_all()?;
        not_implemented!("updating read_ahead_kb");
        Ok(updated.len())
    }
}
