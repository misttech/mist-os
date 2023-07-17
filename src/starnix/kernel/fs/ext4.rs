// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ext4_read_only::parser::{Parser as ExtParser, XattrMap as ExtXattrMap};
use ext4_read_only::readers::VmoReader;
use ext4_read_only::structs::{EntryType, INode, ROOT_INODE_NUM};
use fuchsia_zircon as zx;
use fuchsia_zircon::AsHandleRef;
use once_cell::sync::OnceCell;
use std::sync::{Arc, Weak};

use super::*;
use crate::auth::*;
use crate::fs::{fileops_impl_directory, fs_node_impl_symlink};
use crate::logging::log_warn;
use crate::mm::*;
use crate::task::*;
use crate::types::*;

pub struct ExtFilesystem {
    parser: ExtParser,
}

impl FileSystemOps for Arc<ExtFilesystem> {
    fn name(&self) -> &'static FsStr {
        b"ext4"
    }

    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(statfs::default(EXT4_SUPER_MAGIC))
    }
}

struct ExtNode {
    fs: Weak<ExtFilesystem>,
    inode_num: u32,
    inode: INode,
    xattrs: ExtXattrMap,
}

impl ExtFilesystem {
    pub fn new_fs(
        kernel: &Arc<Kernel>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let mut open_flags = OpenFlags::RDWR;
        let mut prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE | ProtectionFlags::EXEC;
        if options.flags.contains(MountFlags::RDONLY) {
            open_flags = OpenFlags::RDONLY;
            prot_flags ^= ProtectionFlags::WRITE;
        }
        if options.flags.contains(MountFlags::NOEXEC) {
            prot_flags ^= ProtectionFlags::EXEC;
        }

        let source_device = current_task.open_file(&options.source, open_flags)?;

        // TODO(https://fxbug.dev/130502) fall back to regular read operations if get_vmo fails
        let vmo = source_device.get_vmo(current_task, None, prot_flags)?;
        let fs = Arc::new(Self { parser: ExtParser::new(Box::new(VmoReader::new(vmo))) });
        let ops = ExtDirectory {
            inner: Arc::new(ExtNode::new(fs.clone(), current_task, ROOT_INODE_NUM)?),
        };
        let mut root = FsNode::new_root(ops);
        root.node_id = ROOT_INODE_NUM as ino_t;
        let fs = FileSystem::new(kernel, CacheMode::Uncached, fs, options);
        fs.set_root_node(root);
        Ok(fs)
    }
}

impl ExtNode {
    fn new(
        fs: Arc<ExtFilesystem>,
        _current_task: &CurrentTask,
        inode_num: u32,
    ) -> Result<ExtNode, Errno> {
        let inode = fs.parser.inode(inode_num).map_err(|e| errno!(EIO, e))?;
        let xattrs = fs.parser.inode_xattrs(inode_num).unwrap_or_default();
        Ok(ExtNode { fs: Arc::downgrade(&fs), inode_num, inode, xattrs })
    }

    fn fs(&self) -> Arc<ExtFilesystem> {
        self.fs.upgrade().unwrap()
    }

    fn list_xattrs(&self) -> Result<Vec<FsString>, Errno> {
        Ok(self.xattrs.keys().map(FsString::clone).collect())
    }

    fn get_xattr(&self, name: &FsStr) -> Result<FsString, Errno> {
        self.xattrs.get(name).map(FsString::clone).ok_or_else(|| errno!(ENODATA))
    }

    fn set_xattr(&self, _name: &FsStr, _value: &FsStr, _op: XattrOp) -> Result<(), Errno> {
        error!(ENOSYS)
    }
    fn remove_xattr(&self, _name: &FsStr) -> Result<(), Errno> {
        error!(ENOSYS)
    }
}

struct ExtDirectory {
    inner: Arc<ExtNode>,
}

impl FsNodeOps for ExtDirectory {
    fs_node_impl_xattr_delegate!(self, self.inner);

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ExtDirFileObject { inner: self.inner.clone() }))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let dir_entries = self
            .inner
            .fs()
            .parser
            .entries_from_inode(&self.inner.inode)
            .map_err(|e| errno!(EIO, e))?;
        let entry = dir_entries
            .iter()
            .find(|e| e.name_bytes() == name)
            .ok_or_else(|| errno!(ENOENT, String::from_utf8_lossy(name)))?;
        let ext_node = ExtNode::new(self.inner.fs(), current_task, entry.e2d_ino.into())?;
        let inode_num = ext_node.inode_num as ino_t;
        node.fs().get_or_create_node(Some(inode_num as ino_t), |inode_num| {
            let entry_type = EntryType::from_u8(entry.e2d_type).map_err(|e| errno!(EIO, e))?;
            let mode = FileMode::from_bits(ext_node.inode.e2di_mode.into());
            let owner =
                FsCred { uid: ext_node.inode.e2di_uid.into(), gid: ext_node.inode.e2di_gid.into() };
            let size = u32::from(ext_node.inode.e2di_size) as usize;
            let nlink = ext_node.inode.e2di_nlink.into();

            let ops: Box<dyn FsNodeOps> = match entry_type {
                EntryType::RegularFile => Box::new(ExtFile::new(ext_node, name)),
                EntryType::Directory => Box::new(ExtDirectory { inner: Arc::new(ext_node) }),
                EntryType::SymLink => Box::new(ExtSymlink { inner: ext_node }),
                _ => {
                    log_warn!("unhandled ext entry type {:?}", entry_type);
                    Box::new(ExtFile::new(ext_node, name))
                }
            };

            let child = FsNode::new_uncached(
                ops,
                &node.fs(),
                inode_num,
                FsNodeInfo { mode, uid: owner.uid, gid: owner.gid, ..Default::default() },
            );
            child.update_info(|info| {
                info.size = size;
                info.link_count = nlink;
                Ok(())
            })?;
            Ok(child)
        })
    }
}

struct ExtFile {
    inner: ExtNode,
    name: FsString,
    vmo: OnceCell<Arc<zx::Vmo>>,
}

impl ExtFile {
    fn new(inner: ExtNode, name: &FsStr) -> Self {
        ExtFile { inner, name: name.to_owned(), vmo: OnceCell::new() }
    }
}

impl FsNodeOps for ExtFile {
    fs_node_impl_xattr_delegate!(self, self.inner);

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let vmo = self.vmo.get_or_try_init(|| {
            let bytes = self
                .inner
                .fs()
                .parser
                .read_data(self.inner.inode_num)
                .map_err(|e| errno!(EIO, e))?;
            let vmo = zx::Vmo::create(bytes.len() as u64).map_err(|e| errno!(EBADF, e))?;
            let name_slice = [b"ext4!".as_slice(), &self.name].concat();
            let name_slice =
                &name_slice[..std::cmp::min(name_slice.len(), zx::sys::ZX_MAX_NAME_LEN - 1)];
            let name = std::ffi::CString::new(name_slice).map_err(|e| errno!(EINVAL, e))?;
            vmo.set_name(&name)
                .map_err(|e| errno!(EINVAL, format!("vmo set_name({:?}) failed: {e}", name)))?;
            vmo.write(&bytes, 0).map_err(|e| errno!(EBADF, e))?;
            Ok(Arc::new(vmo))
        })?;

        // TODO(https://fxbug.dev/130425) returned vmo shouldn't be writeable
        Ok(Box::new(VmoFileObject::new(vmo.clone())))
    }
}

struct ExtSymlink {
    inner: ExtNode,
}

impl FsNodeOps for ExtSymlink {
    fs_node_impl_symlink!();
    fs_node_impl_xattr_delegate!(self, self.inner);

    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        let data =
            self.inner.fs().parser.read_data(self.inner.inode_num).map_err(|e| errno!(EIO, e))?;
        Ok(SymlinkTarget::Path(data))
    }
}

struct ExtDirFileObject {
    inner: Arc<ExtNode>,
}

impl FileOps for ExtDirFileObject {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        Ok(default_seek(current_offset, target, |_| error!(EINVAL))?)
    }

    fn readdir(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        let dir_entries = self
            .inner
            .fs()
            .parser
            .entries_from_inode(&self.inner.inode)
            .map_err(|e| errno!(EIO, e))?;

        if sink.offset() as usize >= dir_entries.len() {
            return Ok(());
        }

        for entry in dir_entries[(sink.offset() as usize)..].iter() {
            let inode_num = entry.e2d_ino.into();
            let entry_type = directory_entry_type(
                EntryType::from_u8(entry.e2d_type).map_err(|e| errno!(EIO, e))?,
            );
            sink.add(inode_num, sink.offset() + 1, entry_type, entry.name_bytes())?;
        }
        Ok(())
    }
}

fn directory_entry_type(entry_type: EntryType) -> DirectoryEntryType {
    match entry_type {
        EntryType::Unknown => DirectoryEntryType::UNKNOWN,
        EntryType::RegularFile => DirectoryEntryType::REG,
        EntryType::Directory => DirectoryEntryType::DIR,
        EntryType::CharacterDevice => DirectoryEntryType::CHR,
        EntryType::BlockDevice => DirectoryEntryType::BLK,
        EntryType::FIFO => DirectoryEntryType::FIFO,
        EntryType::Socket => DirectoryEntryType::SOCK,
        EntryType::SymLink => DirectoryEntryType::LNK,
    }
}
