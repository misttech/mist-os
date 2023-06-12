// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    auth::FsCred,
    fs::{kobject::*, sysfs::SysFsOps, *},
    task::CurrentTask,
    types::*,
};

use std::sync::{Arc, Weak};

pub struct DeviceDirectory {
    kobject: Weak<KObject>,
}

impl DeviceDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }

    fn device_type(&self) -> Result<DeviceType, Errno> {
        match self.kobject().ktype() {
            KType::Device { device_type, .. } => Ok(device_type),
            _ => error!(ENODEV),
        }
    }
}

impl SysFsOps for DeviceDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.kobject.clone().upgrade().expect("Weak references to kobject must always be valid")
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
        // TODO(fxb/121327): Add power and subsystem nodes.
        Ok(VecDirectory::new_file(vec![
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
        ]))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        match name {
            b"dev" => Ok(node.fs().create_node(
                BytesFile::new_node(
                    format!("{}:{}\n", self.device_type()?.major(), self.device_type()?.minor())
                        .into_bytes(),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"uevent" => Ok(node.fs().create_node(
                UEventFsNode::new(self.kobject()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            )),
            _ => error!(ENOENT),
        }
    }
}
