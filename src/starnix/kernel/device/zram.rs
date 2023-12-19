// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        kobject::{Device, DeviceMetadata, KObjectHandle},
        simple_device_ops, DeviceMode,
    },
    fs::sysfs::{BlockDeviceDirectory, DeviceSysfsOps, SysfsOps},
    task::{CurrentTask, KernelStats},
    vfs::{
        fileops_impl_dataless, fileops_impl_seekless, fs_node_impl_dir_readonly,
        DirectoryEntryType, DynamicFile, DynamicFileBuf, DynamicFileSource, FileOps, FsNode,
        FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
    },
};
use fuchsia_zircon as zx;
use starnix_logging::log_error;
use starnix_uapi::{
    auth::FsCred,
    device_type::{DeviceType, ZRAM_MAJOR},
    errno,
    errors::Errno,
    file_mode::mode,
    open_flags::OpenFlags,
};
use std::sync::Arc;

#[derive(Default)]
pub struct DevZram;

impl FileOps for DevZram {
    fileops_impl_seekless!();
    fileops_impl_dataless!();
}

pub struct ZramDeviceDirectory {
    base_dir: BlockDeviceDirectory,
}

impl ZramDeviceDirectory {
    pub fn new(device: Device) -> Self {
        Self { base_dir: BlockDeviceDirectory::new(device) }
    }
}

impl SysfsOps for ZramDeviceDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.base_dir.kobject()
    }
}

impl DeviceSysfsOps for ZramDeviceDirectory {
    fn device(&self) -> Device {
        self.base_dir.device()
    }
}

impl FsNodeOps for ZramDeviceDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let mut entries = BlockDeviceDirectory::create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"mm_stat".to_vec(),
            inode: None,
        });
        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match name {
            b"mm_stat" => Ok(node.fs().create_node(
                current_task,
                MmStatFile::new_node(&current_task.kernel().stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            _ => self.base_dir.lookup(node, current_task, name),
        }
    }
}

#[derive(Clone)]
struct MmStatFile {
    kernel_stats: Arc<KernelStats>,
}
impl MmStatFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}
impl DynamicFileSource for MmStatFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let stats =
            self.kernel_stats.get().get_memory_stats_compression(zx::Time::INFINITE).map_err(
                |e| {
                    log_error!("FIDL error getting memory compression stats: {e}");
                    errno!(EIO)
                },
            )?;

        let compressed_storage_bytes = stats.compressed_storage_bytes.unwrap_or_default();
        let compressed_fragmentation_bytes =
            stats.compressed_fragmentation_bytes.unwrap_or_default();

        let orig_data_size = stats.uncompressed_storage_bytes.unwrap_or_default();
        // This value isn't entirely correct because we're still counting metadata and other
        // non-fragmentation usage.
        let compr_data_size = compressed_storage_bytes - compressed_fragmentation_bytes;
        let mem_used_total = compressed_storage_bytes;
        // The remaining values are not yet available from Zircon.
        let mem_limit = 0;
        let mem_used_max = 0;
        let same_pages = 0;
        let pages_compacted = 0;
        let huge_pages = 0;

        writeln!(sink, "{orig_data_size} {compr_data_size} {mem_used_total} {mem_limit} {mem_used_max} {same_pages} {pages_compacted} {huge_pages}")?;
        Ok(())
    }
}

pub fn zram_device_init(system_task: &CurrentTask) {
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let virtual_block_class = registry.get_or_create_class(b"block", registry.virtual_bus());
    registry
        .register_device(ZRAM_MAJOR, 0, 1, simple_device_ops::<DevZram>, DeviceMode::Block)
        .expect("Failed to register zram device.");

    registry.add_device(
        system_task,
        b"zram0",
        DeviceMetadata::new(b"zram0", DeviceType::new(ZRAM_MAJOR, 0), DeviceMode::Block),
        virtual_block_class,
        ZramDeviceDirectory::new,
    );
}
