// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use super::*;

use std::sync::Arc;

use crate::auth::FsCred;
use crate::fs::cgroup::CgroupDirectoryNode;
use crate::fs::kobject::*;
use crate::task::*;
use crate::types::*;

struct SysFs;
impl FileSystemOps for SysFs {
    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        Ok(statfs::default(SYSFS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        b"sysfs"
    }
}

impl SysFs {
    pub fn new_fs(kernel: &Kernel, options: FileSystemOptions) -> FileSystemHandle {
        let fs = FileSystem::new(kernel, CacheMode::Permanent, SysFs, options);
        let mut dir = StaticDirectoryBuilder::new(&fs);
        dir.subdir(b"fs", 0o755, |dir| {
            dir.subdir(b"selinux", 0o755, |_| ());
            dir.subdir(b"bpf", 0o755, |_| ());
            dir.node(
                b"cgroup",
                fs.create_node(CgroupDirectoryNode::new(), mode!(IFDIR, 0o755), FsCred::root()),
            );
            dir.subdir(b"fuse", 0o755, |dir| dir.subdir(b"connections", 0o755, |_| ()));
        });

        dir.entry(
            b"devices",
            SysFsDirectory::new(kernel.device_registry.write().root_kobject()),
            mode!(IFDIR, 0o755),
        );
        kernel
            .device_registry
            .write()
            .root_kobject()
            .get_or_create_child(b"system", KType::Bus)
            .get_or_create_child(b"cpu", KType::Class);

        dir.build_root();
        fs
    }
}

pub fn sys_fs(kern: &Arc<Kernel>, options: FileSystemOptions) -> &FileSystemHandle {
    kern.sys_fs.get_or_init(|| SysFs::new_fs(kern, options))
}

struct SysFsDirectory {
    kobject: KObjectHandle,
}

impl SysFsDirectory {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject }
    }
}

impl FsNodeOps for SysFsDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.kobject
                .get_children_names()
                .into_iter()
                .map(|name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name,
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match self.kobject.get_child(name) {
            Some(child_kobject) => match child_kobject.ktype() {
                KType::Device { .. } => Ok(node.fs().create_node(
                    DeviceDirectory::new(child_kobject),
                    mode!(IFDIR, 0o755),
                    FsCred::root(),
                )),
                KType::Class if name == b"cpu" => Ok(node.fs().create_node(
                    ClassDirectory::new(),
                    mode!(IFDIR, 0o755),
                    FsCred::root(),
                )),
                _ => Ok(node.fs().create_node(
                    SysFsDirectory::new(child_kobject),
                    mode!(IFDIR, 0o755),
                    FsCred::root(),
                )),
            },
            None => error!(ENOENT),
        }
    }
}

struct DeviceDirectory {
    kobject: KObjectHandle,
}

impl DeviceDirectory {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject }
    }

    fn device_type(&self) -> Result<DeviceType, Errno> {
        match self.kobject.ktype() {
            KType::Device { device_type, .. } => Ok(device_type),
            _ => error!(ENODEV),
        }
    }
}

impl FsNodeOps for DeviceDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
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
                mode!(IFREG, 0o444),
                FsCred::root(),
            )),
            b"uevent" => Ok(node.fs().create_node(
                UEventFsNode::new(self.kobject.clone()),
                mode!(IFREG, 0o644),
                FsCred::root(),
            )),
            _ => error!(ENOENT),
        }
    }
}

struct ClassDirectory {}

impl ClassDirectory {
    pub fn new() -> Self {
        Self {}
    }
}

impl FsNodeOps for ClassDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // TODO(fxbug.dev/121327): A workaround before binding FsNodeOps to each kobject.
        Ok(VecDirectory::new_file(vec![VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"online".to_vec(),
            inode: None,
        }]))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        match name {
            b"online" => Ok(node.fs().create_node(
                BytesFile::new_node(format!("{}\n", 1).into_bytes()),
                mode!(IFREG, 0o444),
                FsCred::root(),
            )),
            _ => error!(ENOENT),
        }
    }
}
