// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        kobject::{Device, DeviceMetadata, KObjectHandle},
        DeviceMode, DeviceOps,
    },
    fs::sysfs::{BlockDeviceDirectory, BlockDeviceInfo, DeviceSysfsOps, SysfsOps},
    task::{CurrentTask, KernelStats},
    vfs::{
        fileops_impl_dataless, fileops_impl_seekless, fs_node_impl_dir_readonly,
        DirectoryEntryType, DynamicFile, DynamicFileBuf, DynamicFileSource, FileOps, FsNode,
        FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, StubEmptyFile, VecDirectory, VecDirectoryEntry,
    },
};
use fuchsia_zircon as zx;
use starnix_logging::{bug_ref, log_error};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::{
    auth::FsCred,
    device_type::{DeviceType, ZRAM_MAJOR},
    errno,
    errors::Errno,
    file_mode::mode,
    open_flags::OpenFlags,
};
use std::sync::{Arc, Weak};

#[derive(Default)]
pub struct ZramDevice {
    kernel_stats: Arc<KernelStats>,
}

impl ZramDevice {
    fn get_stats(&self) -> Result<fidl_fuchsia_kernel::MemoryStatsCompression, Errno> {
        self.kernel_stats.get().get_memory_stats_compression(zx::Time::INFINITE).map_err(|e| {
            log_error!("FIDL error getting memory compression stats: {e}");
            errno!(EIO)
        })
    }
}

impl DeviceOps for Arc<ZramDevice> {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }
}

impl FileOps for ZramDevice {
    fileops_impl_seekless!();
    fileops_impl_dataless!();
}

impl BlockDeviceInfo for ZramDevice {
    fn size(&self) -> Result<usize, Errno> {
        Ok(self.get_stats()?.uncompressed_storage_bytes.unwrap_or_default() as usize)
    }
}

pub struct ZramDeviceDirectory {
    device: Weak<ZramDevice>,
    base_dir: BlockDeviceDirectory,
}

impl ZramDeviceDirectory {
    pub fn new(device: Device, zram_device: Weak<ZramDevice>) -> Self {
        let base_dir = BlockDeviceDirectory::new(device, zram_device.clone());
        Self { device: zram_device, base_dir }
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
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let mut entries = BlockDeviceDirectory::create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"idle".into(),
            inode: None,
        });
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"mm_stat".into(),
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
        match &**name {
            b"idle" => Ok(node.fs().create_node(
                current_task,
                StubEmptyFile::new_node("zram idle file", bug_ref!("https://fxbug.dev/322892951")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o664), FsCred::root()),
            )),
            b"mm_stat" => {
                let device = self.device.upgrade().ok_or_else(|| errno!(EINVAL))?;
                Ok(node.fs().create_node(
                    current_task,
                    MmStatFile::new_node(device),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                ))
            }
            _ => self.base_dir.lookup(node, current_task, name),
        }
    }
}

#[derive(Clone)]
struct MmStatFile {
    device: Arc<ZramDevice>,
}
impl MmStatFile {
    pub fn new_node(device: Arc<ZramDevice>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { device })
    }
}
impl DynamicFileSource for MmStatFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let stats = self.device.get_stats()?;

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

        writeln!(
            sink,
            "{orig_data_size} {compr_data_size} {mem_used_total} {mem_limit} \
                        {mem_used_max} {same_pages} {pages_compacted} {huge_pages}"
        )?;
        Ok(())
    }
}

pub fn zram_device_init<L>(locked: &mut Locked<'_, L>, system_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let zram_dev = Arc::new(ZramDevice::default());
    let zram_dev_weak = Arc::downgrade(&zram_dev);
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;
    let virtual_block_class = registry.get_or_create_class("block".into(), registry.virtual_bus());
    registry
        .register_device(ZRAM_MAJOR, 0, 1, zram_dev, DeviceMode::Block)
        .expect("Failed to register zram device.");

    registry.add_device(
        locked,
        system_task,
        "zram0".into(),
        DeviceMetadata::new("zram0".into(), DeviceType::new(ZRAM_MAJOR, 0), DeviceMode::Block),
        virtual_block_class,
        move |dev| ZramDeviceDirectory::new(dev, zram_dev_weak.clone()),
    );
}
