// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{Device, UEventFsNode};
use crate::task::CurrentTask;
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::pseudo::simple_file::{BytesFile, BytesFileOps};
use crate::vfs::pseudo::stub_empty_file::StubEmptyFile;
use crate::vfs::{FsNodeOps, DEFAULT_BYTES_PER_BLOCK};
use starnix_logging::{bug_ref, track_stub};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use std::borrow::Cow;
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
        dir.entry("read_ahead_kb", BytesFile::new_node(ReadAheadKbFile), mode!(IFREG, 0o644));
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

struct BlockDeviceSizeFile {
    block_info: Weak<dyn BlockDeviceInfo>,
}

impl BlockDeviceSizeFile {
    pub fn new_node(block_info: Weak<dyn BlockDeviceInfo>) -> impl FsNodeOps {
        BytesFile::new_node(Self { block_info })
    }
}

impl BytesFileOps for BlockDeviceSizeFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let size = self.block_info.upgrade().ok_or_else(|| errno!(EINVAL))?.size()?;
        let size_blocks = size / DEFAULT_BYTES_PER_BLOCK;
        Ok(format!("{size_blocks}").into_bytes().into())
    }
}

struct ReadAheadKbFile;

impl BytesFileOps for ReadAheadKbFile {
    fn write(&self, _current_task: &CurrentTask, _data: Vec<u8>) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/297295673"), "updating read_ahead_kb");
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(b"0".into())
    }
}
