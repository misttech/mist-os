// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use super::{directory_file::MemoryDirectoryFile, *};
use crate::{
    auth::FsCred,
    lock::{Mutex, MutexGuard},
    logging::not_implemented,
    mm::PAGE_SIZE,
    task::{CurrentTask, Kernel},
    types::*,
};

pub struct TmpFs(());

impl FileSystemOps for Arc<TmpFs> {
    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        Ok(statfs {
            // Pretend we have a ton of free space.
            f_blocks: 0x100000000,
            f_bavail: 0x100000000,
            f_bfree: 0x100000000,
            ..statfs::default(TMPFS_MAGIC)
        })
    }
    fn name(&self) -> &'static FsStr {
        b"tmpfs"
    }

    fn rename(
        &self,
        _fs: &FileSystem,
        old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        renamed: &FsNodeHandle,
        replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        fn child_count(node: &FsNodeHandle) -> MutexGuard<'_, u32> {
            // The following cast are safe, unless something is seriously wrong:
            // - The filesystem should not be asked to rename node that it doesn't handle.
            // - Parents in a rename operation need to be directories.
            // - TmpfsDirectory is the ops for directories in this filesystem.
            node.downcast_ops::<TmpfsDirectory>().unwrap().child_count.lock()
        }
        if let Some(replaced) = replaced {
            if replaced.is_dir() {
                if !renamed.is_dir() {
                    return error!(EISDIR);
                }
                // Ensures that replaces is empty.
                if *child_count(replaced) != 0 {
                    return error!(ENOTEMPTY);
                }
            }
        }
        *child_count(old_parent) -= 1;
        *child_count(new_parent) += 1;
        if renamed.is_dir() {
            old_parent.update_info(|info| {
                info.link_count -= 1;
                Ok(())
            })?;
            new_parent.update_info(|info| {
                info.link_count += 1;
                Ok(())
            })?;
        }
        // Fix the wrong changes to new_parent due to the fact that the target element has
        // been replaced instead of added.
        if let Some(replaced) = replaced {
            if replaced.is_dir() {
                new_parent.update_info(|info| {
                    info.link_count -= 1;
                    Ok(())
                })?;
            }
            *child_count(new_parent) -= 1;
        }
        Ok(())
    }
}

impl TmpFs {
    pub fn new_fs(kernel: &Kernel) -> FileSystemHandle {
        Self::new_fs_with_options(kernel, Default::default()).expect("empty options cannot fail")
    }

    pub fn new_fs_with_options(
        kernel: &Kernel,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let fs = FileSystem::new(kernel, CacheMode::Permanent, Arc::new(TmpFs(())), options);
        let mut mount_options = fs_args::generic_parse_mount_options(&fs.options.params);
        let mode = if let Some(mode) = mount_options.remove(b"mode" as &FsStr) {
            FileMode::from_string(mode)?
        } else {
            mode!(IFDIR, 0o777)
        };
        let uid = if let Some(uid) = mount_options.remove(b"uid" as &FsStr) {
            fs_args::parse::<uid_t>(uid)?
        } else {
            0
        };
        let gid = if let Some(gid) = mount_options.remove(b"gid" as &FsStr) {
            fs_args::parse::<gid_t>(gid)?
        } else {
            0
        };
        let root_node = FsNode::new_root_with_properties(TmpfsDirectory::new(), |info| {
            info.chmod(mode);
            info.uid = uid;
            info.gid = gid;
        });
        fs.set_root_node(root_node);

        if !mount_options.is_empty() {
            not_implemented!(
                "Unknown tmpfs option: {:?}",
                itertools::join(
                    mount_options.iter().map(|(k, v)| format!(
                        "{}={}",
                        String::from_utf8_lossy(k),
                        String::from_utf8_lossy(v)
                    )),
                    ","
                )
            );
        }

        Ok(fs)
    }
}

pub struct TmpfsDirectory {
    xattrs: MemoryXattrStorage,
    child_count: Mutex<u32>,
}

impl TmpfsDirectory {
    pub fn new() -> Self {
        Self { xattrs: MemoryXattrStorage::default(), child_count: Mutex::new(0) }
    }
}

impl FsNodeOps for TmpfsDirectory {
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MemoryDirectoryFile::new()))
    }

    fn lookup(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        error!(ENOENT, format!("looking for {:?}", String::from_utf8_lossy(name)))
    }

    fn mkdir(
        &self,
        node: &FsNode,
        _name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        node.update_info(|info| {
            info.link_count += 1;
            Ok(())
        })?;
        *self.child_count.lock() += 1;
        Ok(node.fs().create_node(TmpfsDirectory::new(), FsNodeInfo::new_factory(mode, owner)))
    }

    fn mknod(
        &self,
        node: &FsNode,
        _name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let ops: Box<dyn FsNodeOps> = match mode.fmt() {
            // For files created in tmpfs, forbid sealing, by sealing the seal operation.
            FileMode::IFREG => Box::new(VmoFileNode::new(SealFlags::SEAL)?),
            FileMode::IFIFO | FileMode::IFBLK | FileMode::IFCHR | FileMode::IFSOCK => {
                Box::new(TmpfsSpecialNode::new())
            }
            _ => return error!(EACCES),
        };
        *self.child_count.lock() += 1;
        let node = node.fs().create_node_box(ops, move |id| {
            let mut info = FsNodeInfo::new(id, mode, owner);
            info.rdev = dev;
            // blksize is PAGE_SIZE for in memory node.
            info.blksize = *PAGE_SIZE as usize;
            info
        });
        Ok(node)
    }

    fn create_symlink(
        &self,
        node: &FsNode,
        _name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        *self.child_count.lock() += 1;
        Ok(node.fs().create_node(
            SymlinkNode::new(target),
            FsNodeInfo::new_factory(mode!(IFLNK, 0o777), owner),
        ))
    }

    fn link(&self, _node: &FsNode, _name: &FsStr, child: &FsNodeHandle) -> Result<(), Errno> {
        child.update_info(|info| {
            info.link_count += 1;
            Ok(())
        })?;
        *self.child_count.lock() += 1;
        Ok(())
    }

    fn unlink(&self, node: &FsNode, _name: &FsStr, child: &FsNodeHandle) -> Result<(), Errno> {
        if child.is_dir() {
            node.update_info(|info| {
                info.link_count -= 1;
                Ok(())
            })?;
        }
        child.update_info(|info| {
            info.link_count -= 1;
            Ok(())
        })?;
        *self.child_count.lock() -= 1;
        Ok(())
    }
}

struct TmpfsSpecialNode {
    xattrs: MemoryXattrStorage,
}

impl TmpfsSpecialNode {
    pub fn new() -> Self {
        Self { xattrs: MemoryXattrStorage::default() }
    }
}

impl FsNodeOps for TmpfsSpecialNode {
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        unreachable!("Special nodes cannot be opened.");
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        fs::buffers::{VecInputBuffer, VecOutputBuffer},
        testing::*,
    };
    use zerocopy::AsBytes;

    #[::fuchsia::test]
    async fn test_tmpfs() {
        let (kernel, current_task) = create_kernel_and_task();
        let fs = TmpFs::new_fs(&kernel);
        let root = fs.root();
        let usr = root.create_dir(&current_task, b"usr").unwrap();
        let _etc = root.create_dir(&current_task, b"etc").unwrap();
        let _usr_bin = usr.create_dir(&current_task, b"bin").unwrap();
        let mut names = root.copy_child_names();
        names.sort();
        assert!(names.iter().eq([b"etc", b"usr"].iter()));
    }

    #[::fuchsia::test]
    async fn test_write_read() {
        let (_kernel, current_task) = create_kernel_and_task();

        let path = b"test.bin";
        let _file = current_task
            .fs()
            .root()
            .create_node(&current_task, path, mode!(IFREG, 0o777), DeviceType::NONE)
            .unwrap();

        let wr_file = current_task.open_file(path, OpenFlags::RDWR).unwrap();

        let test_seq = 0..10000u16;
        let test_vec = test_seq.collect::<Vec<_>>();
        let test_bytes = test_vec.as_slice().as_bytes();

        let written = wr_file.write(&current_task, &mut VecInputBuffer::new(test_bytes)).unwrap();
        assert_eq!(written, test_bytes.len());

        let mut read_buffer = VecOutputBuffer::new(test_bytes.len() + 1);
        let read = wr_file.read_at(&current_task, 0, &mut read_buffer).unwrap();
        assert_eq!(read, test_bytes.len());
        assert_eq!(test_bytes, read_buffer.data());
    }

    #[::fuchsia::test]
    async fn test_read_past_eof() {
        let (_kernel, current_task) = create_kernel_and_task();

        // Open an empty file
        let path = b"test.bin";
        let _file = current_task
            .fs()
            .root()
            .create_node(&current_task, path, mode!(IFREG, 0o777), DeviceType::NONE)
            .unwrap();
        let rd_file = current_task.open_file(path, OpenFlags::RDONLY).unwrap();

        // Verify that attempting to read past the EOF (i.e. at a non-zero offset) returns 0
        let buffer_size = 0x10000;
        let mut output_buffer = VecOutputBuffer::new(buffer_size);
        let test_offset = 100;
        let result = rd_file.read_at(&current_task, test_offset, &mut output_buffer).unwrap();
        assert_eq!(result, 0);
    }

    #[::fuchsia::test]
    async fn test_permissions() {
        let (_kernel, current_task) = create_kernel_and_task();

        let path = b"test.bin";
        let file = current_task
            .open_file_at(
                FdNumber::AT_FDCWD,
                path,
                OpenFlags::CREAT | OpenFlags::RDONLY,
                FileMode::from_bits(0o777),
            )
            .expect("failed to create file");
        assert_eq!(
            0,
            file.read(&current_task, &mut VecOutputBuffer::new(0)).expect("failed to read")
        );
        assert!(file.write(&current_task, &mut VecInputBuffer::new(&[])).is_err());

        let file = current_task
            .open_file_at(FdNumber::AT_FDCWD, path, OpenFlags::WRONLY, FileMode::EMPTY)
            .expect("failed to open file WRONLY");
        assert!(file.read(&current_task, &mut VecOutputBuffer::new(0)).is_err());
        assert_eq!(
            0,
            file.write(&current_task, &mut VecInputBuffer::new(&[])).expect("failed to write")
        );

        let file = current_task
            .open_file_at(FdNumber::AT_FDCWD, path, OpenFlags::RDWR, FileMode::EMPTY)
            .expect("failed to open file RDWR");
        assert_eq!(
            0,
            file.read(&current_task, &mut VecOutputBuffer::new(0)).expect("failed to read")
        );
        assert_eq!(
            0,
            file.write(&current_task, &mut VecInputBuffer::new(&[])).expect("failed to write")
        );
    }

    #[::fuchsia::test]
    async fn test_persistence() {
        let (_kernel, current_task) = create_kernel_and_task();

        {
            let root = &current_task.fs().root().entry;
            let usr = root.create_dir(&current_task, b"usr").expect("failed to create usr");
            root.create_dir(&current_task, b"etc").expect("failed to create usr/etc");
            usr.create_dir(&current_task, b"bin").expect("failed to create usr/bin");
        }

        // At this point, all the nodes are dropped.

        current_task
            .open_file(b"/usr/bin", OpenFlags::RDONLY | OpenFlags::DIRECTORY)
            .expect("failed to open /usr/bin");
        assert_eq!(
            errno!(ENOENT),
            current_task.open_file(b"/usr/bin/test.txt", OpenFlags::RDWR).unwrap_err()
        );
        current_task
            .open_file_at(
                FdNumber::AT_FDCWD,
                b"/usr/bin/test.txt",
                OpenFlags::RDWR | OpenFlags::CREAT,
                FileMode::from_bits(0o777),
            )
            .expect("failed to create test.txt");
        let txt = current_task
            .open_file(b"/usr/bin/test.txt", OpenFlags::RDWR)
            .expect("failed to open test.txt");

        let usr_bin = current_task
            .open_file(b"/usr/bin", OpenFlags::RDONLY)
            .expect("failed to open /usr/bin");
        usr_bin
            .name
            .unlink(&current_task, b"test.txt", UnlinkKind::NonDirectory, false)
            .expect("failed to unlink test.text");
        assert_eq!(
            errno!(ENOENT),
            current_task.open_file(b"/usr/bin/test.txt", OpenFlags::RDWR).unwrap_err()
        );
        assert_eq!(
            errno!(ENOENT),
            usr_bin
                .name
                .unlink(&current_task, b"test.txt", UnlinkKind::NonDirectory, false)
                .unwrap_err()
        );

        assert_eq!(
            0,
            txt.read(&current_task, &mut VecOutputBuffer::new(0)).expect("failed to read")
        );
        std::mem::drop(txt);
        assert_eq!(
            errno!(ENOENT),
            current_task.open_file(b"/usr/bin/test.txt", OpenFlags::RDWR).unwrap_err()
        );
        std::mem::drop(usr_bin);

        let usr = current_task.open_file(b"/usr", OpenFlags::RDONLY).expect("failed to open /usr");
        assert_eq!(
            errno!(ENOENT),
            current_task.open_file(b"/usr/foo", OpenFlags::RDONLY).unwrap_err()
        );
        usr.name
            .unlink(&current_task, b"bin", UnlinkKind::Directory, false)
            .expect("failed to unlink /usr/bin");
    }

    #[::fuchsia::test]
    async fn test_data() {
        let (kernel, _current_task) = create_kernel_and_task();
        let fs = TmpFs::new_fs_with_options(
            &kernel,
            FileSystemOptions {
                source: b"".to_vec(),
                flags: MountFlags::empty(),
                params: b"mode=0123,uid=42,gid=84".to_vec(),
            },
        )
        .expect("new_fs");
        let info = fs.root().node.info();
        assert_eq!(info.mode, mode!(IFDIR, 0o123));
        assert_eq!(info.uid, 42);
        assert_eq!(info.gid, 84);
    }
}
