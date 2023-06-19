// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{auth::FsCred, fs::*, lock::*, task::*, types::*};

pub struct SdcardHackFs(());

impl SdcardHackFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
        proxy_from: &FsStr,
    ) -> Result<FileSystemHandle, Errno> {
        let inner = current_task.lookup_path_from_root(proxy_from)?;
        let fs = FileSystem::new(current_task.kernel(), CacheMode::Cached, Self(()), options);
        fs.set_root(SdcardHackNode(inner));
        Ok(fs)
    }
}

impl FileSystemOps for SdcardHackFs {
    fn name(&self) -> &'static FsStr {
        b"fuse"
    }
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(statfs {
            // Pretend we have a ton of free space.
            f_blocks: 0x100000000,
            f_bavail: 0x100000000,
            f_bfree: 0x100000000,
            ..statfs::default(FUSE_SUPER_MAGIC)
        })
    }
}

struct SdcardHackNode(NamespaceNode);

impl FsNodeOps for SdcardHackNode {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ProxyFileOps(FileObject::new(
            self.0.entry.node.create_file_ops(current_task, flags)?,
            self.0.clone(),
            flags,
        ))))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let inner = self.0.with_new_entry(self.0.entry.component_lookup(current_task, name)?);
        let node_id = inner.entry.node.node_id;
        let child =
            node.fs().create_node_with_id(Box::new(Self(inner)), node_id, FsNodeInfo::default());
        let _ = child.stat(current_task);
        Ok(child)
    }

    fn update_info<'a>(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let mut info = info.write();
        *info = self.0.entry.node.info().clone();
        info.mode |= FileMode::ALLOW_ALL;
        Ok(RwLockWriteGuard::downgrade(info))
    }

    fn mknod(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let child = self.0.entry.create_node(current_task, name, mode, dev, owner.clone())?;
        let child = SdcardHackNode(self.0.with_new_entry(child));
        Ok(node.fs().create_node(child, FsNodeInfo::new_factory(mode, owner)))
    }

    fn mkdir(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        self.mknod(node, current_task, name, mode, DeviceType::NONE, owner)
    }

    fn create_symlink(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let child = self.0.entry.create_symlink(current_task, name, target, owner.clone())?;
        let child = SdcardHackNode(self.0.with_new_entry(child));
        Ok(node.fs().create_node(child, FsNodeInfo::new_factory(mode!(IFLNK, 0o777), owner)))
    }

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        self.0.readlink(current_task)
    }

    fn link(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        self.0.entry.node.link(current_task, name, child)
    }

    fn unlink(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        self.0.entry.node.unlink(current_task, name, child)
    }

    fn truncate(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    fn allocate(&self, _node: &FsNode, _offset: u64, _length: u64) -> Result<(), Errno> {
        error!(EINVAL)
    }
}
