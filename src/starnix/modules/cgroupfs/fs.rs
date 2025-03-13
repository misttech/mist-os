// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::{Cgroup, CgroupOps, CgroupRoot, CurrentTask};
use starnix_core::vfs::{
    CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNodeHandle,
    FsNodeInfo, FsStr,
};
use starnix_sync::{FileOpsCore, Locked, Mutex, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, mode, statfs, CGROUP2_SUPER_MAGIC, CGROUP_SUPER_MAGIC};

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use crate::directory::{CgroupDirectory, CgroupDirectoryHandle};

pub struct CgroupV1Fs {
    pub root: Arc<CgroupRoot>,

    /// All directory nodes of the filesystem.
    pub dir_nodes: Arc<DirectoryNodes>,
}

impl CgroupV1Fs {
    pub fn new_fs(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let weak_kernel = Arc::downgrade(current_task.kernel());
        let root = CgroupRoot::new(weak_kernel);
        let dir_nodes = DirectoryNodes::new(Arc::downgrade(&root));
        let root_dir = dir_nodes.root.clone();
        let fs = FileSystem::new(
            current_task.kernel(),
            CacheMode::Uncached,
            CgroupV1Fs { dir_nodes, root },
            options,
        )?;
        root_dir.create_root_interface_files(current_task, &fs);
        fs.set_root(root_dir);
        Ok(fs)
    }
}
impl FileSystemOps for CgroupV1Fs {
    fn name(&self) -> &'static FsStr {
        b"cgroup".into()
    }
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(CGROUP_SUPER_MAGIC))
    }
}

pub struct CgroupV2Fs {
    /// All directory nodes of the filesystem.
    pub dir_nodes: Arc<DirectoryNodes>,
}

struct CgroupV2FsHandle(FileSystemHandle);
pub fn cgroup2_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    Ok(current_task
        .kernel()
        .expando
        .get_or_try_init(|| Ok(CgroupV2FsHandle(CgroupV2Fs::new_fs(current_task, options)?)))?
        .0
        .clone())
}

impl CgroupV2Fs {
    fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let dir_nodes = DirectoryNodes::new(Arc::downgrade(&kernel.cgroups.cgroup2));
        let root = dir_nodes.root.clone();
        let fs = FileSystem::new(
            current_task.kernel(),
            CacheMode::Uncached,
            CgroupV2Fs { dir_nodes },
            options,
        )?;
        root.create_root_interface_files(current_task, &fs);
        fs.set_root(root);
        Ok(fs)
    }
}

impl FileSystemOps for CgroupV2Fs {
    fn name(&self) -> &'static FsStr {
        b"cgroup2".into()
    }
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(CGROUP2_SUPER_MAGIC))
    }
}

/// Represents all directory nodes of a cgroup hierarchy.
pub struct DirectoryNodes {
    /// `CgroupRoot`'s directory handle. The `FileSystem` owns the `FsNode` of the root, and so we
    /// do not have a `FsNodeHandle` of the root.
    root: CgroupDirectoryHandle,

    /// All non-root cgroup directories, keyed by cgroup's ID. Every non-root cgroup has a
    /// corresponding node.
    nodes: Mutex<HashMap<u64, FsNodeHandle>>,
}

impl DirectoryNodes {
    pub fn new(root_cgroup: Weak<CgroupRoot>) -> Arc<DirectoryNodes> {
        Arc::new_cyclic(|weak_self| Self {
            root: CgroupDirectory::new_root(root_cgroup, weak_self.clone()),
            nodes: Mutex::new(HashMap::new()),
        })
    }

    /// Looks for the corresponding node in the filesystem, errors if not found.
    pub fn get_node(&self, cgroup: &Arc<Cgroup>) -> Result<FsNodeHandle, Errno> {
        let nodes = self.nodes.lock();
        nodes.get(&cgroup.id()).cloned().ok_or_else(|| errno!(ENOENT))
    }

    /// Returns the corresponding nodes for a set of cgroups.
    pub fn get_nodes(&self, cgroups: &Vec<Arc<Cgroup>>) -> Vec<Option<FsNodeHandle>> {
        let nodes = self.nodes.lock();
        cgroups.iter().map(|cgroup| nodes.get(&cgroup.id()).cloned()).collect()
    }

    /// Creates a new `FsNode` for `directory` and stores it in `nodes`.
    pub fn add_node(
        &self,
        cgroup: &Arc<Cgroup>,
        directory: CgroupDirectoryHandle,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
    ) -> FsNodeHandle {
        let id = cgroup.id();
        let node = fs.create_node(
            current_task,
            directory,
            FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
        );
        let mut nodes = self.nodes.lock();
        nodes.insert(id, node.clone());
        node
    }

    /// Removes an entry from `nodes`, errors if not found.
    pub fn remove_node(&self, cgroup: &Arc<Cgroup>) -> Result<FsNodeHandle, Errno> {
        let id = cgroup.id();
        let mut nodes = self.nodes.lock();
        nodes.remove(&id).ok_or_else(|| errno!(ENOENT))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use starnix_core::testing::create_kernel_task_and_unlocked;
    use starnix_core::vfs::fs_registry::FsRegistry;

    #[::fuchsia::test]
    async fn test_filesystem_creates_nodes() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = kernel.expando.get::<FsRegistry>();
        registry.register(b"cgroup2".into(), cgroup2_fs);

        let fs = current_task
            .create_filesystem(&mut locked, b"cgroup2".into(), Default::default())
            .expect("create_filesystem");

        let cgroupfs = fs.downcast_ops::<CgroupV2Fs>().expect("downcast_ops");
        let dir_nodes = cgroupfs.dir_nodes.clone();
        assert!(dir_nodes.nodes.lock().is_empty(), "new filesystem does not contain nodes");

        let root_dir = dir_nodes.root.clone();
        assert!(root_dir.has_interface_files(), "root directory is initialized");
    }
}
