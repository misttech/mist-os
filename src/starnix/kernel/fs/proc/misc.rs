// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceMode;
use crate::task::CurrentTask;
use crate::vfs::{BytesFile, BytesFileOps, FileSystemHandle, FsNodeHandle, FsNodeInfo, FsNodeOps};

use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::{DeviceType, MISC_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::mode;

use std::borrow::Cow;

pub fn misc_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        MiscFile::new_node(),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}

#[derive(Clone, Debug)]
struct MiscFile;
impl MiscFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self)
    }
}

impl BytesFileOps for MiscFile {
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let registery = &current_task.kernel().device_registry;
        let devices = registery.list_minor_devices(
            DeviceMode::Char,
            DeviceType::new_range(MISC_MAJOR, DeviceMode::Char.minor_range()),
        );
        let mut contents = String::new();
        for (device_type, name) in devices {
            contents.push_str(&format!("{:3} {}\n", device_type.minor(), name));
        }
        Ok(contents.into_bytes().into())
    }
}
