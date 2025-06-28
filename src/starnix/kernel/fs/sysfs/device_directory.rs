// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{Device, UEventFsNode};
use crate::task::CurrentTask;
use crate::vfs::buffers::InputBuffer;
use crate::vfs::pseudo::dynamic_file::{DynamicFile, DynamicFileBuf, DynamicFileSource};
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::pseudo::simple_file::BytesFile;
use crate::vfs::pseudo::stub_empty_file::StubEmptyFile;
use crate::vfs::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, fs_node_impl_not_dir, FileObject,
    FileOps, FsNode, FsNodeOps, DEFAULT_BYTES_PER_BLOCK,
};
use starnix_logging::{bug_ref, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Weak;

pub fn build_device_directory(device: &Device, dir: &SimpleDirectoryMutator) {
    if let Some(metadata) = &device.metadata {
        dir.entry(
            "dev",
            BytesFile::new_node(format!("{}\n", metadata.device_type).into_bytes()),
            mode!(IFREG, 0o444),
        );
    }
    dir.entry("uevent", UEventFsNode::new(device.clone()), mode!(IFREG, 0o644));
}

pub fn build_block_device_directory(
    device: &Device,
    block_info: Weak<dyn BlockDeviceInfo>,
    dir: &SimpleDirectoryMutator,
) {
    build_device_directory(device, dir);
    dir.subdir("queue", 0o755, |dir| {
        dir.entry(
            "nr_requests",
            StubEmptyFile::new_node(
                "/sys/block/DEVICE/queue/nr_requests",
                bug_ref!("https://fxbug.dev/322906857"),
            ),
            mode!(IFREG, 0o644),
        );
        dir.entry("read_ahead_kb", ReadAheadKbNode, mode!(IFREG, 0o644));
        dir.entry(
            "scheduler",
            StubEmptyFile::new_node(
                "/sys/block/DEVICE/queue/scheduler",
                bug_ref!("https://fxbug.dev/322907749"),
            ),
            mode!(IFREG, 0o644),
        );
    });
    dir.entry("size", BlockDeviceSizeFile::new_node(block_info), mode!(IFREG, 0o444));
}

pub trait BlockDeviceInfo: Send + Sync {
    fn size(&self) -> Result<usize, Errno>;
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
        _locked: &mut Locked<FileOpsCore>,
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
        _locked: &mut Locked<FileOpsCore>,
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
