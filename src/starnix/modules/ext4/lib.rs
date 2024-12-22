// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ext4_read_only::parser::{Parser as ExtParser, XattrMap as ExtXattrMap};
use ext4_read_only::readers::VmoReader;
use ext4_read_only::structs::{EntryType, INode, ROOT_INODE_NUM};
use starnix_sync::Unlocked;

use once_cell::sync::OnceCell;
use starnix_core::mm::memory::MemoryObject;
use starnix_core::mm::ProtectionFlags;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    default_seek, fileops_impl_directory, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    fs_node_impl_not_dir, fs_node_impl_symlink, fs_node_impl_xattr_delegate, CacheConfig,
    CacheMode, DirectoryEntryType, DirentSink, FileObject, FileOps, FileSystem, FileSystemHandle,
    FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString,
    MemoryFileObject, SeekTarget, SymlinkTarget, XattrOp, XattrStorage,
};
use starnix_logging::{impossible_error, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, ino_t, off_t, statfs, EXT4_SUPER_MAGIC};
use std::sync::Arc;
use zx::HandleBased;

mod pager;

use pager::{Pager, PagerExtent};

pub struct ExtFilesystem {
    parser: ExtParser,
    pager: Arc<Pager>,
}

impl FileSystemOps for ExtFilesystem {
    fn name(&self) -> &'static FsStr {
        "ext4".into()
    }

    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(EXT4_SUPER_MAGIC))
    }
}

struct ExtNode {
    inode_num: u32,
    inode: INode,
    xattrs: ExtXattrMap,
}

impl ExtFilesystem {
    pub fn new_fs(
        locked: &mut Locked<'_, Unlocked>,
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

        let source_device = current_task.open_file(locked, options.source.as_ref(), open_flags)?;

        // Note that we *require* get_memory to work here for performance reasons.  Fallback to
        // FIDL-based read/write API is not an option.
        let memory = source_device.get_memory(locked, current_task, None, prot_flags)?;
        let vmo = Arc::new(
            memory
                .as_vmo()
                .ok_or_else(|| errno!(EINVAL))?
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .map_err(impossible_error)?,
        );
        let parser = ExtParser::new(Box::new(VmoReader::new(vmo.clone())));
        let pager = Arc::new(Pager::new(vmo, parser.block_size().map_err(|e| errno!(EIO, e))?)?);
        let fs = Self { parser, pager: pager.clone() };
        let ops = ExtDirectory { inner: Arc::new(ExtNode::new(&fs, ROOT_INODE_NUM)?) };
        let fs = FileSystem::new(
            current_task.kernel(),
            CacheMode::Cached(CacheConfig::default()),
            fs,
            options,
        )?;
        let mut root = FsNode::new_root(ops);
        root.node_id = ROOT_INODE_NUM as ino_t;
        fs.set_root_node(root);
        pager.start_pager_threads(current_task);

        Ok(fs)
    }
}

impl Drop for ExtFilesystem {
    fn drop(&mut self) {
        self.pager.terminate();
    }
}

impl ExtNode {
    fn new(fs: &ExtFilesystem, inode_num: u32) -> Result<ExtNode, Errno> {
        let inode = fs.parser.inode(inode_num).map_err(|e| errno!(EIO, e))?;
        let xattrs = fs.parser.inode_xattrs(inode_num).unwrap_or_default();
        Ok(ExtNode { inode_num, inode, xattrs })
    }
}

impl XattrStorage for ExtNode {
    fn list_xattrs(&self) -> Result<Vec<FsString>, Errno> {
        Ok(self.xattrs.keys().map(|k| k.clone().into()).collect())
    }

    fn get_xattr(&self, name: &FsStr) -> Result<FsString, Errno> {
        self.xattrs.get(&**name).map(|a| a.clone().into()).ok_or_else(|| errno!(ENODATA))
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
    fs_node_impl_dir_readonly!();
    fs_node_impl_xattr_delegate!(self, self.inner);

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ExtDirFileObject { inner: self.inner.clone() }))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let fs = node.fs();
        let fs_ops = fs.downcast_ops::<ExtFilesystem>().unwrap();
        let dir_entries =
            fs_ops.parser.entries_from_inode(&self.inner.inode).map_err(|e| errno!(EIO, e))?;
        let entry = dir_entries
            .iter()
            .find(|e| e.name_bytes() == name)
            .ok_or_else(|| errno!(ENOENT, name))?;
        let ext_node = ExtNode::new(fs_ops, entry.e2d_ino.into())?;
        let inode_num = ext_node.inode_num as ino_t;
        fs.get_or_create_node(current_task, Some(inode_num as ino_t), |inode_num| {
            let entry_type = EntryType::from_u8(entry.e2d_type).map_err(|e| errno!(EIO, e))?;
            let mode = FileMode::from_bits(ext_node.inode.e2di_mode.into());
            let owner =
                FsCred { uid: ext_node.inode.e2di_uid.into(), gid: ext_node.inode.e2di_gid.into() };
            let size = u32::from(ext_node.inode.e2di_size) as usize;
            let nlink = ext_node.inode.e2di_nlink.into();

            let ops: Box<dyn FsNodeOps> = match entry_type {
                EntryType::RegularFile => Box::new(ExtFile::new(ext_node, name.to_owned())),
                EntryType::Directory => Box::new(ExtDirectory { inner: Arc::new(ext_node) }),
                EntryType::SymLink => Box::new(ExtSymlink { inner: ext_node }),
                EntryType::Unknown => {
                    track_stub!(TODO("https://fxbug.dev/322873719"), "ext4 unknown entry type");
                    Box::new(ExtFile::new(ext_node, name.to_owned()))
                }
                EntryType::CharacterDevice => {
                    track_stub!(TODO("https://fxbug.dev/322874445"), "ext4 character device");
                    Box::new(ExtFile::new(ext_node, name.to_owned()))
                }
                EntryType::BlockDevice => {
                    track_stub!(TODO("https://fxbug.dev/322874062"), "ext4 block device");
                    Box::new(ExtFile::new(ext_node, name.to_owned()))
                }
                EntryType::FIFO => {
                    track_stub!(TODO("https://fxbug.dev/322874249"), "ext4 fifo");
                    Box::new(ExtFile::new(ext_node, name.to_owned()))
                }
                EntryType::Socket => {
                    track_stub!(TODO("https://fxbug.dev/322874081"), "ext4 socket");
                    Box::new(ExtFile::new(ext_node, name.to_owned()))
                }
            };

            let child = FsNode::new_uncached(
                current_task,
                ops,
                &fs,
                inode_num,
                FsNodeInfo {
                    ino: inode_num,
                    mode,
                    uid: owner.uid,
                    gid: owner.gid,
                    ..Default::default()
                },
            );
            child.update_info(|info| {
                info.size = size;
                info.link_count = nlink;
            });
            Ok(child)
        })
    }
}

struct ExtFile {
    inner: ExtNode,
    name: FsString,

    // The VMO here will be a child of the main VMO that the pager holds.  We want to keep it here
    // so that whilst ExtFile remains resident, we hold a child reference to the main VMO which
    // will prevent the pager from dropping the VMO (and any data we might have paged-in).
    memory: OnceCell<Arc<MemoryObject>>,
}

impl ExtFile {
    fn new(inner: ExtNode, name: FsString) -> Self {
        ExtFile { inner, name, memory: OnceCell::new() }
    }
}

impl FsNodeOps for ExtFile {
    fs_node_impl_not_dir!();
    fs_node_impl_xattr_delegate!(self, self.inner);

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let fs = node.fs();
        let fs_ops = fs.downcast_ops::<ExtFilesystem>().unwrap();
        let inode_num = self.inner.inode_num;
        let memory = self.memory.get_or_try_init(|| {
            let (file_size, extents) = fs_ops
                .parser
                .read_extents(self.inner.inode_num)
                .map_err(|e| errno!(EINVAL, format!("failed to read extents: {e}")))?;
            // The extents should be sorted which we rely on later.
            let mut pager_extents = Vec::with_capacity(extents.len());
            let mut last_block = 0;
            for e in extents {
                let pager_extent = PagerExtent::from(e);
                if pager_extent.logical.start < last_block {
                    return error!(EIO, "Bad extent");
                }
                last_block = pager_extent.logical.end;
                pager_extents.push(pager_extent);
            }
            Ok(Arc::new(MemoryObject::from(
                fs_ops
                    .pager
                    .register(self.name.as_ref(), inode_num, file_size, pager_extents.into())
                    .map_err(|e| errno!(EINVAL, e))?,
            )))
        })?;

        // TODO(https://fxbug.dev/42080696) returned memory shouldn't be writeable
        Ok(Box::new(MemoryFileObject::new(memory.clone())))
    }
}

impl From<ext4_read_only::structs::Extent> for PagerExtent {
    fn from(e: ext4_read_only::structs::Extent) -> Self {
        let block_count: u16 = e.e_len.into();
        let start = e.e_blk.into();
        Self { logical: start..start + block_count as u32, physical_block: e.target_block_num() }
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
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        let fs = node.fs();
        let fs_ops = fs.downcast_ops::<ExtFilesystem>().unwrap();
        let data = fs_ops.parser.read_data(self.inner.inode_num).map_err(|e| errno!(EIO, e))?;
        Ok(SymlinkTarget::Path(data.into()))
    }
}

struct ExtDirFileObject {
    inner: Arc<ExtNode>,
}

impl FileOps for ExtDirFileObject {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        Ok(default_seek(current_offset, target, |_| error!(EINVAL))?)
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        let fs = file.node().fs();
        let fs_ops = fs.downcast_ops::<ExtFilesystem>().unwrap();
        let dir_entries =
            fs_ops.parser.entries_from_inode(&self.inner.inode).map_err(|e| errno!(EIO, e))?;

        if sink.offset() as usize >= dir_entries.len() {
            return Ok(());
        }

        for entry in dir_entries[(sink.offset() as usize)..].iter() {
            let inode_num = entry.e2d_ino.into();
            let entry_type = directory_entry_type(
                EntryType::from_u8(entry.e2d_type).map_err(|e| errno!(EIO, e))?,
            );
            sink.add(inode_num, sink.offset() + 1, entry_type, entry.name_bytes().into())?;
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
