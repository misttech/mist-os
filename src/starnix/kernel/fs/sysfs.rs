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

struct SysFsOps;
impl FileSystemOps for SysFsOps {
    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        Ok(statfs::default(SYSFS_MAGIC))
    }
}

pub struct SysFs {
    root_kobject: KObjectHandle,
    kernel: Arc<Kernel>,
    fs: FileSystemHandle,
}

impl SysFs {
    pub fn new(kernel: &Arc<Kernel>) -> Self {
        let fs = FileSystem::new(
            kernel,
            CacheMode::Permanent,
            SysFsOps,
            FileSystemLabel::without_source("sysfs"),
        );
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

        let root_kobject = KObject::new_root();
        dir.entry(b"devices", SysFsDirectory::new(root_kobject.clone()), mode!(IFDIR, 0o755));

        dir.build_root();

        let new_sysfs = Self { root_kobject, kernel: kernel.clone(), fs };
        new_sysfs.add_common_devices();
        new_sysfs
    }

    /// Returns the `FileSystemHandle` of the SysFs filesystem.
    pub fn fs(&self) -> FileSystemHandle {
        self.fs.clone()
    }

    /// Returns the virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> KObjectHandle {
        self.root_kobject.get_or_create_child(b"virtual", KType::Bus)
    }

    /// Adds a single device kobject in the tree.
    pub fn add_device(&self, subsystem: KObjectHandle, dev_attr: KObjectDeviceAttribute) {
        let ktype =
            KType::Device { name: Some(dev_attr.device_name), device_type: dev_attr.device_type };
        self.kernel.device_registry.write().dispatch_uevent(
            UEventAction::Add,
            subsystem.get_or_create_child(&dev_attr.kobject_name, ktype),
        );
    }

    /// Adds a list of device kobjects in the tree.
    pub fn add_devices(&self, subsystem: KObjectHandle, dev_attrs: Vec<KObjectDeviceAttribute>) {
        for attr in dev_attrs {
            self.add_device(subsystem.clone(), attr);
        }
    }

    fn add_common_devices(&self) {
        let virtual_bus = self.virtual_bus();

        // MEM class.
        self.add_devices(
            virtual_bus.get_or_create_child(b"mem", KType::Class),
            KObjectDeviceAttribute::new_from_vec(vec![
                (b"null", b"null", DeviceType::NULL),
                (b"zero", b"zero", DeviceType::ZERO),
                (b"full", b"full", DeviceType::FULL),
                (b"random", b"random", DeviceType::RANDOM),
                (b"urandom", b"urandom", DeviceType::URANDOM),
                (b"kmsg", b"kmsg", DeviceType::KMSG),
            ]),
        );

        // MISC class.
        self.add_devices(
            virtual_bus.get_or_create_child(b"misc", KType::Class),
            KObjectDeviceAttribute::new_from_vec(vec![
                (b"hwrng", b"hwrng", DeviceType::HW_RANDOM),
                (b"fuse", b"fuse", DeviceType::FUSE),
                (b"device-mapper", b"mapper/control", DeviceType::DEVICE_MAPPER),
            ]),
        );

        // TTY class.
        self.add_devices(
            virtual_bus.get_or_create_child(b"tty", KType::Class),
            KObjectDeviceAttribute::new_from_vec(vec![
                (b"tty", b"tty", DeviceType::TTY),
                (b"ptmx", b"ptmx", DeviceType::PTMX),
            ]),
        );
    }
}

pub fn sys_fs(kern: &Arc<Kernel>) -> &SysFs {
    kern.sys_fs.get_or_init(|| SysFs::new(kern))
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
