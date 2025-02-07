// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::fuchsia::remote_volume::RemoteVolume;
use crate::fs::fuchsia::sync_file::{SyncFence, SyncFile, SyncPoint, Timeline};
use crate::fs::fuchsia::RemoteUnixDomainSocket;
use crate::mm::memory::MemoryObject;
use crate::mm::{ProtectionFlags, VMEX_RESOURCE};
use crate::security;
use crate::task::{CurrentTask, EncryptionKeyId, Kernel};
use crate::vfs::buffers::{with_iovec_segments, InputBuffer, OutputBuffer};
use crate::vfs::fsverity::FsVerityState;
use crate::vfs::socket::{Socket, SocketFile, ZxioBackedSocket};
use crate::vfs::{
    default_ioctl, default_seek, fileops_impl_directory, fileops_impl_seekable,
    fs_node_impl_not_dir, fs_node_impl_symlink, fs_node_impl_xattr_delegate, Anon, AppendLockGuard,
    CacheConfig, CacheMode, DirectoryEntryType, DirentSink, FallocMode, FileHandle, FileObject,
    FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle,
    FsNodeInfo, FsNodeOps, FsStr, FsString, SeekTarget, SymlinkTarget, XattrOp, XattrStorage,
    DEFAULT_BYTES_PER_BLOCK,
};
use bstr::{BString, ByteSlice};
use fidl::AsHandleRef;
use fuchsia_runtime::UtcInstant;
use linux_uapi::SYNC_IOC_MAGIC;
use once_cell::sync::OnceCell;
use starnix_logging::{impossible_error, log_warn, trace_duration, CATEGORY_STARNIX_MM};
use starnix_sync::{
    FileOpsCore, Locked, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, Unlocked,
};
use starnix_syscalls::{SyscallArg, SyscallResult};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{
    __kernel_fsid_t, errno, error, from_status_like_fdio, fsverity_descriptor, ino_t, off_t, statfs,
};
use std::mem::MaybeUninit;
use std::sync::Arc;
use syncio::zxio::{
    zxio_get_posix_mode, zxio_node_attr, ZXIO_NODE_PROTOCOL_FILE, ZXIO_NODE_PROTOCOL_SYMLINK,
    ZXIO_OBJECT_TYPE_DATAGRAM_SOCKET, ZXIO_OBJECT_TYPE_DIR, ZXIO_OBJECT_TYPE_FILE,
    ZXIO_OBJECT_TYPE_NONE, ZXIO_OBJECT_TYPE_PACKET_SOCKET, ZXIO_OBJECT_TYPE_RAW_SOCKET,
    ZXIO_OBJECT_TYPE_STREAM_SOCKET, ZXIO_OBJECT_TYPE_SYNCHRONOUS_DATAGRAM_SOCKET,
};
use syncio::{
    zxio_fsverity_descriptor_t, zxio_node_attr_has_t, zxio_node_attributes_t, AllocateMode,
    DirentIterator, SelinuxContextAttr, XattrSetMode, Zxio, ZxioDirent, ZxioOpenOptions,
    ZXIO_ROOT_HASH_LENGTH,
};
use zx::{HandleBased, Status};
use {
    fidl_fuchsia_io as fio, fidl_fuchsia_starnix_binder as fbinder,
    fidl_fuchsia_unknown as funknown,
};

pub fn new_remote_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let kernel = current_task.kernel();
    // TODO(379929394): After soft transition of fstab is complete, we should
    // validate the requested_path is a non-empty, non-root path.
    let requested_path = std::str::from_utf8(&options.source)
        .map_err(|_| errno!(EINVAL, "source path is not utf8"))?;
    let create_flags = fio::PERM_READABLE
        | fio::PERM_WRITABLE
        | fio::Flags::FLAG_MAYBE_CREATE
        | fio::Flags::PROTOCOL_DIRECTORY;
    let (root_proxy, subdir) = kernel.open_ns_dir(requested_path, create_flags)?;

    let subdir = if subdir.is_empty() { ".".to_string() } else { subdir };
    let open_rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
    let mut subdir_options = options;
    subdir_options.source = BString::from(subdir);
    create_remotefs_filesystem(&kernel, &root_proxy, subdir_options, open_rights)
}

/// Create a filesystem to access the content of the fuchsia directory available at `fs_src` inside
/// `pkg`.
pub fn create_remotefs_filesystem(
    kernel: &Arc<Kernel>,
    root: &fio::DirectorySynchronousProxy,
    options: FileSystemOptions,
    rights: fio::Flags,
) -> Result<FileSystemHandle, Errno> {
    let root = syncio::directory_open_directory_async(
        root,
        std::str::from_utf8(&options.source)
            .map_err(|_| errno!(EINVAL, "source path is not utf8"))?,
        rights,
    )
    .map_err(|e| errno!(EIO, format!("Failed to open root: {e}")))?;
    RemoteFs::new_fs(kernel, root.into_channel(), options, rights)
}

pub struct RemoteFs {
    // If true, trust the remote file system's IDs (which requires that the remote file system does
    // not span mounts).  This must be true to properly support hard links.  If this is false, the
    // same node can end up having different IDs as it leaves and reenters the node cache.
    // TODO(https://fxbug.dev/42081972): At the time of writing, package directories do not have
    // unique IDs so this *must* be false in that case.
    use_remote_ids: bool,

    root_proxy: fio::DirectorySynchronousProxy,
}

impl RemoteFs {
    /// Returns a reference to a RemoteFs given a reference to a FileSystem.
    ///
    /// # Panics
    ///
    /// This will panic if `fs`'s ops aren't `RemoteFs`, so this should only be called when this is
    /// known to be the case.
    fn from_fs(fs: &FileSystem) -> &RemoteFs {
        if let Some(remote_vol) = fs.downcast_ops::<RemoteVolume>() {
            remote_vol.remotefs()
        } else {
            fs.downcast_ops::<RemoteFs>().unwrap()
        }
    }
}

const REMOTE_FS_MAGIC: u32 = u32::from_be_bytes(*b"f.io");
const SYNC_IOC_FILE_INFO: u8 = 4;
const SYNC_IOC_MERGE: u8 = 3;

impl FileSystemOps for RemoteFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        let (status, info) = self
            .root_proxy
            .query_filesystem(zx::MonotonicInstant::INFINITE)
            .map_err(|_| errno!(EIO))?;
        // Not all remote filesystems support `QueryFilesystem`, many return ZX_ERR_NOT_SUPPORTED.
        if status == 0 {
            if let Some(info) = info {
                let (total_blocks, free_blocks) = if info.block_size > 0 {
                    (
                        (info.total_bytes / u64::from(info.block_size))
                            .try_into()
                            .unwrap_or(i64::MAX),
                        ((info.total_bytes.saturating_sub(info.used_bytes))
                            / u64::from(info.block_size))
                        .try_into()
                        .unwrap_or(i64::MAX),
                    )
                } else {
                    (0, 0)
                };

                let fsid = __kernel_fsid_t {
                    val: [
                        (info.fs_id & 0xffffffff) as i32,
                        ((info.fs_id >> 32) & 0xffffffff) as i32,
                    ],
                };

                return Ok(statfs {
                    f_type: info.fs_type as i64,
                    f_bsize: info.block_size.into(),
                    f_blocks: total_blocks,
                    f_bfree: free_blocks,
                    f_bavail: free_blocks,
                    f_files: info.total_nodes.try_into().unwrap_or(i64::MAX),
                    f_ffree: (info.total_nodes.saturating_sub(info.used_nodes))
                        .try_into()
                        .unwrap_or(i64::MAX),
                    f_fsid: fsid,
                    f_namelen: info.max_filename_size.try_into().unwrap_or(0),
                    f_frsize: info.block_size.into(),
                    ..statfs::default()
                });
            }
        }
        Ok(default_statfs(REMOTE_FS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        "remotefs".into()
    }

    fn generate_node_ids(&self) -> bool {
        self.use_remote_ids
    }

    fn rename(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        current_task: &CurrentTask,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        // Renames should fail if the src or target directory is encrypted and locked.
        old_parent.fail_if_locked(locked, current_task)?;
        new_parent.fail_if_locked(locked, current_task)?;

        let Some(old_parent) = old_parent.downcast_ops::<RemoteNode>() else {
            return error!(EXDEV);
        };
        let Some(new_parent) = new_parent.downcast_ops::<RemoteNode>() else {
            return error!(EXDEV);
        };
        old_parent
            .zxio
            .rename(get_name_str(old_name)?, &new_parent.zxio, get_name_str(new_name)?)
            .map_err(|status| from_status_like_fdio!(status))
    }
}

impl RemoteFs {
    pub fn new(root: zx::Channel, server_end: zx::Channel) -> Result<RemoteFs, Errno> {
        // See if open3 works.  We assume that if open3 works on the root, it will work for all
        // descendent nodes in this filesystem.  At the time of writing, this is true for Fxfs.
        let root_proxy = fio::DirectorySynchronousProxy::new(root);
        root_proxy
            .open(
                ".",
                fio::Flags::PROTOCOL_DIRECTORY
                    | fio::PERM_READABLE
                    | fio::Flags::PERM_INHERIT_WRITE
                    | fio::Flags::PERM_INHERIT_EXECUTE
                    | fio::Flags::FLAG_SEND_REPRESENTATION,
                &fio::Options {
                    attributes: Some(fio::NodeAttributesQuery::ID),
                    ..Default::default()
                },
                server_end,
            )
            .map_err(|_| errno!(EIO))?;
        // Use remote IDs if the filesystem is Fxfs which we know will give us unique IDs.  Hard
        // links need to resolve to the same underlying FsNode, so we can only support hard links if
        // the remote file system will give us unique IDs.  The IDs are also used as the key in
        // caches, so we can't use remote IDs if the remote filesystem is not guaranteed to provide
        // unique IDs, or if the remote filesystem spans multiple filesystems.
        let (status, info) =
            root_proxy.query_filesystem(zx::MonotonicInstant::INFINITE).map_err(|_| errno!(EIO))?;
        // Be tolerant of errors here; many filesystems return `ZX_ERR_NOT_SUPPORTED`.
        let use_remote_ids = status == 0
            && info
                .map(|i| i.fs_type == fidl_fuchsia_fs::VfsType::Fxfs.into_primitive())
                .unwrap_or(false);
        Ok(RemoteFs { use_remote_ids, root_proxy })
    }

    pub fn new_fs(
        kernel: &Arc<Kernel>,
        root: zx::Channel,
        mut options: FileSystemOptions,
        rights: fio::Flags,
    ) -> Result<FileSystemHandle, Errno> {
        let (client_end, server_end) = zx::Channel::create();
        let remotefs = RemoteFs::new(root, server_end)?;
        let mut attrs = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { id: true, ..Default::default() },
            ..Default::default()
        };
        let (remote_node, node_id) =
            match Zxio::create_with_on_representation(client_end.into(), Some(&mut attrs)) {
                Err(status) => return Err(from_status_like_fdio!(status)),
                Ok(zxio) => (RemoteNode { zxio: Arc::new(zxio), rights }, attrs.id),
            };

        if !rights.contains(fio::PERM_WRITABLE) {
            options.flags |= MountFlags::RDONLY;
        }
        let use_remote_ids = remotefs.use_remote_ids;
        let fs =
            FileSystem::new(kernel, CacheMode::Cached(CacheConfig::default()), remotefs, options)?;
        let mut root_node = FsNode::new_root(remote_node);
        if use_remote_ids {
            root_node.node_id = node_id;
        }
        fs.set_root_node(root_node);
        Ok(fs)
    }

    pub fn use_remote_ids(&self) -> bool {
        self.use_remote_ids
    }
}

pub struct RemoteNode {
    /// The underlying Zircon I/O object for this remote node.
    ///
    /// We delegate to the zxio library for actually doing I/O with remote
    /// objects, including fuchsia.io.Directory and fuchsia.io.File objects.
    /// This structure lets us share code with FDIO and other Fuchsia clients.
    zxio: Arc<syncio::Zxio>,

    /// The fuchsia.io rights for the dir handle. Subdirs will be opened with
    /// the same rights.
    rights: fio::Flags,
}

impl RemoteNode {
    pub fn new(zxio: Arc<syncio::Zxio>, rights: fio::Flags) -> Self {
        Self { zxio, rights }
    }
}

/// Create a file handle from a zx::Handle.
///
/// The handle must be a channel, socket, vmo or debuglog object.  If the handle is a channel, then
/// the channel must implement the `fuchsia.unknown/Queryable` protocol.
///
/// The resulting object will be owned by root and will have a permissions derived from the node's
/// underlying abilities (which is not the same as the the permissions that are set if the object
/// was created using Starnix).  This is fine, since this should mostly be used when interfacing
/// with objects created outside of Starnix.
pub fn new_remote_file(
    current_task: &CurrentTask,
    handle: zx::Handle,
    flags: OpenFlags,
) -> Result<FileHandle, Errno> {
    let (attrs, ops) = remote_file_attrs_and_ops(handle)?;
    let mut mode = get_mode(&attrs);
    if ops.as_any().is::<SocketFile>() {
        // Set the file mode to socket.
        mode = (mode & !FileMode::IFMT) | FileMode::IFSOCK;
    }
    let file_handle = Anon::new_file_extended(current_task, ops, flags, "[fuchsia:remote]", |id| {
        let mut info = FsNodeInfo::new(id, mode, FsCred::root());
        update_info_from_attrs(&mut info, &attrs);
        info
    });
    Ok(file_handle)
}

// Create a FileOps from a zx::Handle.
//
// The handle must satisfy the same requirements as `new_remote_file`.
pub fn new_remote_file_ops(handle: zx::Handle) -> Result<Box<dyn FileOps>, Errno> {
    let (_, ops) = remote_file_attrs_and_ops(handle)?;
    Ok(ops)
}

fn remote_file_attrs_and_ops(
    mut handle: zx::Handle,
) -> Result<(zxio_node_attr, Box<dyn FileOps>), Errno> {
    let handle_type =
        handle.basic_info().map_err(|status| from_status_like_fdio!(status))?.object_type;

    // Check whether the channel implements a Starnix specific protoocol.
    if handle_type == zx::ObjectType::CHANNEL {
        let channel = zx::Channel::from(handle);
        let queryable = funknown::QueryableSynchronousProxy::new(channel);
        if let Ok(name) = queryable.query(zx::MonotonicInstant::INFINITE) {
            if name == fbinder::UNIX_DOMAIN_SOCKET_PROTOCOL_NAME.as_bytes() {
                let socket_ops = RemoteUnixDomainSocket::new(queryable.into_channel())?;
                let socket = Socket::new_with_ops(Box::new(socket_ops))?;
                let file_ops = SocketFile::new(socket);
                let attr = zxio_node_attr {
                    has: zxio_node_attr_has_t { mode: true, ..zxio_node_attr_has_t::default() },
                    mode: 0o777,
                    ..zxio_node_attr::default()
                };
                return Ok((attr, file_ops));
            }
        };
        handle = queryable.into_channel().into_handle();
    };

    // Otherwise, use zxio based objects.
    let zxio = Zxio::create(handle).map_err(|status| from_status_like_fdio!(status))?;
    let attrs = zxio
        .attr_get(zxio_node_attr_has_t {
            protocols: true,
            abilities: true,
            content_size: true,
            storage_size: true,
            link_count: true,
            object_type: true,
            ..Default::default()
        })
        .map_err(|status| from_status_like_fdio!(status))?;
    let ops: Box<dyn FileOps> = match (handle_type, attrs.object_type) {
        (_, ZXIO_OBJECT_TYPE_DIR) => Box::new(RemoteDirectoryObject::new(zxio)),
        (zx::ObjectType::VMO, _)
        | (zx::ObjectType::DEBUGLOG, _)
        | (_, ZXIO_OBJECT_TYPE_FILE)
        | (_, ZXIO_OBJECT_TYPE_NONE) => Box::new(RemoteFileObject::new(zxio)),
        (zx::ObjectType::SOCKET, _)
        | (_, ZXIO_OBJECT_TYPE_SYNCHRONOUS_DATAGRAM_SOCKET)
        | (_, ZXIO_OBJECT_TYPE_DATAGRAM_SOCKET)
        | (_, ZXIO_OBJECT_TYPE_STREAM_SOCKET)
        | (_, ZXIO_OBJECT_TYPE_RAW_SOCKET)
        | (_, ZXIO_OBJECT_TYPE_PACKET_SOCKET) => {
            let socket_ops = ZxioBackedSocket::new_with_zxio(zxio);
            let socket = Socket::new_with_ops(Box::new(socket_ops))?;
            SocketFile::new(socket)
        }
        _ => return error!(ENOTSUP),
    };
    Ok((attrs, ops))
}

pub fn create_fuchsia_pipe(
    current_task: &CurrentTask,
    socket: zx::Socket,
    flags: OpenFlags,
) -> Result<FileHandle, Errno> {
    new_remote_file(current_task, socket.into(), flags)
}

fn fetch_and_refresh_info_impl<'a>(
    zxio: &Arc<syncio::Zxio>,
    info: &'a RwLock<FsNodeInfo>,
) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
    // TODO(https://fxbug.dev/294318193): when Fxfs and Memfs support tracking atime, we should
    // refresh the corresponding value.
    let attrs = zxio
        .attr_get(zxio_node_attr_has_t {
            content_size: true,
            storage_size: true,
            link_count: true,
            modification_time: true,
            change_time: true,
            casefold: true,
            wrapping_key_id: true,
            ..Default::default()
        })
        .map_err(|status| from_status_like_fdio!(status))?;
    let mut info = info.write();
    update_info_from_attrs(&mut info, &attrs);
    Ok(RwLockWriteGuard::downgrade(info))
}

// Update info from attrs if they are set.
pub fn update_info_from_attrs(info: &mut FsNodeInfo, attrs: &zxio_node_attributes_t) {
    // TODO - store these in FsNodeState and convert on fstat
    if attrs.has.content_size {
        info.size = attrs.content_size.try_into().unwrap_or(std::usize::MAX);
    }
    if attrs.has.storage_size {
        info.blocks = usize::try_from(attrs.storage_size).unwrap_or(std::usize::MAX)
            / DEFAULT_BYTES_PER_BLOCK;
    }
    info.blksize = DEFAULT_BYTES_PER_BLOCK;
    if attrs.has.link_count {
        info.link_count = attrs.link_count.try_into().unwrap_or(std::usize::MAX);
    }
    if attrs.has.modification_time {
        info.time_modify =
            UtcInstant::from_nanos(attrs.modification_time.try_into().unwrap_or(i64::MAX));
    }
    if attrs.has.change_time {
        info.time_status_change =
            UtcInstant::from_nanos(attrs.change_time.try_into().unwrap_or(i64::MAX));
    }
    if attrs.has.wrapping_key_id {
        info.wrapping_key_id = Some(attrs.wrapping_key_id);
    }
}

fn get_mode(attrs: &zxio_node_attributes_t) -> FileMode {
    if attrs.protocols & ZXIO_NODE_PROTOCOL_SYMLINK != 0 {
        // We don't set the mode for symbolic links , so we synthesize it instead.
        FileMode::IFLNK | FileMode::ALLOW_ALL
    } else if attrs.has.mode {
        // If the filesystem supports POSIX mode bits, use that directly.
        FileMode::from_bits(attrs.mode)
    } else {
        // The filesystem doesn't support the `mode` attribute, so synthesize it from the node's
        // fuchsia.io protocols/abilities.
        let mode =
            FileMode::from_bits(unsafe { zxio_get_posix_mode(attrs.protocols, attrs.abilities) });
        let user_perms = mode.bits() & 0o700;
        // Make sure the same permissions are granted to user, group, and other.
        mode | FileMode::from_bits((user_perms >> 3) | (user_perms >> 6))
    }
}

fn get_name_str<'a>(name_bytes: &'a FsStr) -> Result<&'a str, Errno> {
    std::str::from_utf8(name_bytes.as_ref()).map_err(|_| {
        log_warn!("bad utf8 in pathname! remote filesystems can't handle this");
        errno!(EINVAL)
    })
}

impl XattrStorage for syncio::Zxio {
    fn get_xattr(&self, name: &FsStr) -> Result<FsString, Errno> {
        Ok(self
            .xattr_get(name)
            .map_err(|status| match status {
                zx::Status::NOT_FOUND => errno!(ENODATA),
                status => from_status_like_fdio!(status),
            })?
            .into())
    }

    fn set_xattr(&self, name: &FsStr, value: &FsStr, op: XattrOp) -> Result<(), Errno> {
        let mode = match op {
            XattrOp::Set => XattrSetMode::Set,
            XattrOp::Create => XattrSetMode::Create,
            XattrOp::Replace => XattrSetMode::Replace,
        };

        self.xattr_set(name, value, mode).map_err(|status| match status {
            zx::Status::NOT_FOUND => errno!(ENODATA),
            status => from_status_like_fdio!(status),
        })
    }

    fn remove_xattr(&self, name: &FsStr) -> Result<(), Errno> {
        self.xattr_remove(name).map_err(|status| match status {
            zx::Status::NOT_FOUND => errno!(ENODATA),
            _ => from_status_like_fdio!(status),
        })
    }

    fn list_xattrs(&self) -> Result<Vec<FsString>, Errno> {
        self.xattr_list()
            .map(|attrs| attrs.into_iter().map(FsString::new).collect::<Vec<_>>())
            .map_err(|status| from_status_like_fdio!(status))
    }
}

impl FsNodeOps for RemoteNode {
    fs_node_impl_xattr_delegate!(self, self.zxio);

    fn create_file_ops(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        {
            let node_info = node.fetch_and_refresh_info(locked, current_task)?;
            if node_info.mode.is_dir() {
                if let Some(wrapping_key_id) = node_info.wrapping_key_id {
                    // Locked encrypted directories cannot be opened with write access.
                    if !current_task
                        .kernel()
                        .crypt_service
                        .contains_key(EncryptionKeyId::from(wrapping_key_id))
                        && flags.can_write()
                    {
                        return error!(ENOKEY);
                    }
                }
                // For directories we need to clone the connection because we rely on the seek offset.
                return Ok(Box::new(RemoteDirectoryObject::new(
                    (*self.zxio).clone().map_err(|status| from_status_like_fdio!(status))?,
                )));
            }
        }

        // Locked encrypted files cannot be opened.
        node.fail_if_locked(locked, current_task)?;

        // fsverity files cannot be opened in write mode, including while building.
        if flags.can_write() {
            node.fsverity.lock().check_writable()?;
        }

        // For files we can clone the `Arc<Zxio>` because we don't rely on any per-connection state
        // (i.e. the file offset).
        Ok(Box::new(RemoteFileObject::new(self.zxio.clone())))
    }

    fn mknod(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        node.fail_if_locked(locked, current_task)?;
        let name = get_name_str(name)?;

        let fs = node.fs();
        let fs_ops = RemoteFs::from_fs(&fs);

        let zxio;
        let mut node_id;
        if !(mode.is_reg() || mode.is_chr() || mode.is_blk() || mode.is_fifo() || mode.is_sock()) {
            return error!(EINVAL, name);
        }
        let mut attrs = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { id: true, ..Default::default() },
            ..Default::default()
        };
        zxio = Arc::new(
            self.zxio
                .open(
                    name,
                    fio::Flags::FLAG_MUST_CREATE
                        | fio::Flags::PROTOCOL_FILE
                        | fio::Flags::PERM_READ
                        | fio::Flags::PERM_WRITE
                        | fio::Flags::PERM_GET_ATTRIBUTES
                        | fio::Flags::PERM_SET_ATTRIBUTES
                        | fio::Flags::PERM_MODIFY,
                    ZxioOpenOptions::new(
                        Some(&mut attrs),
                        Some(zxio_node_attributes_t {
                            mode: mode.bits(),
                            uid: owner.uid,
                            gid: owner.gid,
                            rdev: dev.bits(),
                            has: zxio_node_attr_has_t {
                                mode: true,
                                uid: true,
                                gid: true,
                                rdev: true,
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    ),
                )
                .map_err(|status| from_status_like_fdio!(status, name))?,
        );
        node_id = attrs.id;

        let ops = if mode.is_reg() {
            Box::new(RemoteNode { zxio, rights: self.rights }) as Box<dyn FsNodeOps>
        } else {
            Box::new(RemoteSpecialNode { zxio }) as Box<dyn FsNodeOps>
        };

        if !fs_ops.use_remote_ids {
            node_id = fs.next_node_id();
        }
        let child = fs.create_node_with_id(
            current_task,
            ops,
            node_id,
            FsNodeInfo { rdev: dev, ..FsNodeInfo::new(node_id, mode, owner) },
        );
        Ok(child)
    }

    fn mkdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        node.fail_if_locked(locked, current_task)?;
        let name = get_name_str(name)?;

        let fs = node.fs();
        let fs_ops = RemoteFs::from_fs(&fs);

        let zxio;
        let mut node_id;
        let mut attrs = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { id: true, ..Default::default() },
            ..Default::default()
        };
        zxio = Arc::new(
            self.zxio
                .open(
                    name,
                    fio::Flags::FLAG_MUST_CREATE
                        | fio::Flags::PROTOCOL_DIRECTORY
                        | fio::PERM_READABLE
                        | fio::PERM_WRITABLE,
                    ZxioOpenOptions::new(
                        Some(&mut attrs),
                        Some(zxio_node_attributes_t {
                            mode: mode.bits(),
                            uid: owner.uid,
                            gid: owner.gid,
                            has: zxio_node_attr_has_t {
                                mode: true,
                                uid: true,
                                gid: true,
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    ),
                )
                .map_err(|status| from_status_like_fdio!(status, name))?,
        );
        node_id = attrs.id;

        let ops = RemoteNode { zxio, rights: self.rights };
        if !fs_ops.use_remote_ids {
            node_id = fs.next_node_id();
        }
        let child = fs.create_node_with_id(
            current_task,
            ops,
            node_id,
            FsNodeInfo::new(node_id, mode, owner),
        );
        Ok(child)
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let name = get_name_str(name)?;

        let fs = node.fs();
        let fs_ops = RemoteFs::from_fs(&fs);

        let zxio;
        let mode;
        let node_id;
        let owner;
        let rdev;
        let fsverity_enabled;
        let mut attrs = zxio_node_attributes_t {
            has: zxio_node_attr_has_t {
                protocols: true,
                abilities: true,
                mode: true,
                uid: true,
                gid: true,
                rdev: true,
                id: true,
                fsverity_enabled: true,
                casefold: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut options = ZxioOpenOptions::new(Some(&mut attrs), None);
        let mut selinux_context_buffer =
            MaybeUninit::<[u8; fio::MAX_SELINUX_CONTEXT_ATTRIBUTE_LEN as usize]>::uninit();
        let mut cached_context = security::fs_is_xattr_labeled(node.fs())
            .then(|| SelinuxContextAttr::new(&mut selinux_context_buffer));
        if let Some(buffer) = &mut cached_context {
            options = options.with_selinux_context_read(buffer).unwrap();
        }
        zxio = Arc::new(
            self.zxio
                .open(name, self.rights, options)
                .map_err(|status| from_status_like_fdio!(status, name))?,
        );
        mode = get_mode(&attrs);
        node_id = attrs.id;
        rdev = DeviceType::from_bits(attrs.rdev);
        owner = FsCred { uid: attrs.uid, gid: attrs.gid };
        fsverity_enabled = attrs.fsverity_enabled;
        // fsverity should not be enabled for non-file nodes.
        if fsverity_enabled && (attrs.protocols & ZXIO_NODE_PROTOCOL_FILE == 0) {
            return error!(EINVAL);
        }
        let casefold = attrs.casefold;

        let node = fs.get_or_create_node(
            current_task,
            if fs_ops.use_remote_ids {
                if node_id == fio::INO_UNKNOWN {
                    return error!(ENOTSUP);
                }
                Some(node_id)
            } else {
                None
            },
            |node_id| {
                let ops = if mode.is_lnk() {
                    Box::new(RemoteSymlink { zxio }) as Box<dyn FsNodeOps>
                } else if mode.is_reg() || mode.is_dir() {
                    Box::new(RemoteNode { zxio, rights: self.rights }) as Box<dyn FsNodeOps>
                } else {
                    Box::new(RemoteSpecialNode { zxio }) as Box<dyn FsNodeOps>
                };
                let child = FsNode::new_uncached(
                    current_task,
                    ops,
                    &fs,
                    node_id,
                    FsNodeInfo { rdev, casefold, ..FsNodeInfo::new(node_id, mode, owner) },
                );
                if fsverity_enabled {
                    *child.fsverity.lock() = FsVerityState::FsVerity;
                }
                if let Some(buffer) = cached_context.as_ref().and_then(|buffer| buffer.get()) {
                    // This is valid to fail if we're using mount point labelling or the
                    // provided context string is invalid.
                    let _ = security::fs_node_notify_security_context(
                        current_task,
                        &child,
                        FsStr::new(buffer),
                    );
                }
                Ok(child)
            },
        )?;
        Ok(node)
    }

    fn truncate(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
        node: &FsNode,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        node.fail_if_locked(locked, current_task)?;
        self.zxio.truncate(length).map_err(|status| from_status_like_fdio!(status))
    }

    fn allocate(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
        node: &FsNode,
        current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno> {
        match mode {
            FallocMode::Allocate { keep_size: false } => {
                node.fail_if_locked(locked, current_task)?;
                self.zxio
                    .allocate(offset, length, AllocateMode::empty())
                    .map_err(|status| from_status_like_fdio!(status))?;
                Ok(())
            }
            _ => error!(EINVAL),
        }
    }

    fn fetch_and_refresh_info<'a>(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        fetch_and_refresh_info_impl(&self.zxio, info)
    }

    fn filesystem_manages_timestamps(&self, _node: &FsNode) -> bool {
        true
    }

    fn update_attributes(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _current_task: &CurrentTask,
        info: &FsNodeInfo,
        has: zxio_node_attr_has_t,
    ) -> Result<(), Errno> {
        // Omit updating creation_time. By definition, there shouldn't be a change in creation_time.
        let mut mutable_node_attributes = zxio_node_attributes_t {
            modification_time: info.time_modify.into_nanos() as u64,
            access_time: info.time_access.into_nanos() as u64,
            mode: info.mode.bits(),
            uid: info.uid,
            gid: info.gid,
            rdev: info.rdev.bits(),
            casefold: info.casefold,
            has,
            ..Default::default()
        };
        if let Some(id) = info.wrapping_key_id {
            mutable_node_attributes.wrapping_key_id = id;
        }
        self.zxio
            .attr_set(&mutable_node_attributes)
            .map_err(|status| from_status_like_fdio!(status))
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        // We don't care about the _child argument because 1. unlinking already takes the parent's
        // children lock, so we don't have to worry about conflicts on this path, and 2. the remote
        // filesystem tracks the link counts so we don't need to update them here.
        let name = get_name_str(name)?;
        self.zxio
            .unlink(name, fio::UnlinkFlags::empty())
            .map_err(|status| from_status_like_fdio!(status))
    }

    fn create_symlink(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        // TODO(https://fxbug.dev/360171961): Add symlink support for fscrypt.
        // Fail if user tries to create a symlink inside an encrypted directory.
        {
            let node_info = node.fetch_and_refresh_info(locked, current_task)?;
            if node_info.wrapping_key_id.is_some() {
                return error!(ENOTSUP);
            }
        }

        let name = get_name_str(name)?;
        let zxio = Arc::new(
            self.zxio
                .create_symlink(name, target)
                .map_err(|status| from_status_like_fdio!(status))?,
        );

        let fs = node.fs();
        let fs_ops = RemoteFs::from_fs(&fs);

        let node_id = if fs_ops.use_remote_ids {
            let attrs = zxio
                .attr_get(zxio_node_attr_has_t { id: true, ..Default::default() })
                .map_err(|status| from_status_like_fdio!(status))?;
            attrs.id
        } else {
            fs.next_node_id()
        };
        let symlink = fs.create_node_with_id(
            current_task,
            RemoteSymlink { zxio },
            node_id,
            FsNodeInfo {
                size: target.len(),
                ..FsNodeInfo::new(node_id, FileMode::IFLNK | FileMode::ALLOW_ALL, owner)
            },
        );
        Ok(symlink)
    }

    fn get_attr(&self, has: zxio_node_attr_has_t) -> Result<zxio_node_attributes_t, Errno> {
        self.zxio.attr_get(has).map_err(|status| from_status_like_fdio!(status))
    }

    fn create_tmpfile(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let fs = node.fs();
        let fs_ops = RemoteFs::from_fs(&fs);

        let zxio;
        let mut node_id;
        if !mode.is_reg() {
            return error!(EINVAL);
        }
        let mut attrs = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { id: true, ..Default::default() },
            ..Default::default()
        };
        // `create_tmpfile` is used by O_TMPFILE. Note that
        // <https://man7.org/linux/man-pages/man2/open.2.html> states that if O_EXCL is specified
        // with O_TMPFILE, the temporary file created cannot be linked into the filesystem. Although
        // there exist fuchsia flags `fio::FLAG_TEMPORARY_AS_NOT_LINKABLE`, the starnix vfs already
        // handles this case and makes sure that the created file is not linkable. There is also no
        // current way of passing the open flags to this function.
        zxio = Arc::new(
            self.zxio
                .open(
                    ".",
                    fio::Flags::PROTOCOL_FILE
                        | fio::Flags::FLAG_CREATE_AS_UNNAMED_TEMPORARY
                        | self.rights,
                    ZxioOpenOptions::new(
                        Some(&mut attrs),
                        Some(zxio_node_attributes_t {
                            mode: mode.bits(),
                            uid: owner.uid,
                            gid: owner.gid,
                            has: zxio_node_attr_has_t {
                                mode: true,
                                uid: true,
                                gid: true,
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    ),
                )
                .map_err(|status| from_status_like_fdio!(status))?,
        );
        node_id = attrs.id;

        let ops = Box::new(RemoteNode { zxio, rights: self.rights }) as Box<dyn FsNodeOps>;

        if !fs_ops.use_remote_ids {
            node_id = fs.next_node_id();
        }
        let child = fs.create_node_with_id(
            current_task,
            ops,
            node_id,
            FsNodeInfo::new(node_id, mode, owner),
        );

        Ok(child)
    }

    fn link(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        if !RemoteFs::from_fs(&node.fs()).use_remote_ids {
            return error!(EPERM);
        }
        let name = get_name_str(name)?;
        let link_into = |zxio: &syncio::Zxio| {
            zxio.link_into(&self.zxio, name).map_err(|status| match status {
                zx::Status::BAD_STATE => errno!(EXDEV),
                zx::Status::ACCESS_DENIED => errno!(ENOKEY),
                s => from_status_like_fdio!(s),
            })
        };
        if let Some(child) = child.downcast_ops::<RemoteNode>() {
            link_into(&child.zxio)
        } else if let Some(child) = child.downcast_ops::<RemoteSymlink>() {
            link_into(&child.zxio)
        } else {
            error!(EXDEV)
        }
    }

    fn enable_fsverity(&self, descriptor: &fsverity_descriptor) -> Result<(), Errno> {
        let descr = zxio_fsverity_descriptor_t {
            hash_algorithm: descriptor.hash_algorithm,
            salt_size: descriptor.salt_size,
            salt: descriptor.salt,
        };
        self.zxio.enable_verity(&descr).map_err(|status| from_status_like_fdio!(status))
    }

    fn get_fsverity_descriptor(&self, log_blocksize: u8) -> Result<fsverity_descriptor, Errno> {
        let mut root_hash = [0; ZXIO_ROOT_HASH_LENGTH];
        let attrs = self
            .zxio
            .attr_get_with_root_hash(
                zxio_node_attr_has_t {
                    content_size: true,
                    fsverity_options: true,
                    fsverity_root_hash: true,
                    ..Default::default()
                },
                &mut root_hash,
            )
            .map_err(|status| match status {
                zx::Status::INVALID_ARGS => errno!(ENODATA),
                _ => from_status_like_fdio!(status),
            })?;
        return Ok(fsverity_descriptor {
            version: 1,
            hash_algorithm: attrs.fsverity_options.hash_alg,
            log_blocksize,
            salt_size: attrs.fsverity_options.salt_size as u8,
            __reserved_0x04: 0u32,
            data_size: attrs.content_size,
            root_hash,
            salt: attrs.fsverity_options.salt,
            __reserved: [0u8; 144],
        });
    }
}

struct RemoteSpecialNode {
    zxio: Arc<syncio::Zxio>,
}

impl FsNodeOps for RemoteSpecialNode {
    fs_node_impl_not_dir!();
    fs_node_impl_xattr_delegate!(self, self.zxio);

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

fn zxio_read_write_inner_map_error(status: zx::Status) -> Errno {
    match status {
        // zx::Stream may return invalid args or not found error because of
        // invalid zx_iovec buffer pointers.
        zx::Status::INVALID_ARGS | zx::Status::NOT_FOUND => errno!(EFAULT, ""),
        status => from_status_like_fdio!(status),
    }
}

fn zxio_read_inner(
    data: &mut dyn OutputBuffer,
    unified_read_fn: impl FnOnce(&[syncio::zxio::zx_iovec]) -> Result<usize, zx::Status>,
    vmo_read_fn: impl FnOnce(&mut [u8]) -> Result<usize, zx::Status>,
) -> Result<usize, Errno> {
    let read_bytes = with_iovec_segments(data, |iovecs| {
        unified_read_fn(&iovecs).map_err(zxio_read_write_inner_map_error)
    });

    match read_bytes {
        Some(actual) => {
            let actual = actual?;
            // SAFETY: we successfully read `actual` bytes
            // directly to the user's buffer segments.
            unsafe { data.advance(actual) }?;
            Ok(actual)
        }
        None => {
            // Perform the (slower) operation by using an intermediate buffer.
            let total = data.available();
            let mut bytes = vec![0u8; total];
            let actual =
                vmo_read_fn(&mut bytes).map_err(|status| from_status_like_fdio!(status))?;
            data.write_all(&bytes[0..actual])
        }
    }
}

fn zxio_read_at(zxio: &Zxio, offset: usize, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
    let offset = offset as u64;
    zxio_read_inner(
        data,
        |iovecs| {
            // SAFETY: `zxio_read_inner` maps the returned error to an appropriate
            // `Errno` for userspace to handle. `data` only points to memory that
            // is allowed to be written to (Linux user-mode aspace or a valid
            // Starnix owned buffer).
            unsafe { zxio.readv_at(offset, iovecs) }
        },
        |bytes| zxio.read_at(offset, bytes),
    )
}

fn zxio_write_inner(
    data: &mut dyn InputBuffer,
    unified_write_fn: impl FnOnce(&[syncio::zxio::zx_iovec]) -> Result<usize, zx::Status>,
    vmo_write_fn: impl FnOnce(&[u8]) -> Result<usize, zx::Status>,
) -> Result<usize, Errno> {
    let write_bytes = with_iovec_segments(data, |iovecs| {
        unified_write_fn(&iovecs).map_err(zxio_read_write_inner_map_error)
    });

    match write_bytes {
        Some(actual) => {
            let actual = actual?;
            data.advance(actual)?;
            Ok(actual)
        }
        None => {
            // Perform the (slower) operation by using an intermediate buffer.
            let bytes = data.peek_all()?;
            let actual = vmo_write_fn(&bytes).map_err(|status| from_status_like_fdio!(status))?;
            data.advance(actual)?;
            Ok(actual)
        }
    }
}

fn zxio_write_at(
    zxio: &Zxio,
    _current_task: &CurrentTask,
    offset: usize,
    data: &mut dyn InputBuffer,
) -> Result<usize, Errno> {
    let offset = offset as u64;
    zxio_write_inner(
        data,
        |iovecs| {
            // SAFETY: `zxio_write_inner` maps the returned error to an appropriate
            // `Errno` for userspace to handle.
            unsafe { zxio.writev_at(offset, iovecs) }
        },
        |bytes| zxio.write_at(offset, bytes),
    )
}

/// Helper struct to track the context necessary to iterate over dir entries.
#[derive(Default)]
struct RemoteDirectoryIterator<'a> {
    iterator: Option<DirentIterator<'a>>,

    /// If the last attempt to write to the sink failed, this contains the entry that is pending to
    /// be added. This is also used to synthesize dot-dot.
    pending_entry: Entry,
}

#[derive(Default)]
enum Entry {
    // Indicates no more entries.
    #[default]
    None,

    Some(ZxioDirent),

    // Indicates dot-dot should be synthesized.
    DotDot,
}

impl Entry {
    fn take(&mut self) -> Entry {
        std::mem::replace(self, Entry::None)
    }
}

impl From<Option<ZxioDirent>> for Entry {
    fn from(value: Option<ZxioDirent>) -> Self {
        match value {
            None => Entry::None,
            Some(x) => Entry::Some(x),
        }
    }
}

impl<'a> RemoteDirectoryIterator<'a> {
    fn get_or_init_iterator(&mut self, zxio: &'a Zxio) -> Result<&mut DirentIterator<'a>, Errno> {
        if self.iterator.is_none() {
            let iterator =
                zxio.create_dirent_iterator().map_err(|status| from_status_like_fdio!(status))?;
            self.iterator = Some(iterator);
        }
        if let Some(iterator) = &mut self.iterator {
            return Ok(iterator);
        }

        // Should be an impossible error, because we just created the iterator above.
        error!(EIO)
    }

    /// Returns the next dir entry. If no more entries are found, returns None.  Returns an error if
    /// the iterator fails for other reasons described by the zxio library.
    pub fn next(&mut self, zxio: &'a Zxio) -> Result<Entry, Errno> {
        let mut next = self.pending_entry.take();
        if let Entry::None = next {
            next = self
                .get_or_init_iterator(zxio)?
                .next()
                .transpose()
                .map_err(|status| from_status_like_fdio!(status))?
                .into();
        }
        // We only want to synthesize .. if . exists because the . and .. entries get removed if the
        // directory is unlinked, so if the remote filesystem has removed ., we know to omit the
        // .. entry.
        match &next {
            Entry::Some(ZxioDirent { name, .. }) if name == "." => {
                self.pending_entry = Entry::DotDot;
            }
            _ => {}
        }
        Ok(next)
    }
}

struct RemoteDirectoryObject {
    iterator: Mutex<RemoteDirectoryIterator<'static>>,

    // The underlying Zircon I/O object.  This *must* be dropped after `iterator` above because the
    // iterator has references to this object.  We use some unsafe code below to erase the lifetime
    // (hence the 'static above).
    zxio: Zxio,
}

impl RemoteDirectoryObject {
    pub fn new(zxio: Zxio) -> RemoteDirectoryObject {
        RemoteDirectoryObject { zxio, iterator: Mutex::new(RemoteDirectoryIterator::default()) }
    }

    /// Returns a reference to Zxio with the lifetime erased.
    ///
    /// # Safety
    ///
    /// The caller must uphold the lifetime requirements, which will be the case if this is only
    /// used for the contained iterator (`iterator` is dropped before `zxio`).
    unsafe fn zxio(&self) -> &'static Zxio {
        &*(&self.zxio as *const Zxio)
    }
}

impl FileOps for RemoteDirectoryObject {
    fileops_impl_directory!();

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        let mut iterator = self.iterator.lock();
        let new_offset = default_seek(current_offset, target, |_| error!(EINVAL))?;
        let mut iterator_position = current_offset;

        if new_offset < iterator_position {
            // Our iterator only goes forward, so reset it here.  Note: we *must* rewind it rather
            // than just create a new iterator because the remote end maintains the offset.
            if let Some(iterator) = &mut iterator.iterator {
                iterator.rewind().map_err(|status| from_status_like_fdio!(status))?;
            }
            iterator.pending_entry = Entry::None;
            iterator_position = 0;
        }

        // Advance the iterator to catch up with the offset.
        for i in iterator_position..new_offset {
            // SAFETY: See the comment on the `zxio` function above.  The iterator outlives this
            // function and the zxio object must outlive the iterator.
            match iterator.next(unsafe { self.zxio() }) {
                Ok(Entry::Some(_) | Entry::DotDot) => {}
                Ok(Entry::None) => break, // No more entries.
                Err(_) => {
                    // In order to keep the offset and the iterator in sync, set the new offset
                    // to be as far as we could get.
                    // Note that failing the seek here would also cause the iterator and the
                    // offset to not be in sync, because the iterator has already moved from
                    // where it was.
                    return Ok(i);
                }
            }
        }

        Ok(new_offset)
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        // It is important to acquire the lock to the offset before the context, to avoid a deadlock
        // where seek() tries to modify the context.
        let mut iterator = self.iterator.lock();

        loop {
            // SAFETY: See the comment on the `zxio` function above.  The iterator outlives this
            // function and the zxio object must outlive the iterator.
            let entry = iterator.next(unsafe { self.zxio() })?;
            if let Err(e) = match &entry {
                Entry::Some(entry) => {
                    let inode_num: ino_t = entry.id.ok_or_else(|| errno!(EIO))?;
                    let entry_type = if entry.is_dir() {
                        DirectoryEntryType::DIR
                    } else if entry.is_file() {
                        DirectoryEntryType::REG
                    } else {
                        DirectoryEntryType::UNKNOWN
                    };
                    sink.add(inode_num, sink.offset() + 1, entry_type, entry.name.as_bstr())
                }
                Entry::DotDot => {
                    let inode_num = if let Some(parent) = file.name.parent_within_mount() {
                        parent.node.node_id
                    } else {
                        // For the root .. should have the same inode number as .
                        file.name.entry.node.node_id
                    };
                    sink.add(inode_num, sink.offset() + 1, DirectoryEntryType::DIR, "..".into())
                }
                Entry::None => break,
            } {
                iterator.pending_entry = entry;
                return Err(e);
            }
        }
        Ok(())
    }

    fn sync(&self, _file: &FileObject, _current_task: &CurrentTask) -> Result<(), Errno> {
        self.zxio.sync().map_err(|status| match status {
            Status::NO_RESOURCES | Status::NO_MEMORY | Status::NO_SPACE => errno!(ENOSPC),
            Status::INVALID_ARGS | Status::NOT_FILE => errno!(EINVAL),
            Status::BAD_HANDLE => errno!(EBADFD),
            Status::NOT_SUPPORTED => errno!(ENOTSUP),
            Status::INTERRUPTED_RETRY => errno!(EINTR),
            _ => errno!(EIO),
        })
    }

    fn to_handle(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.zxio
            .clone()
            .and_then(Zxio::release)
            .map(Some)
            .map_err(|status| from_status_like_fdio!(status))
    }
}

pub struct RemoteFileObject {
    /// The underlying Zircon I/O object.  This is shared, so we must take care not to use any
    /// stateful methods on the underlying object (reading and writing is fine).
    zxio: Arc<Zxio>,

    /// Cached read-only VMO handle.
    read_only_memory: OnceCell<Arc<MemoryObject>>,

    /// Cached read/exec VMO handle.
    read_exec_memory: OnceCell<Arc<MemoryObject>>,
}

impl RemoteFileObject {
    fn new(zxio: impl Into<Arc<Zxio>>) -> RemoteFileObject {
        RemoteFileObject {
            zxio: zxio.into(),
            read_only_memory: Default::default(),
            read_exec_memory: Default::default(),
        }
    }

    fn fetch_remote_memory(&self, prot: ProtectionFlags) -> Result<Arc<MemoryObject>, Errno> {
        let without_exec = self
            .zxio
            .vmo_get(prot.to_vmar_flags() - zx::VmarFlags::PERM_EXECUTE)
            .map_err(|status| from_status_like_fdio!(status))?;
        let all_flags = if prot.contains(ProtectionFlags::EXEC) {
            without_exec.replace_as_executable(&VMEX_RESOURCE).map_err(impossible_error)?
        } else {
            without_exec
        };
        Ok(Arc::new(MemoryObject::from(all_flags)))
    }
}

impl FileOps for RemoteFileObject {
    fileops_impl_seekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        zxio_read_at(&self.zxio, offset, data)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        zxio_write_at(&self.zxio, current_task, offset, data)
    }

    fn get_memory(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        trace_duration!(CATEGORY_STARNIX_MM, c"RemoteFileGetVmo");
        let memory_cache = if prot == (ProtectionFlags::READ | ProtectionFlags::EXEC) {
            Some(&self.read_exec_memory)
        } else if prot == ProtectionFlags::READ {
            Some(&self.read_only_memory)
        } else {
            None
        };

        memory_cache
            .map(|c| c.get_or_try_init(|| self.fetch_remote_memory(prot)).cloned())
            .unwrap_or_else(|| self.fetch_remote_memory(prot))
    }

    fn to_handle(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.zxio
            .as_ref()
            .clone()
            .and_then(Zxio::release)
            .map(Some)
            .map_err(|status| from_status_like_fdio!(status))
    }

    fn sync(&self, _file: &FileObject, _current_task: &CurrentTask) -> Result<(), Errno> {
        self.zxio.sync().map_err(|status| match status {
            Status::NO_RESOURCES | Status::NO_MEMORY | Status::NO_SPACE => errno!(ENOSPC),
            Status::INVALID_ARGS | Status::NOT_FILE => errno!(EINVAL),
            Status::BAD_HANDLE => errno!(EBADFD),
            Status::NOT_SUPPORTED => errno!(ENOTSUP),
            Status::INTERRUPTED_RETRY => errno!(EINTR),
            _ => errno!(EIO),
        })
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        // TODO(b/305781995): Change the SyncFence implementation to not rely on VMOs and
        // remove this ioctl. This is temporary solution.
        if (request >> 8) as u8 == SYNC_IOC_MAGIC && (request as u8 == SYNC_IOC_FILE_INFO)
            || (request as u8 == SYNC_IOC_MERGE)
        {
            let mut sync_points: Vec<SyncPoint> = vec![];
            let memory = self.get_memory(
                &mut locked.cast_locked::<FileOpsCore>(),
                file,
                current_task,
                Some(8),
                ProtectionFlags::READ,
            )?;
            let vmo = memory
                .as_vmo()
                .ok_or_else(|| errno!(ENOTSUP))?
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .map_err(impossible_error)?;
            sync_points.push(SyncPoint { timeline: Timeline::Hwc, handle: Arc::new(vmo) });
            let sync_file_name: &[u8; 32] = b"hwc semaphore\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
            let sync_file = SyncFile::new(*sync_file_name, SyncFence { sync_points });
            return sync_file.ioctl(locked, file, current_task, request, arg);
        }

        default_ioctl(file, locked, current_task, request, arg)
    }
}

struct RemoteSymlink {
    zxio: Arc<syncio::Zxio>,
}

impl FsNodeOps for RemoteSymlink {
    fs_node_impl_symlink!();
    fs_node_impl_xattr_delegate!(self, self.zxio);

    fn readlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path(
            self.zxio.read_link().map_err(|status| from_status_like_fdio!(status))?.into(),
        ))
    }

    fn fetch_and_refresh_info<'a>(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        fetch_and_refresh_info_impl(&self.zxio, info)
    }

    fn filesystem_manages_timestamps(&self, _node: &FsNode) -> bool {
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mm::PAGE_SIZE;
    use crate::testing::*;
    use crate::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
    use crate::vfs::socket::SocketMessageFlags;
    use crate::vfs::{EpollFileObject, LookupContext, Namespace, SymlinkMode, TimeUpdateType};
    use assert_matches::assert_matches;
    use fidl::endpoints::Proxy;
    use fuchsia_fs::{directory, file};
    use fxfs_testing::{TestFixture, TestFixtureOptions};
    use starnix_uapi::auth::Credentials;
    use starnix_uapi::errors::EINVAL;
    use starnix_uapi::file_mode::{mode, AccessCheck};
    use starnix_uapi::vfs::{EpollEvent, FdEvents};
    use zx::HandleBased;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    #[::fuchsia::test]
    async fn test_remote_uds() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (s1, s2) = zx::Socket::create_datagram();
        s2.write(&vec![0]).expect("write");
        let file =
            new_remote_file(&current_task, s1.into(), OpenFlags::RDWR).expect("new_remote_file");
        assert!(file.node().is_sock());
        let socket_ops = file.downcast_file::<SocketFile>().unwrap();
        let flags = SocketMessageFlags::CTRUNC
            | SocketMessageFlags::TRUNC
            | SocketMessageFlags::NOSIGNAL
            | SocketMessageFlags::CMSG_CLOEXEC;
        let mut buffer = VecOutputBuffer::new(1024);
        let info = socket_ops
            .recvmsg(&mut locked, &current_task, &file, &mut buffer, flags, None)
            .expect("recvmsg");
        assert!(info.ancillary_data.is_empty());
        assert_eq!(info.message_length, 1);
    }

    #[::fuchsia::test]
    async fn test_tree() -> Result<(), anyhow::Error> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let rights = fio::PERM_READABLE | fio::PERM_EXECUTABLE;
        let (server, client) = zx::Channel::create();
        fdio::open("/pkg", rights, server).expect("failed to open /pkg");
        let fs = RemoteFs::new_fs(
            &kernel,
            client,
            FileSystemOptions { source: b"/pkg".into(), ..Default::default() },
            rights,
        )?;
        let ns = Namespace::new(fs);
        let root = ns.root();
        let mut context = LookupContext::default();
        assert_eq!(
            root.lookup_child(&mut locked, &current_task, &mut context, "nib".into()).err(),
            Some(errno!(ENOENT))
        );
        let mut context = LookupContext::default();
        root.lookup_child(&mut locked, &current_task, &mut context, "lib".into()).unwrap();

        let mut context = LookupContext::default();
        let _test_file = root
            .lookup_child(
                &mut locked,
                &current_task,
                &mut context,
                "data/tests/hello_starnix".into(),
            )?
            .open(&mut locked, &current_task, OpenFlags::RDONLY, AccessCheck::default())?;
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_blocking_io() {
        let (kernel, current_task) = create_kernel_and_task();

        let (client, server) = zx::Socket::create_stream();
        let pipe = create_fuchsia_pipe(&current_task, client, OpenFlags::RDWR).unwrap();

        let bytes = [0u8; 64];
        assert_eq!(bytes.len(), server.write(&bytes).unwrap());

        // Spawn a kthread to get the right lock context.
        let bytes_read = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result_sync(move |locked, current_task| {
                pipe.read(locked, &current_task, &mut VecOutputBuffer::new(64)).unwrap()
            })
            .unwrap();

        assert_eq!(bytes_read, bytes.len());
    }

    #[::fuchsia::test]
    async fn test_poll() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let (client, server) = zx::Socket::create_stream();
        let pipe = create_fuchsia_pipe(&current_task, client, OpenFlags::RDWR)
            .expect("create_fuchsia_pipe");
        let server_zxio = Zxio::create(server.into_handle()).expect("Zxio::create");

        assert_eq!(
            pipe.query_events(&mut locked, &current_task),
            Ok(FdEvents::POLLOUT | FdEvents::POLLWRNORM)
        );

        let epoll_object = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_object.downcast_file::<EpollFileObject>().unwrap();
        let event = EpollEvent::new(FdEvents::POLLIN, 0);
        epoll_file
            .add(&mut locked, &current_task, &pipe, &epoll_object, event)
            .expect("poll_file.add");

        let fds = epoll_file
            .wait(&mut locked, &current_task, 1, zx::MonotonicInstant::ZERO)
            .expect("wait");
        assert!(fds.is_empty());

        assert_eq!(server_zxio.write(&[0]).expect("write"), 1);

        assert_eq!(
            pipe.query_events(&mut locked, &current_task),
            Ok(FdEvents::POLLOUT | FdEvents::POLLWRNORM | FdEvents::POLLIN | FdEvents::POLLRDNORM)
        );
        let fds = epoll_file
            .wait(&mut locked, &current_task, 1, zx::MonotonicInstant::ZERO)
            .expect("wait");
        assert_eq!(fds.len(), 1);

        assert_eq!(
            pipe.read(&mut locked, &current_task, &mut VecOutputBuffer::new(64)).expect("read"),
            1
        );

        assert_eq!(
            pipe.query_events(&mut locked, &current_task),
            Ok(FdEvents::POLLOUT | FdEvents::POLLWRNORM)
        );
        let fds = epoll_file
            .wait(&mut locked, &current_task, 1, zx::MonotonicInstant::ZERO)
            .expect("wait");
        assert!(fds.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_new_remote_directory() {
        let (_kernel, current_task, _) = create_kernel_task_and_unlocked();
        let pkg_channel: zx::Channel =
            directory::open_in_namespace("/pkg", fio::PERM_READABLE | fio::PERM_EXECUTABLE)
                .expect("failed to open /pkg")
                .into_channel()
                .expect("into_channel")
                .into();

        let fd = new_remote_file(&current_task, pkg_channel.into(), OpenFlags::RDWR)
            .expect("new_remote_file");
        assert!(fd.node().is_dir());
        assert!(fd.to_handle(&current_task).expect("to_handle").is_some());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_new_remote_file() {
        let (_kernel, current_task, _) = create_kernel_task_and_unlocked();
        let content_channel: zx::Channel =
            file::open_in_namespace("/pkg/meta/contents", fio::PERM_READABLE)
                .expect("failed to open /pkg/meta/contents")
                .into_channel()
                .expect("into_channel")
                .into();

        let fd = new_remote_file(&current_task, content_channel.into(), OpenFlags::RDONLY)
            .expect("new_remote_file");
        assert!(!fd.node().is_dir());
        assert!(fd.to_handle(&current_task).expect("to_handle").is_some());
    }

    #[::fuchsia::test]
    async fn test_new_remote_vmo() {
        let (_kernel, current_task, _) = create_kernel_task_and_unlocked();
        let vmo = zx::Vmo::create(*PAGE_SIZE).expect("Vmo::create");
        let fd =
            new_remote_file(&current_task, vmo.into(), OpenFlags::RDWR).expect("new_remote_file");
        assert!(!fd.node().is_dir());
        assert!(fd.to_handle(&current_task).expect("to_handle").is_some());
    }

    #[::fuchsia::test(threads = 2)]
    async fn test_symlink() {
        let fixture = TestFixture::new().await;

        const LINK_PATH: &'static str = "symlink";
        const LINK_TARGET: &'static str = "私は「UTF8」です";
        // We expect the reported size of the symlink to be the length of the target, in bytes,
        // *without* a null terminator. Most Linux systems assume UTF-8 encoding.
        const LINK_SIZE: usize = 22;
        assert_eq!(LINK_SIZE, LINK_TARGET.len());

        {
            let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
            let (server, client) = zx::Channel::create();
            fixture.root().clone(server.into()).expect("clone failed");
            let fs = RemoteFs::new_fs(
                &kernel,
                client,
                FileSystemOptions { source: b"/".into(), ..Default::default() },
                fio::PERM_READABLE | fio::PERM_WRITABLE,
            )
            .expect("new_fs failed");
            let ns = Namespace::new(fs);
            let root = ns.root();
            let symlink_node = root
                .create_symlink(&mut locked, &current_task, LINK_PATH.into(), LINK_TARGET.into())
                .expect("symlink failed");
            assert_matches!(&*symlink_node.entry.node.info(), FsNodeInfo { size: LINK_SIZE, .. });

            let mut context = LookupContext::new(SymlinkMode::NoFollow);
            let child = root
                .lookup_child(&mut locked, &current_task, &mut context, "symlink".into())
                .expect("lookup_child failed");

            match child.readlink(&mut locked, &current_task).expect("readlink failed") {
                SymlinkTarget::Path(path) => assert_eq!(path, LINK_TARGET),
                SymlinkTarget::Node(_) => panic!("readlink returned SymlinkTarget::Node"),
            }
            // Ensure the size stat reports matches what is expected.
            let stat_result =
                child.entry.node.stat(&mut locked, &current_task).expect("stat failed");
            assert_eq!(stat_result.st_size as usize, LINK_SIZE);
        }

        // Simulate a second run to ensure the symlink was persisted correctly.
        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;

        {
            let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
            let (server, client) = zx::Channel::create();
            fixture.root().clone(server.into()).expect("clone failed after remount");
            let fs = RemoteFs::new_fs(
                &kernel,
                client,
                FileSystemOptions { source: b"/".into(), ..Default::default() },
                fio::PERM_READABLE | fio::PERM_WRITABLE,
            )
            .expect("new_fs failed after remount");
            let ns = Namespace::new(fs);
            let root = ns.root();
            let mut context = LookupContext::new(SymlinkMode::NoFollow);
            let child = root
                .lookup_child(&mut locked, &current_task, &mut context, "symlink".into())
                .expect("lookup_child failed after remount");

            match child.readlink(&mut locked, &current_task).expect("readlink failed after remount")
            {
                SymlinkTarget::Path(path) => assert_eq!(path, LINK_TARGET),
                SymlinkTarget::Node(_) => {
                    panic!("readlink returned SymlinkTarget::Node after remount")
                }
            }
            // Ensure the size stat reports matches what is expected.
            let stat_result = child
                .entry
                .node
                .stat(&mut locked, &current_task)
                .expect("stat failed after remount");
            assert_eq!(stat_result.st_size as usize, LINK_SIZE);
        }

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_mode_uid_gid_and_dev_persists() {
        const FILE_MODE: FileMode = mode!(IFREG, 0o467);
        const DIR_MODE: FileMode = mode!(IFDIR, 0o647);
        const BLK_MODE: FileMode = mode!(IFBLK, 0o746);

        let fixture = TestFixture::new().await;

        // Simulate a first run of starnix.
        {
            let (server, client) = zx::Channel::create();
            fixture.root().clone(server.into()).expect("clone failed");

            let (kernel, _init_task) = create_kernel_and_task();
            kernel
                .kthreads
                .spawner()
                .spawn_and_get_result({
                    let kernel = Arc::clone(&kernel);
                    move |locked, current_task| {
                        current_task.set_creds(Credentials {
                            euid: 1,
                            fsuid: 1,
                            egid: 2,
                            fsgid: 2,
                            ..current_task.creds()
                        });
                        let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                        let fs = RemoteFs::new_fs(
                            &kernel,
                            client,
                            FileSystemOptions { source: b"/".into(), ..Default::default() },
                            rights,
                        )
                        .expect("new_fs failed");
                        let ns = Namespace::new(fs);
                        current_task.fs().set_umask(FileMode::from_bits(0));
                        ns.root()
                            .create_node(
                                locked,
                                &current_task,
                                "file".into(),
                                FILE_MODE,
                                DeviceType::NONE,
                            )
                            .expect("create_node failed");
                        ns.root()
                            .create_node(
                                locked,
                                &current_task,
                                "dir".into(),
                                DIR_MODE,
                                DeviceType::NONE,
                            )
                            .expect("create_node failed");
                        ns.root()
                            .create_node(
                                locked,
                                &current_task,
                                "dev".into(),
                                BLK_MODE,
                                DeviceType::RANDOM,
                            )
                            .expect("create_node failed");
                    }
                })
                .await
                .expect("spawn");
        }

        // Simulate a second run.
        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;

        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");
                    assert_matches!(
                        &*child.entry.node.info(),
                        FsNodeInfo { mode: FILE_MODE, uid: 1, gid: 2, rdev: DeviceType::NONE, .. }
                    );
                    let child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "dir".into())
                        .expect("lookup_child failed");
                    assert_matches!(
                        &*child.entry.node.info(),
                        FsNodeInfo { mode: DIR_MODE, uid: 1, gid: 2, rdev: DeviceType::NONE, .. }
                    );
                    let child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "dev".into())
                        .expect("lookup_child failed");
                    assert_matches!(
                        &*child.entry.node.info(),
                        FsNodeInfo { mode: BLK_MODE, uid: 1, gid: 2, rdev: DeviceType::RANDOM, .. }
                    );
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_dot_dot_inode_numbers() {
        let fixture = TestFixture::new().await;

        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFDIR.bits() | 0o777);

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let sub_dir1 = ns
                        .root()
                        .create_node(locked, &current_task, "dir".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");
                    let sub_dir2 = sub_dir1
                        .create_node(locked, &current_task, "dir".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");

                    let dir_handle = ns
                        .root()
                        .entry
                        .open_anonymous(locked, &current_task, OpenFlags::RDONLY)
                        .expect("open failed");

                    #[derive(Default)]
                    struct Sink {
                        offset: off_t,
                        dot_dot_inode_num: u64,
                    }
                    impl DirentSink for Sink {
                        fn add(
                            &mut self,
                            inode_num: ino_t,
                            offset: off_t,
                            entry_type: DirectoryEntryType,
                            name: &FsStr,
                        ) -> Result<(), Errno> {
                            if name == ".." {
                                self.dot_dot_inode_num = inode_num;
                                assert_eq!(entry_type, DirectoryEntryType::DIR);
                            }
                            self.offset = offset;
                            Ok(())
                        }
                        fn offset(&self) -> off_t {
                            self.offset
                        }
                    }
                    let mut sink = Sink::default();
                    dir_handle.readdir(locked, &current_task, &mut sink).expect("readdir failed");

                    // inode_num for .. for the root should be the same as root.
                    assert_eq!(sink.dot_dot_inode_num, ns.root().entry.node.node_id);

                    let dir_handle = sub_dir1
                        .entry
                        .open_anonymous(locked, &current_task, OpenFlags::RDONLY)
                        .expect("open failed");
                    let mut sink = Sink::default();
                    dir_handle.readdir(locked, &current_task, &mut sink).expect("readdir failed");

                    // inode_num for .. for the first sub directory should be the same as root.
                    assert_eq!(sink.dot_dot_inode_num, ns.root().entry.node.node_id);

                    let dir_handle = sub_dir2
                        .entry
                        .open_anonymous(locked, &current_task, OpenFlags::RDONLY)
                        .expect("open failed");
                    let mut sink = Sink::default();
                    dir_handle.readdir(locked, &current_task, &mut sink).expect("readdir failed");

                    // inode_num for .. for the second sub directory should be the first sub directory.
                    assert_eq!(sink.dot_dot_inode_num, sub_dir1.entry.node.node_id);
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_remote_special_node() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");
        const FIFO_MODE: FileMode = FileMode::from_bits(FileMode::IFIFO.bits() | 0o777);
        const REG_MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits());
        let (kernel, _init_task) = create_kernel_and_task();

        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let root = ns.root();

                    // Create RemoteSpecialNode (e.g. FIFO)
                    root.create_node(
                        locked,
                        &current_task,
                        "fifo".into(),
                        FIFO_MODE,
                        DeviceType::NONE,
                    )
                    .expect("create_node failed");
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let fifo_node = root
                        .lookup_child(locked, &current_task, &mut context, "fifo".into())
                        .expect("lookup_child failed");

                    // Test that we get expected behaviour for RemoteSpecialNode operation, e.g.
                    // test that truncate should return EINVAL
                    match fifo_node.truncate(locked, &current_task, 0) {
                        Ok(_) => {
                            panic!("truncate passed for special node")
                        }
                        Err(errno) if errno == EINVAL => {}
                        Err(e) => {
                            panic!("truncate failed with error {:?}", e)
                        }
                    };

                    // Create regular RemoteNode
                    root.create_node(
                        locked,
                        &current_task,
                        "file".into(),
                        REG_MODE,
                        DeviceType::NONE,
                    )
                    .expect("create_node failed");
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let reg_node = root
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");

                    // We should be able to perform truncate on regular files
                    reg_node.truncate(locked, &current_task, 0).expect("truncate failed");
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_hard_link() {
        let fixture = TestFixture::new().await;

        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let node = ns
                        .root()
                        .create_node(
                            locked,
                            &current_task,
                            "file1".into(),
                            mode!(IFREG, 0o666),
                            DeviceType::NONE,
                        )
                        .expect("create_node failed");
                    ns.root()
                        .entry
                        .node
                        .link(
                            locked,
                            &current_task,
                            &ns.root().mount,
                            "file2".into(),
                            &node.entry.node,
                        )
                        .expect("link failed");
                }
            })
            .await
            .expect("spawn");

        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;

        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let child1 = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "file1".into())
                        .expect("lookup_child failed");
                    let child2 = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "file2".into())
                        .expect("lookup_child failed");
                    assert!(Arc::ptr_eq(&child1.entry.node, &child2.entry.node));
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_lookup_on_fsverity_enabled_file() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits() | 0o467);

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let file = ns
                        .root()
                        .create_node(locked, &current_task, "file".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");
                    // Enable verity on the file.
                    let desc = fsverity_descriptor {
                        version: 1,
                        hash_algorithm: 1,
                        salt_size: 32,
                        log_blocksize: 12,
                        ..Default::default()
                    };
                    file.entry.node.ops().enable_fsverity(&desc).expect("enable fsverity failed");
                }
            })
            .await
            .expect("spawn");

        // Tear down the kernel and open the file again. The file should no longer be cached.
        // Test that lookup works as expected for an fsverity-enabled file.
        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let _child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_update_attributes_persists() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits() | 0o467);

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let file = ns
                        .root()
                        .create_node(locked, &current_task, "file".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");
                    // Change the mode, this change should persist
                    file.entry
                        .node
                        .chmod(locked, &current_task, &file.mount, MODE | FileMode::ALLOW_ALL)
                        .expect("chmod failed");
                }
            })
            .await
            .expect("spawn");

        // Tear down the kernel and open the file again. Check that changes persisted.
        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");
                    assert_eq!(child.entry.node.info().mode, MODE | FileMode::ALLOW_ALL);
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_statfs() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task, mut locked) = create_kernel_task_and_unlocked();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |_, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");

                    let statfs = fs.statfs(&mut locked, &current_task).expect("statfs failed");
                    assert!(statfs.f_type != 0);
                    assert!(statfs.f_bsize > 0);
                    assert!(statfs.f_blocks > 0);
                    assert!(statfs.f_bfree > 0 && statfs.f_bfree <= statfs.f_blocks);
                    assert!(statfs.f_files > 0);
                    assert!(statfs.f_ffree > 0 && statfs.f_ffree <= statfs.f_files);
                    assert!(statfs.f_fsid.val[0] != 0 || statfs.f_fsid.val[1] != 0);
                    assert!(statfs.f_namelen > 0);
                    assert!(statfs.f_frsize > 0);
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_allocate() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let root = ns.root();

                    const REG_MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits());
                    root.create_node(
                        locked,
                        &current_task,
                        "file".into(),
                        REG_MODE,
                        DeviceType::NONE,
                    )
                    .expect("create_node failed");
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let reg_node = root
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");

                    reg_node
                        .entry
                        .node
                        .fallocate(
                            locked,
                            &current_task,
                            FallocMode::Allocate { keep_size: false },
                            0,
                            20,
                        )
                        .expect("truncate failed");
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_allocate_overflow() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let root = ns.root();

                    const REG_MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits());
                    root.create_node(
                        locked,
                        &current_task,
                        "file".into(),
                        REG_MODE,
                        DeviceType::NONE,
                    )
                    .expect("create_node failed");
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let reg_node = root
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");

                    reg_node
                        .entry
                        .node
                        .fallocate(
                            locked,
                            &current_task,
                            FallocMode::Allocate { keep_size: false },
                            1,
                            u64::MAX,
                        )
                        .expect_err("truncate unexpectedly passed");
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_time_modify_persists() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits() | 0o467);

        let (kernel, _init_task) = create_kernel_and_task();
        let last_modified = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns: Arc<Namespace> = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let child = ns
                        .root()
                        .create_node(locked, &current_task, "file".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");
                    // Write to file (this should update mtime (time_modify))
                    let file = child
                        .open(locked, &current_task, OpenFlags::RDWR, AccessCheck::default())
                        .expect("open failed");
                    // Call `fetch_and_refresh_info(..)` to refresh `time_modify` with the time managed by the
                    // underlying filesystem
                    let time_before_write = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .time_modify;
                    let write_bytes: [u8; 5] = [1, 2, 3, 4, 5];
                    let written = file
                        .write(locked, &current_task, &mut VecInputBuffer::new(&write_bytes))
                        .expect("write failed");
                    assert_eq!(written, write_bytes.len());
                    let last_modified = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .time_modify;
                    assert!(last_modified > time_before_write);
                    last_modified
                }
            })
            .await
            .expect("spawn");

        // Tear down the kernel and open the file again. Check that modification time is when we
        // last modified the contents of the file
        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");
        let (kernel, _init_task) = create_kernel_and_task();
        let refreshed_modified_time = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "file".into())
                        .expect("lookup_child failed");
                    let last_modified = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .time_modify;
                    last_modified
                }
            })
            .await
            .expect("spawn");
        assert_eq!(last_modified, refreshed_modified_time);

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_update_atime_mtime() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits() | 0o467);

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns: Arc<Namespace> = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let child = ns
                        .root()
                        .create_node(locked, &current_task, "file".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");

                    let info_original = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .clone();

                    child
                        .entry
                        .node
                        .update_atime_mtime(
                            locked,
                            &current_task,
                            &child.mount,
                            TimeUpdateType::Time(UtcInstant::from_nanos(30)),
                            TimeUpdateType::Omit,
                        )
                        .expect("update_atime_mtime failed");
                    let info_after_update = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .clone();
                    assert_eq!(info_after_update.time_modify, info_original.time_modify);
                    assert_eq!(info_after_update.time_access, UtcInstant::from_nanos(30));

                    child
                        .entry
                        .node
                        .update_atime_mtime(
                            locked,
                            &current_task,
                            &child.mount,
                            TimeUpdateType::Omit,
                            TimeUpdateType::Time(UtcInstant::from_nanos(50)),
                        )
                        .expect("update_atime_mtime failed");
                    let info_after_update2 = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .clone();
                    assert_eq!(info_after_update2.time_modify, UtcInstant::from_nanos(50));
                    assert_eq!(info_after_update2.time_access, UtcInstant::from_nanos(30));
                }
            })
            .await
            .expect("spawn");
        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_write_updates_mtime_ctime() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits() | 0o467);

        let (kernel, _init_task) = create_kernel_and_task();
        kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns: Arc<Namespace> = Namespace::new(fs);
                    current_task.fs().set_umask(FileMode::from_bits(0));
                    let child = ns
                        .root()
                        .create_node(locked, &current_task, "file".into(), MODE, DeviceType::NONE)
                        .expect("create_node failed");
                    let file = child
                        .open(locked, &current_task, OpenFlags::RDWR, AccessCheck::default())
                        .expect("open failed");
                    // Call `fetch_and_refresh_info(..)` to refresh ctime and mtime with the time managed by the
                    // underlying filesystem
                    let (ctime_before_write, mtime_before_write) = {
                        let info = child
                            .entry
                            .node
                            .fetch_and_refresh_info(locked, &current_task)
                            .expect("fetch_and_refresh_info failed");
                        (info.time_status_change, info.time_modify)
                    };

                    // Writing to a file should update ctime and mtime
                    let write_bytes: [u8; 5] = [1, 2, 3, 4, 5];
                    let written = file
                        .write(locked, &current_task, &mut VecInputBuffer::new(&write_bytes))
                        .expect("write failed");
                    assert_eq!(written, write_bytes.len());

                    // As Fxfs, the underlying filesystem in this test, can manage file timestamps,
                    // we should not see an update in mtime and ctime without first refreshing the node with
                    // the metadata from Fxfs.
                    let (ctime_after_write_no_refresh, mtime_after_write_no_refresh) = {
                        let info = child.entry.node.info();
                        (info.time_status_change, info.time_modify)
                    };
                    assert_eq!(ctime_after_write_no_refresh, ctime_before_write);
                    assert_eq!(mtime_after_write_no_refresh, mtime_before_write);

                    // Refresh information, we should see `info` with mtime and ctime from the remote
                    // filesystem (assume this is true if the new timestamp values are greater than the ones
                    // without the refresh).
                    let (ctime_after_write_refresh, mtime_after_write_refresh) = {
                        let info = child
                            .entry
                            .node
                            .fetch_and_refresh_info(locked, &current_task)
                            .expect("fetch_and_refresh_info failed");
                        (info.time_status_change, info.time_modify)
                    };
                    assert_eq!(ctime_after_write_refresh, mtime_after_write_refresh);
                    assert!(ctime_after_write_refresh > ctime_after_write_no_refresh);
                }
            })
            .await
            .expect("spawn");

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_casefold_persists() {
        let fixture = TestFixture::new().await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");

        let (kernel, _init_task) = create_kernel_and_task();
        let _ = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns: Arc<Namespace> = Namespace::new(fs);
                    let child = ns
                        .root()
                        .create_node(
                            locked,
                            &current_task,
                            "dir".into(),
                            FileMode::ALLOW_ALL.with_type(FileMode::IFDIR),
                            DeviceType::NONE,
                        )
                        .expect("create_node failed");
                    child
                        .entry
                        .node
                        .update_attributes(locked, &current_task, |info| {
                            info.casefold = true;
                            Ok(())
                        })
                        .expect("enable casefold")
                }
            })
            .await
            .expect("spawn");

        // Tear down the kernel and open the dir again. Check that casefold is preserved.
        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;
        let (server, client) = zx::Channel::create();
        fixture.root().clone(server.into()).expect("clone failed");
        let (kernel, _init_task) = create_kernel_and_task();
        let casefold = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result({
                let kernel = Arc::clone(&kernel);
                move |locked, current_task| {
                    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
                    let fs = RemoteFs::new_fs(
                        &kernel,
                        client,
                        FileSystemOptions { source: b"/".into(), ..Default::default() },
                        rights,
                    )
                    .expect("new_fs failed");
                    let ns = Namespace::new(fs);
                    let mut context = LookupContext::new(SymlinkMode::NoFollow);
                    let child = ns
                        .root()
                        .lookup_child(locked, &current_task, &mut context, "dir".into())
                        .expect("lookup_child failed");
                    let casefold = child
                        .entry
                        .node
                        .fetch_and_refresh_info(locked, &current_task)
                        .expect("fetch_and_refresh_info failed")
                        .casefold;
                    casefold
                }
            })
            .await
            .expect("spawn");
        assert!(casefold);

        fixture.close().await;
    }
}
