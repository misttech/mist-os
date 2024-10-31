// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::PAGE_SIZE;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::directory_file::MemoryDirectoryFile;
use crate::vfs::{
    fs_args, fs_node_impl_not_dir, fs_node_impl_xattr_delegate, CacheMode, FileOps, FileSystem,
    FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, MemoryFileNode, MemoryXattrStorage, SymlinkNode, XattrStorage as _,
};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::{FileOpsCore, Locked, Mutex, MutexGuard, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::seal_flags::SealFlags;
use starnix_uapi::{error, gid_t, statfs, uid_t, TMPFS_MAGIC};
use std::sync::Arc;

pub struct TmpFs(());

impl FileSystemOps for Arc<TmpFs> {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(statfs {
            // Pretend we have a ton of free space.
            f_blocks: 0x100000000,
            f_bavail: 0x100000000,
            f_bfree: 0x100000000,
            ..default_statfs(TMPFS_MAGIC)
        })
    }
    fn name(&self) -> &'static FsStr {
        "tmpfs".into()
    }

    fn rename(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
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
            });
            new_parent.update_info(|info| {
                info.link_count += 1;
            });
        }
        // Fix the wrong changes to new_parent due to the fact that the target element has
        // been replaced instead of added.
        if let Some(replaced) = replaced {
            if replaced.is_dir() {
                new_parent.update_info(|info| {
                    info.link_count -= 1;
                });
            }
            *child_count(new_parent) -= 1;
        }
        Ok(())
    }

    fn exchange(
        &self,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        node1: &FsNodeHandle,
        parent1: &FsNodeHandle,
        _name1: &FsStr,
        node2: &FsNodeHandle,
        parent2: &FsNodeHandle,
        _name2: &FsStr,
    ) -> Result<(), Errno> {
        if node1.is_dir() {
            parent1.update_info(|info| {
                info.link_count -= 1;
            });
            parent2.update_info(|info| {
                info.link_count += 1;
            });
        }

        if node2.is_dir() {
            parent1.update_info(|info| {
                info.link_count += 1;
            });
            parent2.update_info(|info| {
                info.link_count -= 1;
            });
        }

        Ok(())
    }
}

pub fn tmp_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    TmpFs::new_fs_with_options(&current_task.kernel(), options)
}

impl TmpFs {
    pub fn new_fs(kernel: &Arc<Kernel>) -> FileSystemHandle {
        Self::new_fs_with_options(kernel, Default::default()).expect("empty options cannot fail")
    }

    pub fn new_fs_with_options(
        kernel: &Arc<Kernel>,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let fs = FileSystem::new(kernel, CacheMode::Permanent, Arc::new(TmpFs(())), options)?;
        let mut mount_options = fs.options.params.clone();
        let mode = if let Some(mode) = mount_options.remove(b"mode") {
            FileMode::from_string(mode.as_ref())?
        } else {
            mode!(IFDIR, 0o777)
        };
        let uid = if let Some(uid) = mount_options.remove(b"uid") {
            fs_args::parse::<uid_t>(uid.as_ref())?
        } else {
            0
        };
        let gid = if let Some(gid) = mount_options.remove(b"gid") {
            fs_args::parse::<gid_t>(gid.as_ref())?
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
            track_stub!(
                TODO("https://fxbug.dev/322873419"),
                "unknown tmpfs options, see logs for strings"
            );
            log_warn!("Unknown tmpfs options: {}", mount_options);
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

fn create_child_node(
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
    dev: DeviceType,
    owner: FsCred,
) -> Result<FsNodeHandle, Errno> {
    let ops: Box<dyn FsNodeOps> = match mode.fmt() {
        FileMode::IFREG => Box::new(MemoryFileNode::new()?),
        FileMode::IFIFO | FileMode::IFBLK | FileMode::IFCHR | FileMode::IFSOCK => {
            Box::new(TmpfsSpecialNode::new())
        }
        _ => return error!(EACCES),
    };
    let child = parent.fs().create_node(current_task, ops, move |id| {
        let mut info = FsNodeInfo::new(id, mode, owner);
        info.rdev = dev;
        // blksize is PAGE_SIZE for in memory node.
        info.blksize = *PAGE_SIZE as usize;
        info
    });
    if mode.fmt() == FileMode::IFREG {
        // For files created in tmpfs, forbid sealing, by sealing the seal operation.
        child.write_guard_state.lock().enable_sealing(SealFlags::SEAL);
    }
    Ok(child)
}

impl FsNodeOps for TmpfsDirectory {
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MemoryDirectoryFile::new()))
    }

    fn mkdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        _name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        node.update_info(|info| {
            info.link_count += 1;
        });
        *self.child_count.lock() += 1;
        Ok(node.fs().create_node(
            current_task,
            TmpfsDirectory::new(),
            FsNodeInfo::new_factory(mode, owner),
        ))
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        _name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let child = create_child_node(current_task, node, mode, dev, owner)?;
        *self.child_count.lock() += 1;
        Ok(child)
    }

    fn create_symlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        _name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        *self.child_count.lock() += 1;
        let (link, info) = SymlinkNode::new(target, owner);
        Ok(node.fs().create_node(current_task, link, info))
    }

    fn create_tmpfile(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        assert!(mode.is_reg());
        create_child_node(current_task, node, mode, DeviceType::NONE, owner)
    }

    fn link(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        child.update_info(|info| {
            info.link_count += 1;
        });
        *self.child_count.lock() += 1;
        Ok(())
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        if child.is_dir() {
            node.update_info(|info| {
                info.link_count -= 1;
            });
        }
        child.update_info(|info| {
            info.link_count -= 1;
        });
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
    fs_node_impl_not_dir!();
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        unreachable!("Special nodes cannot be opened.");
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;
    use crate::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
    use crate::vfs::fs_args::MountParams;
    use crate::vfs::{FdNumber, UnlinkKind};
    use starnix_uapi::errno;
    use starnix_uapi::file_mode::AccessCheck;
    use starnix_uapi::mount_flags::MountFlags;
    use starnix_uapi::vfs::ResolveFlags;
    use zerocopy::IntoBytes;

    #[::fuchsia::test]
    async fn test_tmpfs() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let fs = TmpFs::new_fs(&kernel);
        let root = fs.root();
        let usr = root.create_dir(&mut locked, &current_task, "usr".into()).unwrap();
        let _etc = root.create_dir(&mut locked, &current_task, "etc".into()).unwrap();
        let _usr_bin = usr.create_dir(&mut locked, &current_task, "bin".into()).unwrap();
        let mut names = root.copy_child_names();
        names.sort();
        assert!(names.iter().eq(["etc", "usr"].iter()));
    }

    #[::fuchsia::test]
    async fn test_write_read() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let path = "test.bin";
        let _file = current_task
            .fs()
            .root()
            .create_node(
                &mut locked,
                &current_task,
                path.into(),
                mode!(IFREG, 0o777),
                DeviceType::NONE,
            )
            .unwrap();

        let wr_file = current_task.open_file(&mut locked, path.into(), OpenFlags::RDWR).unwrap();

        let test_seq = 0..10000u16;
        let test_vec = test_seq.collect::<Vec<_>>();
        let test_bytes = test_vec.as_slice().as_bytes();

        let written = wr_file
            .write(&mut locked, &current_task, &mut VecInputBuffer::new(test_bytes))
            .unwrap();
        assert_eq!(written, test_bytes.len());

        let mut read_buffer = VecOutputBuffer::new(test_bytes.len() + 1);
        let read = wr_file.read_at(&mut locked, &current_task, 0, &mut read_buffer).unwrap();
        assert_eq!(read, test_bytes.len());
        assert_eq!(test_bytes, read_buffer.data());
    }

    #[::fuchsia::test]
    async fn test_read_past_eof() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        // Open an empty file
        let path = "test.bin";
        let _file = current_task
            .fs()
            .root()
            .create_node(
                &mut locked,
                &current_task,
                path.into(),
                mode!(IFREG, 0o777),
                DeviceType::NONE,
            )
            .unwrap();
        let rd_file = current_task.open_file(&mut locked, path.into(), OpenFlags::RDONLY).unwrap();

        // Verify that attempting to read past the EOF (i.e. at a non-zero offset) returns 0
        let buffer_size = 0x10000;
        let mut output_buffer = VecOutputBuffer::new(buffer_size);
        let test_offset = 100;
        let result =
            rd_file.read_at(&mut locked, &current_task, test_offset, &mut output_buffer).unwrap();
        assert_eq!(result, 0);
    }

    #[::fuchsia::test]
    async fn test_permissions() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let path = "test.bin";
        let file = current_task
            .open_file_at(
                &mut locked,
                FdNumber::AT_FDCWD,
                path.into(),
                OpenFlags::CREAT | OpenFlags::RDONLY,
                FileMode::from_bits(0o777),
                ResolveFlags::empty(),
                AccessCheck::default(),
            )
            .expect("failed to create file");
        assert_eq!(
            0,
            file.read(&mut locked, &current_task, &mut VecOutputBuffer::new(0))
                .expect("failed to read")
        );

        assert!(file.write(&mut locked, &current_task, &mut VecInputBuffer::new(&[])).is_err());

        let file = current_task
            .open_file_at(
                &mut locked,
                FdNumber::AT_FDCWD,
                path.into(),
                OpenFlags::WRONLY,
                FileMode::EMPTY,
                ResolveFlags::empty(),
                AccessCheck::default(),
            )
            .expect("failed to open file WRONLY");

        assert!(file.read(&mut locked, &current_task, &mut VecOutputBuffer::new(0)).is_err());

        assert_eq!(
            0,
            file.write(&mut locked, &current_task, &mut VecInputBuffer::new(&[]))
                .expect("failed to write")
        );

        let file = current_task
            .open_file_at(
                &mut locked,
                FdNumber::AT_FDCWD,
                path.into(),
                OpenFlags::RDWR,
                FileMode::EMPTY,
                ResolveFlags::empty(),
                AccessCheck::default(),
            )
            .expect("failed to open file RDWR");

        assert_eq!(
            0,
            file.read(&mut locked, &current_task, &mut VecOutputBuffer::new(0))
                .expect("failed to read")
        );

        assert_eq!(
            0,
            file.write(&mut locked, &current_task, &mut VecInputBuffer::new(&[]))
                .expect("failed to write")
        );
    }

    #[::fuchsia::test]
    async fn test_persistence() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        {
            let root = &current_task.fs().root().entry;
            let usr = root
                .create_dir(&mut locked, &current_task, "usr".into())
                .expect("failed to create usr");
            root.create_dir(&mut locked, &current_task, "etc".into())
                .expect("failed to create usr/etc");
            usr.create_dir(&mut locked, &current_task, "bin".into())
                .expect("failed to create usr/bin");
        }

        // At this point, all the nodes are dropped.

        current_task
            .open_file(&mut locked, "/usr/bin".into(), OpenFlags::RDONLY | OpenFlags::DIRECTORY)
            .expect("failed to open /usr/bin");
        assert_eq!(
            errno!(ENOENT),
            current_task
                .open_file(&mut locked, "/usr/bin/test.txt".into(), OpenFlags::RDWR)
                .unwrap_err()
        );
        current_task
            .open_file_at(
                &mut locked,
                FdNumber::AT_FDCWD,
                "/usr/bin/test.txt".into(),
                OpenFlags::RDWR | OpenFlags::CREAT,
                FileMode::from_bits(0o777),
                ResolveFlags::empty(),
                AccessCheck::default(),
            )
            .expect("failed to create test.txt");
        let txt = current_task
            .open_file(&mut locked, "/usr/bin/test.txt".into(), OpenFlags::RDWR)
            .expect("failed to open test.txt");

        let usr_bin = current_task
            .open_file(&mut locked, "/usr/bin".into(), OpenFlags::RDONLY)
            .expect("failed to open /usr/bin");
        usr_bin
            .name
            .unlink(&mut locked, &current_task, "test.txt".into(), UnlinkKind::NonDirectory, false)
            .expect("failed to unlink test.text");
        assert_eq!(
            errno!(ENOENT),
            current_task
                .open_file(&mut locked, "/usr/bin/test.txt".into(), OpenFlags::RDWR)
                .unwrap_err()
        );
        assert_eq!(
            errno!(ENOENT),
            usr_bin
                .name
                .unlink(
                    &mut locked,
                    &current_task,
                    "test.txt".into(),
                    UnlinkKind::NonDirectory,
                    false
                )
                .unwrap_err()
        );

        assert_eq!(
            0,
            txt.read(&mut locked, &current_task, &mut VecOutputBuffer::new(0))
                .expect("failed to read")
        );
        std::mem::drop(txt);
        assert_eq!(
            errno!(ENOENT),
            current_task
                .open_file(&mut locked, "/usr/bin/test.txt".into(), OpenFlags::RDWR)
                .unwrap_err()
        );
        std::mem::drop(usr_bin);

        let usr = current_task
            .open_file(&mut locked, "/usr".into(), OpenFlags::RDONLY)
            .expect("failed to open /usr");
        assert_eq!(
            errno!(ENOENT),
            current_task.open_file(&mut locked, "/usr/foo".into(), OpenFlags::RDONLY).unwrap_err()
        );
        usr.name
            .unlink(&mut locked, &current_task, "bin".into(), UnlinkKind::Directory, false)
            .expect("failed to unlink /usr/bin");
    }

    #[::fuchsia::test]
    async fn test_data() {
        let (kernel, _current_task) = create_kernel_and_task();
        let fs = TmpFs::new_fs_with_options(
            &kernel,
            FileSystemOptions {
                source: Default::default(),
                flags: MountFlags::empty(),
                params: MountParams::parse(b"mode=0123,uid=42,gid=84".into())
                    .expect("parsed correctly"),
            },
        )
        .expect("new_fs");
        let info = fs.root().node.info();
        assert_eq!(info.mode, mode!(IFDIR, 0o123));
        assert_eq!(info.uid, 42);
        assert_eq!(info.gid, 84);
    }
}
