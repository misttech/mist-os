// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::KObjectHandle;
use crate::fs::sysfs::{
    sysfs_kernel_directory, sysfs_power_directory, CpuClassDirectory, KObjectDirectory,
    VulnerabilitiesClassDirectory,
};
use crate::task::CurrentTask;
use crate::vfs::pseudo::pseudo_directory::PseudoDirectoryBuilder;
use crate::vfs::pseudo::simple_file::BytesFile;
use crate::vfs::pseudo::stub_empty_file::StubEmptyFile;
use crate::vfs::{
    CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
    FsNodeInfo, FsStr, PathBuilder, SymlinkNode,
};
use ebpf_api::BPF_PROG_TYPE_FUSE;
use starnix_logging::bug_ref;
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::{statfs, SYSFS_MAGIC};

pub const SYSFS_DEVICES: &str = "devices";
pub const SYSFS_BUS: &str = "bus";
pub const SYSFS_CLASS: &str = "class";
pub const SYSFS_BLOCK: &str = "block";
pub const SYSFS_DEV: &str = "dev";

struct SysFs;
impl FileSystemOps for SysFs {
    fn statfs(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(SYSFS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "sysfs".into()
    }
}

impl SysFs {
    pub fn new_fs(current_task: &CurrentTask, options: FileSystemOptions) -> FileSystemHandle {
        const DIR_MODE: FileMode = mode!(IFDIR, 0o755);
        const REG_MODE: FileMode = mode!(IFREG, 0o444);
        const CREDS: FsCred = FsCred::root();

        let mut dir = PseudoDirectoryBuilder::default();
        {
            let dir = dir.subdir(b"fs");
            dir.subdir(b"selinux");
            dir.subdir(b"bpf");
            dir.subdir(b"cgroup");
            {
                let dir = dir.subdir(b"fuse");
                dir.subdir(b"connections");
                {
                    let dir = dir.subdir(b"features");
                    dir.node(b"fuse_bpf", REG_MODE, CREDS, || {
                        BytesFile::new_node(b"supported\n".to_vec())
                    });
                }
                dir.node(b"bpf_prog_type_fuse", REG_MODE, CREDS, || {
                    BytesFile::new_node(format!("{}\n", BPF_PROG_TYPE_FUSE).into_bytes())
                });
            }
            dir.subdir(b"pstore");
        }

        let kernel = current_task.kernel();
        let registry = &kernel.device_registry;

        let devices = registry.objects.devices.clone();
        dir.node_ops(SYSFS_DEVICES, DIR_MODE, CREDS, move || devices.ops());

        let bus = registry.objects.bus.clone();
        dir.node_ops(SYSFS_BUS, DIR_MODE, CREDS, move || bus.ops());

        let block = registry.objects.block.clone();
        dir.node_ops(SYSFS_BLOCK, DIR_MODE, CREDS, move || block.ops());

        let class = registry.objects.class.clone();
        dir.node_ops(SYSFS_CLASS, DIR_MODE, CREDS, move || class.ops());

        let dev = registry.objects.dev.clone();
        dir.node_ops(SYSFS_DEV, DIR_MODE, CREDS, move || dev.ops());

        sysfs_kernel_directory(kernel, &mut dir.subdir(b"kernel"));
        sysfs_power_directory(kernel, &mut dir.subdir(b"power"));

        {
            let dir = dir.subdir(b"module");
            {
                let dir = dir.subdir(b"dm_verity");
                {
                    let dir = dir.subdir(b"parameters");
                    dir.node(b"prefetch_cluster", mode!(IFREG, 0o644), CREDS, || {
                        StubEmptyFile::new_node(
                            "/sys/module/dm_verity/paramters/prefetch_cluster",
                            bug_ref!("https://fxbug.dev/322893670"),
                        )
                    });
                }
            }
        }

        // TODO(https://fxbug.dev/42072346): Temporary fix of flakeness in tcp_socket_test.
        // Remove after registry.rs refactor is in place.
        registry
            .objects
            .devices
            .get_or_create_child("system".into(), KObjectDirectory::new)
            .get_or_create_child("cpu".into(), CpuClassDirectory::new)
            .get_or_create_child("vulnerabilities".into(), VulnerabilitiesClassDirectory::new);

        let fs = FileSystem::new(kernel, CacheMode::Cached(CacheConfig::default()), SysFs, options)
            .expect("sysfs constructed with valid options");
        dir.build_root(&fs, mode!(IFDIR, 0o777), CREDS);
        fs
    }
}

struct SysFsHandle(FileSystemHandle);

pub fn sys_fs(
    _locked: &mut Locked<Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    Ok(current_task
        .kernel()
        .expando
        .get_or_init(|| SysFsHandle(SysFs::new_fs(current_task, options)))
        .0
        .clone())
}

/// Creates a path to the `to` kobject in the devices tree, relative to the `from` kobject from
/// a subsystem.
pub fn sysfs_create_link(
    from: KObjectHandle,
    to: KObjectHandle,
    owner: FsCred,
) -> (SymlinkNode, FsNodeInfo) {
    let mut path = PathBuilder::new();
    path.prepend_element(to.path().as_ref());
    // Escape one more level from its subsystem to the root of sysfs.
    path.prepend_element("..".into());

    let path_to_root = from.path_to_root();
    if !path_to_root.is_empty() {
        path.prepend_element(path_to_root.as_ref());
    }

    // Build a symlink with the relative path.
    SymlinkNode::new(path.build_relative().as_ref(), owner)
}

/// Creates a path to the `to` kobject in the devices tree, relative to the `from` kobject from
/// the bus devices directory.
pub fn sysfs_create_bus_link(
    from: KObjectHandle,
    to: KObjectHandle,
    owner: FsCred,
) -> (SymlinkNode, FsNodeInfo) {
    let mut path = PathBuilder::new();
    path.prepend_element(to.path().as_ref());
    // Escape two more levels from its subsystem to the root of sysfs.
    path.prepend_element("..".into());
    path.prepend_element("..".into());

    let path_to_root = from.path_to_root();
    if !path_to_root.is_empty() {
        path.prepend_element(path_to_root.as_ref());
    }

    // Build a symlink with the relative path.
    SymlinkNode::new(path.build_relative().as_ref(), owner)
}
