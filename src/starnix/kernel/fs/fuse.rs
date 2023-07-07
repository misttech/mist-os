// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    fs::{
        buffers::{InputBuffer, OutputBuffer, OutputBufferCallback},
        default_eof_offset, default_fcntl, default_ioctl, default_seek, fileops_impl_nonseekable,
        fs_args, CacheMode, DirEntry, DirectoryEntryType, DirentSink, FdEvents, FdNumber,
        FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString, SeekTarget, SymlinkTarget,
        ValueOrSize, XattrOp,
    },
    lock::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard},
    logging::{log_error, log_trace, log_warn, not_implemented, not_implemented_log_once},
    mm::{vmo::round_up_to_increment, PAGE_SIZE},
    syscalls::{SyscallArg, SyscallResult},
    task::{CurrentTask, EventHandler, Kernel, WaitCanceler, WaitQueue, Waiter},
    types::{
        errno,
        errno::{EINTR, EINVAL, ENOSYS},
        errno_from_code, error, off_t, statfs, time_from_timespec, uapi, DeviceType, Errno,
        FileMode, OpenFlags, FUSE_SUPER_MAGIC,
    },
};
use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    sync::Arc,
};
use zerocopy::{AsBytes, FromBytes, FromZeroes};

const CONFIGURATION_AVAILABLE_EVENT: u64 = u64::MAX;

#[derive(Debug, Default)]
pub struct DevFuse {
    connection: Arc<FuseConnection>,
}

impl FileOps for DevFuse {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN, None, || self.connection.read(data))
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        self.connection.write(data)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.connection.wait_async(waiter, events, handler)
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.connection.query_events())
    }
}

pub fn new_fuse_fs(
    task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let mut mount_options = fs_args::generic_parse_mount_options(&options.params);
    let fd = fs_args::parse::<FdNumber>(
        mount_options.remove(b"fd" as &FsStr).ok_or_else(|| errno!(EINVAL))?,
    )?;
    let connection = task
        .files
        .get(fd)?
        .downcast_file::<DevFuse>()
        .ok_or_else(|| errno!(EINVAL))?
        .connection
        .clone();

    let fs =
        FileSystem::new(task.kernel(), CacheMode::Cached, FuseFs::new(connection.clone()), options);
    let fuse_node = Arc::new(FuseNode {
        connection: connection.clone(),
        nodeid: uapi::FUSE_ROOT_ID as u64,
        state: Default::default(),
    });
    fuse_node.state.lock().nlookup += 1;
    let mut root_node = FsNode::new_root(fuse_node.clone());
    root_node.node_id = uapi::FUSE_ROOT_ID as u64;
    fs.set_root_node(root_node);
    connection.execute_operation(task, &fuse_node, FuseOperation::Init)?;
    Ok(fs)
}

#[derive(Debug)]
struct FuseFs {
    connection: Arc<FuseConnection>,
}

impl FuseFs {
    fn new(connection: Arc<FuseConnection>) -> Self {
        Self { connection }
    }
}

impl FileSystemOps for FuseFs {
    fn rename(
        &self,
        _fs: &FileSystem,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    fn generate_node_ids(&self) -> bool {
        true
    }

    fn statfs(&self, fs: &FileSystem, current_task: &CurrentTask) -> Result<statfs, Errno> {
        let node = if let Ok(node) = FuseNode::from_node(&fs.root().node) {
            node
        } else {
            log_error!("Unexpected file type");
            return error!(EINVAL);
        };
        let response =
            self.connection.execute_operation(current_task, node, FuseOperation::Statfs)?;
        let statfs_out = if let FuseResponse::Statfs(statfs_out) = response {
            statfs_out
        } else {
            return error!(EINVAL);
        };
        Ok(statfs {
            f_blocks: statfs_out.st.blocks.try_into().map_err(|_| errno!(EINVAL))?,
            f_bfree: statfs_out.st.bfree.try_into().map_err(|_| errno!(EINVAL))?,
            f_bavail: statfs_out.st.bavail.try_into().map_err(|_| errno!(EINVAL))?,
            f_files: statfs_out.st.files.try_into().map_err(|_| errno!(EINVAL))?,
            f_ffree: statfs_out.st.ffree.try_into().map_err(|_| errno!(EINVAL))?,
            f_bsize: statfs_out.st.bsize.try_into().map_err(|_| errno!(EINVAL))?,
            f_namelen: statfs_out.st.namelen.try_into().map_err(|_| errno!(EINVAL))?,
            f_frsize: statfs_out.st.frsize.try_into().map_err(|_| errno!(EINVAL))?,
            ..statfs::default(FUSE_SUPER_MAGIC)
        })
    }
    fn name(&self) -> &'static FsStr {
        b"fuse"
    }
    fn unmount(&self) {
        self.connection.disconnect();
    }
}

#[derive(Debug, Default)]
struct FuseNodeMutableState {
    nlookup: u64,
}

#[derive(Debug)]
struct FuseNode {
    connection: Arc<FuseConnection>,
    nodeid: u64,
    state: Mutex<FuseNodeMutableState>,
}

impl FuseNode {
    fn from_node(node: &FsNode) -> Result<&Arc<FuseNode>, Errno> {
        node.downcast_ops::<Arc<FuseNode>>().ok_or_else(|| errno!(ENOENT))
    }

    fn update_node_info(info: &mut FsNodeInfo, attributes: uapi::fuse_attr) -> Result<(), Errno> {
        info.ino = attributes.ino as uapi::ino_t;
        info.mode = FileMode::from_bits(attributes.mode);
        info.size = attributes.size.try_into().map_err(|_| errno!(EINVAL))?;
        info.blocks = attributes.blocks.try_into().map_err(|_| errno!(EINVAL))?;
        info.blksize = attributes.blksize.try_into().map_err(|_| errno!(EINVAL))?;
        info.uid = attributes.uid;
        info.gid = attributes.gid;
        info.link_count = attributes.nlink.try_into().map_err(|_| errno!(EINVAL))?;
        info.time_status_change = time_from_timespec(uapi::timespec {
            tv_sec: attributes.ctime as i64,
            tv_nsec: attributes.ctimensec as i64,
        })?;
        info.time_access = time_from_timespec(uapi::timespec {
            tv_sec: attributes.atime as i64,
            tv_nsec: attributes.atimensec as i64,
        })?;
        info.time_modify = time_from_timespec(uapi::timespec {
            tv_sec: attributes.mtime as i64,
            tv_nsec: attributes.mtimensec as i64,
        })?;
        info.rdev = DeviceType::from_bits(attributes.rdev as u64);
        Ok(())
    }

    /// Build a FsNodeHandle from a FuseResponse that is expected to be a FuseResponse::Entry.
    fn fs_node_from_entry(
        &self,
        node: &FsNode,
        name: &FsStr,
        response: FuseResponse,
    ) -> Result<FsNodeHandle, Errno> {
        let entry = if let FuseResponse::Entry(entry) = response {
            entry
        } else {
            return error!(EINVAL);
        };
        if entry.nodeid == 0 {
            return error!(ENOENT);
        }
        let node = node.fs().get_or_create_node(Some(entry.nodeid), |id| {
            let fuse_node = Arc::new(FuseNode {
                connection: self.connection.clone(),
                nodeid: entry.nodeid,
                state: Default::default(),
            });
            let mut info = FsNodeInfo::default();
            FuseNode::update_node_info(&mut info, entry.attr)?;
            Ok(FsNode::new_uncached(Box::new(fuse_node), &node.fs(), id, info))
        })?;
        // . and .. do not get their lookup count increased.
        if !DirEntry::is_reserved_name(name) {
            let fuse_node = FuseNode::from_node(&node)?;
            fuse_node.state.lock().nlookup += 1;
        }
        Ok(node)
    }
}

struct FuseFileObject {
    connection: Arc<FuseConnection>,
    /// The response to the open calls from the userspace process.
    open_out: uapi::fuse_open_out,

    /// The current kernel. This is a temporary measure to access a task on close and flush.
    // TODO(https://fxbug.dev/128843): Remove this.
    kernel: Arc<Kernel>,
}

impl FuseFileObject {
    /// Returns the `FuseNode` associated with the opened file.
    fn get_fuse_node<'a>(&self, file: &'a FileObject) -> Result<&'a Arc<FuseNode>, Errno> {
        FuseNode::from_node(file.node())
    }
}

impl FileOps for FuseFileObject {
    fn close(&self, file: &FileObject) {
        let node = if let Ok(node) = self.get_fuse_node(file) {
            node
        } else {
            log_error!("Unexpected file type");
            return;
        };
        // TODO(https://fxbug.dev/128843): This should receives a CurrentTask instead of relying on
        // the system task.
        let mode = file.node().info().mode;
        if let Err(e) = self.connection.execute_operation(
            self.kernel.kthreads.system_task(),
            node,
            FuseOperation::Release { flags: file.flags(), mode, open_out: self.open_out },
        ) {
            log_error!("Error when relasing fh: {e:?}");
        }
    }

    fn flush(&self, file: &FileObject) {
        let node = if let Ok(node) = self.get_fuse_node(file) {
            node
        } else {
            log_error!("Unexpected file type");
            return;
        };
        // TODO(https://fxbug.dev/128843): This should receives a CurrentTask instead of relying on
        // the system task.
        if let Err(e) = self.connection.execute_operation(
            self.kernel.kthreads.system_task(),
            node,
            FuseOperation::Flush(self.open_out),
        ) {
            log_error!("Error when flushing fh: {e:?}");
        }
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let node = self.get_fuse_node(file)?;
        let response = self.connection.execute_operation(
            current_task,
            node,
            FuseOperation::Read(uapi::fuse_read_in {
                fh: self.open_out.fh,
                offset: offset.try_into().map_err(|_| errno!(EINVAL))?,
                size: data.available().try_into().unwrap_or(u32::MAX),
                read_flags: 0,
                lock_owner: 0,
                flags: 0,
                padding: 0,
            }),
        )?;
        let read_out = if let FuseResponse::Read(read_out) = response {
            read_out
        } else {
            return error!(EINVAL);
        };
        data.write(&read_out)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let node = self.get_fuse_node(file)?;
        let content = data.peek_all()?;
        let response = self.connection.execute_operation(
            current_task,
            node,
            FuseOperation::Write {
                write_in: uapi::fuse_write_in {
                    fh: self.open_out.fh,
                    offset: offset.try_into().map_err(|_| errno!(EINVAL))?,
                    size: content.len().try_into().map_err(|_| errno!(EINVAL))?,
                    write_flags: 0,
                    lock_owner: 0,
                    flags: 0,
                    padding: 0,
                },
                content,
            },
        )?;
        let write_out = if let FuseResponse::Write(write_out) = response {
            write_out
        } else {
            return error!(EINVAL);
        };

        let written = write_out.size as usize;

        data.advance(written)?;
        Ok(written)
    }

    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        // Only delegate SEEK_DATA and SEEK_HOLE to the userspace process.
        if matches!(target, SeekTarget::Data(_) | SeekTarget::Hole(_)) {
            let node = self.get_fuse_node(file)?;
            let response = self.connection.execute_operation(
                current_task,
                node,
                FuseOperation::Seek(uapi::fuse_lseek_in {
                    fh: self.open_out.fh,
                    offset: target.offset().try_into().map_err(|_| errno!(EINVAL))?,
                    whence: target.whence(),
                    padding: 0,
                }),
            );
            match response {
                Ok(response) => {
                    let seek_out = if let FuseResponse::Seek(seek_out) = response {
                        seek_out
                    } else {
                        return error!(EINVAL);
                    };
                    return seek_out.offset.try_into().map_err(|_| errno!(EINVAL));
                }
                // If errno is ENOSYS, the userspace process doesn't support the seek operation and
                // the default seek must be used.
                Err(errno) if errno == ENOSYS => {}
                Err(errno) => return Err(errno),
            };
        }

        default_seek(current_offset, target, |offset| {
            let eof_offset = default_eof_offset(file, current_task)?;
            offset.checked_add(eof_offset).ok_or_else(|| errno!(EINVAL))
        })
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        None
    }

    fn query_events(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let node = self.get_fuse_node(file)?;
        let response = self.connection.execute_operation(
            current_task,
            node,
            FuseOperation::Poll(uapi::fuse_poll_in {
                fh: self.open_out.fh,
                kh: 0,
                flags: 0,
                events: FdEvents::all().bits(),
            }),
        )?;
        let poll_out = if let FuseResponse::Poll(poll_out) = response {
            poll_out
        } else {
            return error!(EINVAL);
        };
        FdEvents::from_bits(poll_out.revents).ok_or_else(|| errno!(EINVAL))
    }

    fn readdir(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        let configuration = self.connection.get_configuration(current_task)?;
        let use_readdirplus = {
            if configuration.flags.contains(FuseInitFlags::DO_READDIRPLUS) {
                if configuration.flags.contains(FuseInitFlags::READDIRPLUS_AUTO) {
                    sink.offset() == 0
                } else {
                    true
                }
            } else {
                false
            }
        };
        // Request a number of bytes related to the user capacity. If none is given, default to a
        // single page of data.
        let user_capacity = if let Some(base_user_capacity) = sink.user_capacity() {
            if use_readdirplus {
                // Add some amount of capacity for the entries.
                base_user_capacity * 3 / 2
            } else {
                base_user_capacity
            }
        } else {
            *PAGE_SIZE as usize
        };
        let node = self.get_fuse_node(file)?;
        let response = self.connection.execute_operation(
            current_task,
            node,
            FuseOperation::Readdir {
                read_in: uapi::fuse_read_in {
                    fh: self.open_out.fh,
                    offset: sink.offset().try_into().map_err(|_| errno!(EINVAL))?,
                    size: user_capacity.try_into().map_err(|_| errno!(EINVAL))?,
                    read_flags: 0,
                    lock_owner: 0,
                    flags: 0,
                    padding: 0,
                },
                use_readdirplus,
            },
        )?;
        let dirents = if let FuseResponse::Readdir(dirents) = response {
            dirents
        } else {
            return error!(EINVAL);
        };
        let mut sink_result = Ok(());
        for (dirent, name, entry) in dirents {
            if let Some(entry) = entry {
                // nodeid == 0 means the server doesn't want to send entry info.
                if entry.nodeid != 0 {
                    if let Err(e) =
                        node.fs_node_from_entry(file.node(), &name, FuseResponse::Entry(entry))
                    {
                        log_error!("Unable to prefill entry: {e:?}");
                    }
                }
            }
            if sink_result.is_ok() {
                sink_result = sink.add(
                    dirent.ino,
                    dirent.off.try_into().map_err(|_| errno!(EINVAL))?,
                    DirectoryEntryType::from_bits(
                        dirent.type_.try_into().map_err(|_| errno!(EINVAL))?,
                    ),
                    &name,
                );
            }
        }
        sink_result
    }

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        not_implemented_log_once!("ioctl is using default implementation for use.");
        default_ioctl(file, current_task, request, arg)
    }

    fn fcntl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        _arg: u64,
    ) -> Result<SyscallResult, Errno> {
        not_implemented_log_once!("fcntl is using default implementation for use.");
        default_fcntl(cmd)
    }
}

impl FsNodeOps for Arc<FuseNode> {
    fn create_file_ops(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // The node already exists. The creation has been handled before calling this method.
        let flags = flags & !(OpenFlags::CREAT | OpenFlags::EXCL);
        let mode = node.info().mode;
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Open { flags, mode },
        )?;
        let open_out = if let FuseResponse::Open(open_out) = response {
            open_out
        } else {
            return error!(EINVAL);
        };
        Ok(Box::new(FuseFileObject {
            connection: self.connection.clone(),
            open_out,
            kernel: current_task.kernel().clone(),
        }))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Lookup { name: name.to_owned() },
        )?;
        self.fs_node_from_entry(node, name, response)
    }

    fn mknod(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Mknod {
                mknod_in: uapi::fuse_mknod_in {
                    mode: mode.bits(),
                    rdev: dev.bits() as u32,
                    umask: current_task.fs().umask().bits(),
                    padding: 0,
                },
                name: name.to_owned(),
            },
        )?;
        self.fs_node_from_entry(node, name, response)
    }

    fn mkdir(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Mkdir {
                mkdir_in: uapi::fuse_mkdir_in {
                    mode: mode.bits(),
                    umask: current_task.fs().umask().bits(),
                },
                name: name.to_owned(),
            },
        )?;
        self.fs_node_from_entry(node, name, response)
    }

    fn create_symlink(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Symlink { target: target.to_owned(), name: name.to_owned() },
        )?;
        self.fs_node_from_entry(node, name, response)
    }

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        let response =
            self.connection.execute_operation(current_task, self, FuseOperation::Readlink)?;
        let read_out = if let FuseResponse::Read(read_out) = response {
            read_out
        } else {
            return error!(EINVAL);
        };
        Ok(SymlinkTarget::Path(read_out))
    }

    fn link(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let child_node = FuseNode::from_node(child)?;
        self.connection
            .execute_operation(
                current_task,
                self,
                FuseOperation::Link {
                    link_in: uapi::fuse_link_in { oldnodeid: child_node.nodeid },
                    name: name.to_owned(),
                },
            )
            .map(|_| ())
    }

    fn unlink(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        self.connection
            .execute_operation(current_task, self, FuseOperation::Unlink { name: name.to_owned() })
            .map(|_| ())
    }

    fn truncate(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        node.update_info(|info| {
            // Truncate is implemented by updating the attributes of the file.
            let attributes = uapi::fuse_setattr_in {
                size: length,
                valid: uapi::FATTR_SIZE,
                ..Default::default()
            };

            let response = self.connection.execute_operation(
                current_task,
                self,
                FuseOperation::SetAttr(attributes),
            )?;
            let attr = if let FuseResponse::Attr(attr) = response {
                attr
            } else {
                return error!(EINVAL);
            };
            FuseNode::update_node_info(info, attr.attr)?;
            Ok(())
        })
    }

    fn allocate(&self, _node: &FsNode, _offset: u64, _length: u64) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::allocate");
        error!(ENOTSUP)
    }

    fn update_info<'a>(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let response =
            self.connection.execute_operation(current_task, self, FuseOperation::GetAttr)?;
        let attr = if let FuseResponse::Attr(attr) = response {
            attr
        } else {
            return error!(EINVAL);
        };
        let mut info = info.write();
        FuseNode::update_node_info(&mut info, attr.attr)?;
        Ok(RwLockWriteGuard::downgrade(info))
    }

    fn get_xattr(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        max_size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::GetXAttr {
                getxattr_in: uapi::fuse_getxattr_in {
                    size: max_size.try_into().map_err(|_| errno!(EINVAL))?,
                    padding: 0,
                },
                name: name.to_vec(),
            },
        )?;
        if let FuseResponse::GetXAttr(result) = response {
            Ok(result)
        } else {
            error!(EINVAL)
        }
    }

    fn set_xattr(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        value: &FsStr,
        op: XattrOp,
    ) -> Result<(), Errno> {
        self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::SetXAttr {
                setxattr_in: uapi::fuse_setxattr_in {
                    size: value.len().try_into().map_err(|_| errno!(EINVAL))?,
                    flags: op.into_flags(),
                    setxattr_flags: 0,
                    padding: 0,
                },
                name: name.to_owned(),
                value: value.to_owned(),
            },
        )?;
        Ok(())
    }

    fn remove_xattr(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<(), Errno> {
        self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::RemoveXAttr { name: name.to_owned() },
        )?;
        Ok(())
    }

    fn list_xattrs(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        max_size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::ListXAttr(uapi::fuse_getxattr_in {
                size: max_size.try_into().map_err(|_| errno!(EINVAL))?,
                padding: 0,
            }),
        )?;
        if let FuseResponse::GetXAttr(result) = response {
            Ok(result.map(|s| {
                let mut result = s.split(|c| *c == 0).map(|s| s.to_vec()).collect::<Vec<_>>();
                // The returned string ends with a '\0', so the split ends with an empty value that
                // needs to be removed.
                result.pop();
                result
            }))
        } else {
            error!(EINVAL)
        }
    }

    fn forget(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<(), Errno> {
        if self.connection.state.lock().disconnected {
            return Ok(());
        }
        let nlookup = self.state.lock().nlookup;
        if nlookup > 0 {
            self.connection.execute_operation(
                current_task,
                self,
                FuseOperation::Forget(uapi::fuse_forget_in { nlookup }),
            )?;
        };
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FuseConnection {
    /// Mutable state of the connection.
    state: Mutex<FuseMutableState>,
}

impl FuseConnection {
    fn disconnect(&self) {
        self.state.lock().disconnect()
    }

    fn get_configuration(
        &self,
        current_task: &CurrentTask,
    ) -> Result<Arc<FuseConfiguration>, Errno> {
        let mut state = self.state.lock();
        if let Some(configuration) = state.configuration.as_ref() {
            return Ok(configuration.clone());
        }
        loop {
            if state.disconnected {
                return error!(EINTR);
            }
            let waiter = Waiter::new();
            state.waiters.wait_async_value(&waiter, CONFIGURATION_AVAILABLE_EVENT);
            if let Some(configuration) = state.configuration.as_ref() {
                return Ok(configuration.clone());
            }
            MutexGuard::unlocked(&mut state, || waiter.wait(current_task))?;
        }
    }

    /// Execute the given operation on the `node`. If the operation is not asynchronous, this
    /// method will wait on the userspace process for a response. If the operation is interrupted,
    /// an interrupt will be sent to the userspace process and the operation will then block until
    /// the initial operation has a response. This block can only be interrupted by the filesystem
    /// being unmounted.
    fn execute_operation(
        &self,
        task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
    ) -> Result<FuseResponse, Errno> {
        let configuration = match operation {
            FuseOperation::Init => Arc::new(FuseConfiguration::for_init()),
            _ => self.get_configuration(task)?,
        };
        let mut state = self.state.lock();
        if let Some(result) = state.operations_state.get(&operation.opcode()) {
            return result.clone();
        }
        if !operation.has_response() {
            state.queue_operation(task, node, operation, configuration, None)?;
            return Ok(FuseResponse::None);
        }
        let waiter = Waiter::new();
        let is_async = operation.is_async();
        let unique_id =
            state.queue_operation(task, node, operation, configuration.clone(), Some(&waiter))?;
        if is_async {
            return Ok(FuseResponse::None);
        }
        let mut first_loop = true;
        loop {
            if let Some(response) = state.get_response(unique_id) {
                return response;
            }
            match MutexGuard::unlocked(&mut state, || waiter.wait(task)) {
                Ok(()) => {}
                Err(e) if e == EINTR => {
                    // If interrupted by another process, send an interrupt command to the server
                    // the first time, then wait unconditionally.
                    if first_loop {
                        state.interrupt(task, node, unique_id, configuration.clone())?;
                        first_loop = false;
                    }
                }
                Err(e) => {
                    log_error!("Unexpected error: {e:?}");
                    return Err(e);
                }
            }
        }
    }

    fn wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.state.lock().waiters.wait_async_events(waiter, events, handler))
    }

    fn query_events(&self) -> FdEvents {
        self.state.lock().query_events()
    }

    fn read(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        self.state.lock().read(data)
    }

    fn write(&self, data: &mut dyn InputBuffer) -> Result<usize, Errno> {
        self.state.lock().write(data)
    }
}

#[derive(Debug)]
struct FuseConfiguration {
    flags: FuseInitFlags,
}

impl FuseConfiguration {
    // Build a fake configuration, valid only to send the Init message.
    fn for_init() -> Self {
        Self { flags: FuseInitFlags::all() }
    }
}

impl TryFrom<uapi::fuse_init_out> for FuseConfiguration {
    type Error = Errno;
    fn try_from(init_out: uapi::fuse_init_out) -> Result<Self, Errno> {
        let unknown_flags = init_out.flags & !FuseInitFlags::all().bits();
        if unknown_flags != 0 {
            log_warn!("FUSE daemon requested unknown flags in init: {unknown_flags}");
        }
        let flags = FuseInitFlags::from_bits_truncate(init_out.flags);
        Ok(Self { flags })
    }
}

/// A per connection state for operations that can be shortcircuited.
///
/// For a number of Fuse operation, Fuse protocol specifies that if they fail in a specific way,
/// they should not be sent to the server again and must be handled in a predefined way. This
/// map keep track of these operations for a given connection. If this map contains a result for a
/// given opcode, any further attempt to send this opcode to userspace will be answered with the
/// content of the map.
type OperationsState = HashMap<uapi::fuse_opcode, Result<FuseResponse, Errno>>;

#[derive(Debug, Default)]
struct FuseMutableState {
    /// Whether the mount has been disconnected.
    disconnected: bool,

    /// Last unique id used to identify messages between the kernel and user space.
    last_unique_id: u64,

    /// The configuration, negotiated with the client.
    configuration: Option<Arc<FuseConfiguration>>,

    /// In progress operations.
    operations: HashMap<u64, RunningOperation>,

    /// Enqueued messages. These messages have not yet been sent to userspace. There should be
    /// multiple queues, but for now, push every messages to the same queue.
    /// New messages are added at the end of the queues. Read consume from the front of the queue.
    message_queue: VecDeque<FuseKernelMessage>,

    /// Queue to notify of new messages.
    waiters: WaitQueue,

    /// The state of the different operations, to allow short-circuiting the userspace process.
    operations_state: OperationsState,
}

impl FuseMutableState {
    fn set_configuration(&mut self, configuration: FuseConfiguration) {
        debug_assert!(self.configuration.is_none());
        log_trace!("Fuse configuration: {configuration:?}");
        self.configuration = Some(Arc::new(configuration));
        self.waiters.notify_value_event(CONFIGURATION_AVAILABLE_EVENT);
    }

    /// Disconnect the mount. Happens on unmount. Every filesystem operation will fail with EINTR,
    /// and every read/write on the /dev/fuse fd will fail with ENODEV.
    fn disconnect(&mut self) {
        if self.disconnected {
            return;
        }
        self.disconnected = true;
        self.message_queue.clear();
        for operation in &mut self.operations {
            operation.1.response = Some(error!(EINTR));
        }
        self.waiters.notify_all();
    }

    /// Queue the given operation on the internal queue for the userspace daemon to read. If
    /// `waiter` is not None, register `waiter` to be notified when userspace responds to the
    /// operation. This should only be used if the operation expects a response.
    fn queue_operation(
        &mut self,
        task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
        configuration: Arc<FuseConfiguration>,
        waiter: Option<&Waiter>,
    ) -> Result<u64, Errno> {
        debug_assert!(waiter.is_some() == operation.has_response(), "{operation:?}");
        if self.disconnected {
            return error!(EINTR);
        }
        let operation = Arc::new(operation);
        self.last_unique_id += 1;
        let message =
            FuseKernelMessage::new(self.last_unique_id, task, node, operation, configuration)?;
        if let Some(waiter) = waiter {
            self.waiters.wait_async_value(waiter, self.last_unique_id);
        }
        if message.operation.has_response() {
            self.operations.insert(self.last_unique_id, message.operation.clone().into());
        }
        self.message_queue.push_back(message);
        self.waiters.notify_fd_events(FdEvents::POLLIN);
        Ok(self.last_unique_id)
    }

    /// Interrupt the operation with the given unique_id.
    ///
    /// If the operation is still enqueued, this will immediately dequeue the operation and return
    /// with an EINTR error.
    ///
    /// If not, it will send an interrupt operation.
    fn interrupt(
        &mut self,
        task: &CurrentTask,
        node: &FuseNode,
        unique_id: u64,
        configuration: Arc<FuseConfiguration>,
    ) -> Result<(), Errno> {
        debug_assert!(self.operations.contains_key(&unique_id));

        let mut in_queue = false;
        self.message_queue.retain(|m| {
            if m.header.unique == unique_id {
                self.operations.remove(&unique_id);
                in_queue = true;
                false
            } else {
                true
            }
        });
        if in_queue {
            // Nothing to do, the operation has been cancelled before being sent.
            return error!(EINTR);
        }
        self.queue_operation(
            task,
            node,
            FuseOperation::Interrupt { unique_id },
            configuration,
            None,
        )
        .map(|_| ())
    }

    /// Returns the response for the operation with the given identifier. Returns None if the
    /// operation is still in flight.
    fn get_response(&mut self, unique_id: u64) -> Option<Result<FuseResponse, Errno>> {
        match self.operations.entry(unique_id) {
            Entry::Vacant(_) => Some(error!(EINVAL)),
            Entry::Occupied(mut entry) => {
                let result = entry.get_mut().response.take();
                if result.is_some() {
                    entry.remove();
                }
                result
            }
        }
    }

    fn query_events(&self) -> FdEvents {
        let mut events = FdEvents::POLLOUT;
        if self.disconnected || !self.message_queue.is_empty() {
            events |= FdEvents::POLLIN
        };
        if self.disconnected {
            events |= FdEvents::POLLERR;
        }
        events
    }

    fn read(&mut self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        if self.disconnected {
            return error!(ENODEV);
        }
        if let Some(message) = self.message_queue.pop_front() {
            message.serialize(data, &message.configuration)
        } else {
            error!(EAGAIN)
        }
    }

    fn write(&mut self, data: &mut dyn InputBuffer) -> Result<usize, Errno> {
        if self.disconnected {
            return error!(ENODEV);
        }
        let mut header = uapi::fuse_out_header::new_zeroed();
        if data.read(header.as_bytes_mut())? != std::mem::size_of::<uapi::fuse_out_header>() {
            return error!(EINVAL);
        }
        let remainder: usize = (header.len as usize) - std::mem::size_of::<uapi::fuse_out_header>();
        if data.available() < remainder {
            return error!(EINVAL);
        }
        self.waiters.notify_value_event(header.unique);
        let running_operation =
            self.operations.get_mut(&header.unique).ok_or_else(|| errno!(EINVAL))?;
        let operation = running_operation.operation.clone();
        if header.error < 0 {
            log_trace!("Fuse: {operation:?} -> {header:?}");
            let code = i16::try_from(-header.error).unwrap_or(EINVAL.error_code() as i16);
            let errno = errno_from_code!(code);
            running_operation.response =
                Some(operation.handle_error(&mut self.operations_state, errno));
        } else {
            let mut buffer = vec![0u8; remainder];
            if data.read(&mut buffer)? != remainder {
                return error!(EINVAL);
            }
            let response = operation.parse_response(buffer)?;
            log_trace!("Fuse: {operation:?} -> {response:?}");
            if operation.is_async() {
                self.handle_async(&operation, response)?;
            } else {
                running_operation.response = Some(Ok(response));
            }
        }
        if operation.is_async() {
            self.operations.remove(&header.unique);
        }
        Ok(header.len as usize)
    }

    fn handle_async(
        &mut self,
        operation: &FuseOperation,
        response: FuseResponse,
    ) -> Result<(), Errno> {
        debug_assert!(operation.is_async());
        if let FuseOperation::Init = operation {
            if let FuseResponse::Init(init_out) = response {
                self.set_configuration(init_out.try_into()?);
            } else {
                return error!(EINVAL);
            }
        }
        Ok(())
    }
}

/// An operation that is either queued to be send to userspace, or already sent to userspace and
/// waiting for a response.
#[derive(Debug)]
struct RunningOperation {
    operation: Arc<FuseOperation>,
    response: Option<Result<FuseResponse, Errno>>,
}

impl From<Arc<FuseOperation>> for RunningOperation {
    fn from(operation: Arc<FuseOperation>) -> Self {
        Self { operation, response: None }
    }
}

#[derive(Debug)]
struct FuseKernelMessage {
    header: uapi::fuse_in_header,
    operation: Arc<FuseOperation>,
    configuration: Arc<FuseConfiguration>,
}

impl FuseKernelMessage {
    fn new(
        unique: u64,
        task: &CurrentTask,
        node: &FuseNode,
        operation: Arc<FuseOperation>,
        configuration: Arc<FuseConfiguration>,
    ) -> Result<Self, Errno> {
        let creds = task.creds();
        Ok(Self {
            header: uapi::fuse_in_header {
                len: u32::try_from(
                    std::mem::size_of::<uapi::fuse_in_header>() + operation.len(&configuration),
                )
                .map_err(|_| errno!(EINVAL))?,
                opcode: operation.opcode(),
                unique,
                nodeid: node.nodeid,
                uid: creds.uid,
                gid: creds.gid,
                pid: task.get_tid() as u32,
                padding: 0,
            },
            operation,
            configuration,
        })
    }

    fn serialize(
        &self,
        data: &mut dyn OutputBuffer,
        configuration: &FuseConfiguration,
    ) -> Result<usize, Errno> {
        let size = data.write(self.header.as_bytes())?;
        Ok(size + self.operation.serialize(data, configuration)?)
    }
}

bitflags::bitflags! {
    pub struct FuseInitFlags : u32 {
        const BIG_WRITES = uapi::FUSE_BIG_WRITES;
        const DONT_MASK = uapi::FUSE_DONT_MASK;
        const SPLICE_WRITE = uapi::FUSE_SPLICE_WRITE;
        const SPLICE_MOVE = uapi::FUSE_SPLICE_MOVE;
        const SPLICE_READ = uapi::FUSE_SPLICE_READ;
        const DO_READDIRPLUS = uapi::FUSE_DO_READDIRPLUS;
        const READDIRPLUS_AUTO = uapi::FUSE_READDIRPLUS_AUTO;
        const SETXATTR_EXT = uapi::FUSE_SETXATTR_EXT;
    }
}

#[derive(Debug)]
enum FuseOperation {
    Flush(uapi::fuse_open_out),
    Forget(uapi::fuse_forget_in),
    GetAttr,
    Init,
    Interrupt {
        /// Identifier of the operation to interrupt
        unique_id: u64,
    },
    GetXAttr {
        getxattr_in: uapi::fuse_getxattr_in,
        /// Name of the attribute
        name: FsString,
    },
    ListXAttr(uapi::fuse_getxattr_in),
    Lookup {
        /// Name of the entry to lookup
        name: FsString,
    },
    Mkdir {
        mkdir_in: uapi::fuse_mkdir_in,
        /// Name of the entry to create
        name: FsString,
    },
    Mknod {
        mknod_in: uapi::fuse_mknod_in,
        /// Name of the node to create
        name: FsString,
    },
    Link {
        link_in: uapi::fuse_link_in,
        /// Name of the link to create
        name: FsString,
    },
    Open {
        flags: OpenFlags,
        mode: FileMode,
    },
    Poll(uapi::fuse_poll_in),
    Read(uapi::fuse_read_in),
    Readdir {
        read_in: uapi::fuse_read_in,
        /// Whether to use the READDIRPLUS api
        use_readdirplus: bool,
    },
    Readlink,
    Release {
        flags: OpenFlags,
        mode: FileMode,
        open_out: uapi::fuse_open_out,
    },
    RemoveXAttr {
        /// Name of the attribute
        name: FsString,
    },
    Seek(uapi::fuse_lseek_in),
    SetAttr(uapi::fuse_setattr_in),
    SetXAttr {
        setxattr_in: uapi::fuse_setxattr_in,
        /// Name of the attribute
        name: FsString,
        /// Value of the attribute
        value: FsString,
    },
    Statfs,
    Symlink {
        /// Target of the link
        target: FsString,
        /// Name of the link
        name: FsString,
    },
    Unlink {
        /// Name of the file to unlink
        name: FsString,
    },
    Write {
        write_in: uapi::fuse_write_in,
        // Content to write
        content: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
enum FuseResponse {
    Attr(uapi::fuse_attr_out),
    Entry(uapi::fuse_entry_out),
    GetXAttr(ValueOrSize<FsString>),
    Init(uapi::fuse_init_out),
    Open(uapi::fuse_open_out),
    Poll(uapi::fuse_poll_out),
    Read(
        // Content read
        Vec<u8>,
    ),
    Seek(uapi::fuse_lseek_out),
    Readdir(Vec<(uapi::fuse_dirent, FsString, Option<uapi::fuse_entry_out>)>),
    Statfs(uapi::fuse_statfs_out),
    Write(uapi::fuse_write_out),
    None,
}

impl FuseOperation {
    fn serialize(
        &self,
        data: &mut dyn OutputBuffer,
        configuration: &FuseConfiguration,
    ) -> Result<usize, Errno> {
        match self {
            Self::Flush(open_in) => {
                let message =
                    uapi::fuse_flush_in { fh: open_in.fh, unused: 0, padding: 0, lock_owner: 0 };
                data.write_all(message.as_bytes())
            }
            Self::Forget(forget_in) => data.write_all(forget_in.as_bytes()),
            Self::GetAttr | Self::Readlink | Self::Statfs => Ok(0),
            Self::GetXAttr { getxattr_in, name } => {
                let mut len = data.write_all(getxattr_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Init => {
                let message = uapi::fuse_init_in {
                    major: uapi::FUSE_KERNEL_VERSION,
                    minor: uapi::FUSE_KERNEL_MINOR_VERSION,
                    flags: FuseInitFlags::all().bits(),
                    ..Default::default()
                };
                data.write_all(message.as_bytes())
            }
            Self::Interrupt { unique_id } => {
                let message = uapi::fuse_interrupt_in { unique: *unique_id };
                data.write_all(message.as_bytes())
            }
            Self::ListXAttr(getxattr_in) => data.write_all(getxattr_in.as_bytes()),
            Self::Lookup { name } => Self::write_null_terminated(data, name),
            Self::Open { flags, .. } => {
                let message = uapi::fuse_open_in { flags: flags.bits(), open_flags: 0 };
                data.write_all(message.as_bytes())
            }
            Self::Poll(poll_in) => data.write_all(poll_in.as_bytes()),
            Self::Mkdir { mkdir_in, name } => {
                let mut len = data.write_all(mkdir_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Mknod { mknod_in, name } => {
                let mut len = data.write_all(mknod_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Link { link_in, name } => {
                let mut len = data.write_all(link_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Read(read_in) | Self::Readdir { read_in, .. } => {
                data.write_all(read_in.as_bytes())
            }
            Self::Release { open_out, .. } => {
                let message = uapi::fuse_release_in {
                    fh: open_out.fh,
                    flags: 0,
                    release_flags: 0,
                    lock_owner: 0,
                };
                data.write_all(message.as_bytes())
            }
            Self::RemoveXAttr { name } => Self::write_null_terminated(data, name),
            Self::Seek(seek_in) => data.write_all(seek_in.as_bytes()),
            Self::SetAttr(setattr_in) => data.write_all(setattr_in.as_bytes()),
            Self::SetXAttr { setxattr_in, name, value } => {
                let header = if configuration.flags.contains(FuseInitFlags::SETXATTR_EXT) {
                    setxattr_in.as_bytes()
                } else {
                    &setxattr_in.as_bytes()[..8]
                };
                let mut len = data.write_all(header)?;
                len += Self::write_null_terminated(data, name)?;
                len += data.write_all(value.as_bytes())?;
                Ok(len)
            }
            Self::Symlink { target, name } => {
                let mut len = Self::write_null_terminated(data, name)?;
                len += Self::write_null_terminated(data, target)?;
                Ok(len)
            }
            Self::Unlink { name } => Self::write_null_terminated(data, name),
            Self::Write { write_in, content } => {
                let mut len = data.write_all(write_in.as_bytes())?;
                len += data.write_all(content)?;
                Ok(len)
            }
        }
    }

    fn write_null_terminated(
        data: &mut dyn OutputBuffer,
        content: &Vec<u8>,
    ) -> Result<usize, Errno> {
        let mut len = data.write_all(content.as_bytes())?;
        len += data.write_all(&[0])?;
        Ok(len)
    }

    fn opcode(&self) -> u32 {
        match self {
            Self::Flush(_) => uapi::fuse_opcode_FUSE_FLUSH,
            Self::Forget(_) => uapi::fuse_opcode_FUSE_FORGET,
            Self::GetAttr => uapi::fuse_opcode_FUSE_GETATTR,
            Self::GetXAttr { .. } => uapi::fuse_opcode_FUSE_GETXATTR,
            Self::Init => uapi::fuse_opcode_FUSE_INIT,
            Self::Interrupt { .. } => uapi::fuse_opcode_FUSE_INTERRUPT,
            Self::ListXAttr(_) => uapi::fuse_opcode_FUSE_LISTXATTR,
            Self::Lookup { .. } => uapi::fuse_opcode_FUSE_LOOKUP,
            Self::Mkdir { .. } => uapi::fuse_opcode_FUSE_MKDIR,
            Self::Mknod { .. } => uapi::fuse_opcode_FUSE_MKNOD,
            Self::Link { .. } => uapi::fuse_opcode_FUSE_LINK,
            Self::Open { flags, mode } => {
                if mode.is_dir() || flags.contains(OpenFlags::DIRECTORY) {
                    uapi::fuse_opcode_FUSE_OPENDIR
                } else {
                    uapi::fuse_opcode_FUSE_OPEN
                }
            }
            Self::Poll(_) => uapi::fuse_opcode_FUSE_POLL,
            Self::Read(_) => uapi::fuse_opcode_FUSE_READ,
            Self::Readdir { use_readdirplus, .. } => {
                if *use_readdirplus {
                    uapi::fuse_opcode_FUSE_READDIRPLUS
                } else {
                    uapi::fuse_opcode_FUSE_READDIR
                }
            }
            Self::Readlink => uapi::fuse_opcode_FUSE_READLINK,
            Self::Release { flags, mode, .. } => {
                if mode.is_dir() || flags.contains(OpenFlags::DIRECTORY) {
                    uapi::fuse_opcode_FUSE_RELEASEDIR
                } else {
                    uapi::fuse_opcode_FUSE_RELEASE
                }
            }
            Self::RemoveXAttr { .. } => uapi::fuse_opcode_FUSE_REMOVEXATTR,
            Self::Seek(_) => uapi::fuse_opcode_FUSE_LSEEK,
            Self::SetAttr(_) => uapi::fuse_opcode_FUSE_SETATTR,
            Self::SetXAttr { .. } => uapi::fuse_opcode_FUSE_SETXATTR,
            Self::Statfs => uapi::fuse_opcode_FUSE_STATFS,
            Self::Symlink { .. } => uapi::fuse_opcode_FUSE_SYMLINK,
            Self::Unlink { .. } => uapi::fuse_opcode_FUSE_UNLINK,
            Self::Write { .. } => uapi::fuse_opcode_FUSE_WRITE,
        }
    }

    fn len(&self, configuration: &FuseConfiguration) -> usize {
        #[derive(Debug, Default)]
        struct CountingOutputBuffer {
            written: usize,
        }

        impl OutputBuffer for CountingOutputBuffer {
            fn available(&self) -> usize {
                usize::MAX
            }
            fn bytes_written(&self) -> usize {
                self.written
            }
            fn write_each(
                &mut self,
                _callback: &mut OutputBufferCallback<'_>,
            ) -> Result<usize, Errno> {
                panic!("Should not be called.");
            }
            fn write_all(&mut self, buffer: &[u8]) -> Result<usize, Errno> {
                self.written += buffer.len();
                Ok(buffer.len())
            }
        }

        let mut counting_output_buffer = CountingOutputBuffer::default();
        self.serialize(&mut counting_output_buffer, configuration)
            .expect("Serialization should not fail");
        counting_output_buffer.written
    }

    fn has_response(&self) -> bool {
        !matches!(self, Self::Interrupt { .. } | Self::Forget(_))
    }

    fn is_async(&self) -> bool {
        matches!(self, Self::Init)
    }

    fn to_response<T: FromBytes + AsBytes>(buffer: &[u8]) -> T {
        let mut result = T::new_zeroed();
        let length_to_copy = std::cmp::min(buffer.len(), std::mem::size_of::<T>());
        result.as_bytes_mut()[..length_to_copy].copy_from_slice(&buffer[..length_to_copy]);
        result
    }

    fn parse_response(&self, buffer: Vec<u8>) -> Result<FuseResponse, Errno> {
        debug_assert!(self.has_response());
        match self {
            Self::GetAttr | Self::SetAttr(_) => {
                Ok(FuseResponse::Attr(Self::to_response::<uapi::fuse_attr_out>(&buffer)))
            }
            Self::GetXAttr { getxattr_in, .. } | Self::ListXAttr(getxattr_in) => {
                if getxattr_in.size == 0 {
                    if buffer.len() < std::mem::size_of::<uapi::fuse_getxattr_out>() {
                        return error!(EINVAL);
                    }
                    let getxattr_out = Self::to_response::<uapi::fuse_getxattr_out>(&buffer);
                    Ok(FuseResponse::GetXAttr(ValueOrSize::Size(getxattr_out.size as usize)))
                } else {
                    Ok(FuseResponse::GetXAttr(buffer.into()))
                }
            }
            Self::Init => Ok(FuseResponse::Init(Self::to_response::<uapi::fuse_init_out>(&buffer))),
            Self::Lookup { .. }
            | Self::Mkdir { .. }
            | Self::Mknod { .. }
            | Self::Link { .. }
            | Self::Symlink { .. } => {
                Ok(FuseResponse::Entry(Self::to_response::<uapi::fuse_entry_out>(&buffer)))
            }
            Self::Open { .. } => {
                Ok(FuseResponse::Open(Self::to_response::<uapi::fuse_open_out>(&buffer)))
            }
            Self::Poll(_) => {
                Ok(FuseResponse::Poll(Self::to_response::<uapi::fuse_poll_out>(&buffer)))
            }
            Self::Read(_) | Self::Readlink => Ok(FuseResponse::Read(buffer)),
            Self::Readdir { use_readdirplus, .. } => {
                let mut result = vec![];
                let mut slice = &buffer[..];
                while !slice.is_empty() {
                    // If using READDIRPLUS, the data starts with the entry.
                    let entry = if *use_readdirplus {
                        if slice.len() < std::mem::size_of::<uapi::fuse_entry_out>() {
                            return error!(EINVAL);
                        }
                        let entry = Self::to_response::<uapi::fuse_entry_out>(slice);
                        slice = &slice[std::mem::size_of::<uapi::fuse_entry_out>()..];
                        Some(entry)
                    } else {
                        None
                    };
                    // The next item is the dirent.
                    if slice.len() < std::mem::size_of::<uapi::fuse_dirent>() {
                        return error!(EINVAL);
                    }
                    let dirent = Self::to_response::<uapi::fuse_dirent>(slice);
                    // And it ends with the name.
                    slice = &slice[std::mem::size_of::<uapi::fuse_dirent>()..];
                    let namelen = dirent.namelen as usize;
                    if slice.len() < namelen {
                        return error!(EINVAL);
                    }
                    let name: FsString = slice[..namelen].to_owned();
                    result.push((dirent, name, entry));
                    let skipped = round_up_to_increment(namelen, 8)?;
                    if slice.len() < skipped {
                        return error!(EINVAL);
                    }
                    slice = &slice[skipped..];
                }
                Ok(FuseResponse::Readdir(result))
            }
            Self::Flush(_)
            | Self::Release { .. }
            | Self::RemoveXAttr { .. }
            | Self::SetXAttr { .. }
            | Self::Unlink { .. } => Ok(FuseResponse::None),
            Self::Statfs => {
                Ok(FuseResponse::Statfs(Self::to_response::<uapi::fuse_statfs_out>(&buffer)))
            }
            Self::Seek(_) => {
                Ok(FuseResponse::Seek(Self::to_response::<uapi::fuse_lseek_out>(&buffer)))
            }
            Self::Write { .. } => {
                Ok(FuseResponse::Write(Self::to_response::<uapi::fuse_write_out>(&buffer)))
            }
            Self::Interrupt { .. } | Self::Forget(_) => {
                panic!("Response for operation without one");
            }
        }
    }

    /// Handles an error from the userspace daemon.
    ///
    /// Given the `errno` returned by the userspace daemon, returns the response the caller should
    /// see. This can also update the `OperationState` to allow shortcircuit on future requests.
    fn handle_error(
        &self,
        state: &mut OperationsState,
        errno: Errno,
    ) -> Result<FuseResponse, Errno> {
        match self {
            Self::Flush(_) if errno == ENOSYS => {
                state.insert(self.opcode(), Ok(FuseResponse::None));
                Ok(FuseResponse::None)
            }
            Self::Seek(_) if errno == ENOSYS => {
                state.insert(self.opcode(), Err(errno.clone()));
                Err(errno)
            }
            Self::Poll(_) if errno == ENOSYS => {
                let response = FuseResponse::Poll(uapi::fuse_poll_out {
                    revents: (FdEvents::POLLIN | FdEvents::POLLOUT).bits(),
                    padding: 0,
                });
                state.insert(self.opcode(), Ok(response.clone()));
                Ok(response)
            }
            _ => Err(errno),
        }
    }
}
