// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::AsHandleRef;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use std::sync::Arc;
use syncio::{
    zxio,
    zxio::{zxio_get_posix_mode, ZXIO_NODE_PROTOCOL_SYMLINK},
    zxio_node_attr_has_t, zxio_node_attributes_t, DirentIterator, XattrSetMode, Zxio, ZxioDirent,
    ZxioSignals,
};

use crate::{
    auth::FsCred,
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
    logging::*,
    mm::ProtectionFlags,
    task::*,
    types::*,
    vmex_resource::VMEX_RESOURCE,
};

// Returns true if this remote filesystem supports open2. This only works if the specified node is
// from the remote filesystem.
fn supports_open2(node: &FsNode) -> bool {
    node.fs().downcast_ops::<RemoteFs>().unwrap().supports_open2
}

pub struct RemoteFs {
    supports_open2: bool,
}

impl FileSystemOps for RemoteFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        const REMOTE_FS_MAGIC: u32 = u32::from_be_bytes(*b"f.io");
        Ok(statfs::default(REMOTE_FS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        b"remote"
    }

    fn generate_node_ids(&self) -> bool {
        true
    }

    fn rename(
        &self,
        _fs: &FileSystem,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
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
    pub fn new_fs(
        kernel: &Arc<Kernel>,
        root: zx::Channel,
        source: &str,
        rights: fio::OpenFlags,
    ) -> Result<FileSystemHandle, Errno> {
        // See if open2 works.  We assume that if open2 works on the root, it will work for all
        // descendent nodes in this filesystem.  At the time of writing, this is true for Fxfs.
        let (client_end, server_end) = zx::Channel::create();
        let root_proxy = fio::DirectorySynchronousProxy::new(root);
        root_proxy
            .open2(
                ".",
                &fio::ConnectionProtocols::Node(fio::NodeOptions {
                    flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                    attributes: Some(fio::NodeAttributesQuery::ID),
                    ..Default::default()
                }),
                server_end,
            )
            .map_err(|_| errno!(EIO))?;
        let mut attrs = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { id: true, ..Default::default() },
            ..Default::default()
        };
        let (remote_node, node_id, supports_open2) =
            match Zxio::create_with_on_representation(client_end.into(), Some(&mut attrs)) {
                Err(zx::Status::NOT_SUPPORTED) => {
                    // Fall back to open.
                    let remote_node = RemoteNode {
                        zxio: Arc::new(
                            Zxio::create(root_proxy.into_channel().into())
                                .map_err(|status| from_status_like_fdio!(status))?,
                        ),
                        rights,
                    };
                    let attrs = remote_node.zxio.attr_get().map_err(|_| errno!(EIO))?;
                    (remote_node, attrs.id, false)
                }
                Err(status) => return Err(from_status_like_fdio!(status)),
                Ok(zxio) => (RemoteNode { zxio: Arc::new(zxio), rights }, attrs.id, true),
            };
        let mut root_node = FsNode::new_root(remote_node);
        root_node.node_id = node_id;
        let fs = FileSystem::new(
            kernel,
            CacheMode::Cached,
            RemoteFs { supports_open2 },
            FileSystemOptions {
                source: source.as_bytes().to_vec(),
                flags: if rights.contains(fio::OpenFlags::RIGHT_WRITABLE) {
                    MountFlags::empty()
                } else {
                    MountFlags::RDONLY
                },
                params: b"".to_vec(),
            },
        );
        fs.set_root_node(root_node);
        Ok(fs)
    }
}

struct RemoteNode {
    /// The underlying Zircon I/O object for this remote node.
    ///
    /// We delegate to the zxio library for actually doing I/O with remote
    /// objects, including fuchsia.io.Directory and fuchsia.io.File objects.
    /// This structure lets us share code with FDIO and other Fuchsia clients.
    zxio: Arc<syncio::Zxio>,

    /// The fuchsia.io rights for the dir handle. Subdirs will be opened with
    /// the same rights.
    rights: fio::OpenFlags,
}

/// Create a file handle from a zx::Handle.
///
/// The handle must be a channel, socket, vmo or debuglog object. If the handle is a
/// channel, then the channel must implement the `fuchsia.unknown/Queryable` protocol.
pub fn new_remote_file(
    kernel: &Arc<Kernel>,
    handle: zx::Handle,
    flags: OpenFlags,
) -> Result<FileHandle, Errno> {
    let handle_type =
        handle.basic_info().map_err(|status| from_status_like_fdio!(status))?.object_type;
    let zxio = Zxio::create(handle).map_err(|status| from_status_like_fdio!(status))?;
    let attrs = zxio.attr_get().map_err(|status| from_status_like_fdio!(status))?;
    let mode = get_mode(&attrs)?;
    let ops: Box<dyn FileOps> = match handle_type {
        zx::ObjectType::CHANNEL | zx::ObjectType::VMO | zx::ObjectType::DEBUGLOG => {
            if mode.is_dir() {
                Box::new(RemoteDirectoryObject::new(zxio))
            } else {
                Box::new(RemoteFileObject::new(zxio))
            }
        }
        zx::ObjectType::SOCKET => Box::new(RemotePipeObject::new(Arc::new(zxio))),
        _ => return error!(ENOSYS),
    };
    let file_handle = Anon::new_file_extended(kernel, ops, flags, |id| {
        let mut info = FsNodeInfo::new(id, mode, FsCred::root());
        update_into_from_attrs(&mut info, &attrs);
        info
    });
    Ok(file_handle)
}

pub fn create_fuchsia_pipe(
    current_task: &CurrentTask,
    socket: zx::Socket,
    flags: OpenFlags,
) -> Result<FileHandle, Errno> {
    new_remote_file(current_task.kernel(), socket.into(), flags)
}

pub fn update_into_from_attrs(info: &mut FsNodeInfo, attrs: &zxio_node_attributes_t) {
    /// st_blksize is measured in units of 512 bytes.
    const BYTES_PER_BLOCK: usize = 512;
    // TODO - store these in FsNodeState and convert on fstat
    info.ino = attrs.id;
    info.size = attrs.content_size.try_into().unwrap_or(std::usize::MAX);
    info.blocks = usize::try_from(attrs.storage_size).unwrap_or(std::usize::MAX) / BYTES_PER_BLOCK;
    info.blksize = BYTES_PER_BLOCK;
    info.link_count = attrs.link_count.try_into().unwrap_or(std::usize::MAX);
}

fn get_mode(attrs: &zxio_node_attributes_t) -> Result<FileMode, Errno> {
    let mut mode =
        FileMode::from_bits(unsafe { zxio_get_posix_mode(attrs.protocols, attrs.abilities) });
    let user_perms = mode.bits() & 0o700;
    // Make sure the same permissions are granted to user, group, and other.
    mode |= FileMode::from_bits((user_perms >> 3) | (user_perms >> 6));
    Ok(mode)
}

fn get_zxio_signals_from_events(events: FdEvents) -> zxio::zxio_signals_t {
    let mut signals = ZxioSignals::NONE;

    if events.contains(FdEvents::POLLIN) {
        signals |= ZxioSignals::READABLE;
    }
    if events.contains(FdEvents::POLLPRI) {
        signals |= ZxioSignals::OUT_OF_BAND;
    }
    if events.contains(FdEvents::POLLOUT) {
        signals |= ZxioSignals::WRITABLE;
    }
    if events.contains(FdEvents::POLLERR) {
        signals |= ZxioSignals::ERROR;
    }
    if events.contains(FdEvents::POLLHUP) {
        signals |= ZxioSignals::PEER_CLOSED;
    }
    if events.contains(FdEvents::POLLRDHUP) {
        signals |= ZxioSignals::READ_DISABLED;
    }

    signals.bits()
}

fn get_events_from_zxio_signals(signals: zxio::zxio_signals_t) -> FdEvents {
    let zxio_signals = ZxioSignals::from_bits_truncate(signals);

    let mut events = FdEvents::empty();

    if zxio_signals.contains(ZxioSignals::READABLE) {
        events |= FdEvents::POLLIN;
    }
    if zxio_signals.contains(ZxioSignals::OUT_OF_BAND) {
        events |= FdEvents::POLLPRI;
    }
    if zxio_signals.contains(ZxioSignals::WRITABLE) {
        events |= FdEvents::POLLOUT;
    }
    if zxio_signals.contains(ZxioSignals::ERROR) {
        events |= FdEvents::POLLERR;
    }
    if zxio_signals.contains(ZxioSignals::PEER_CLOSED) {
        events |= FdEvents::POLLHUP;
    }
    if zxio_signals.contains(ZxioSignals::READ_DISABLED) {
        events |= FdEvents::POLLRDHUP;
    }

    events
}

fn get_name_str(name_bytes: &FsStr) -> Result<&str, Errno> {
    std::str::from_utf8(name_bytes).map_err(|_| {
        log_warn!("bad utf8 in pathname! remote filesystems can't handle this");
        errno!(EINVAL)
    })
}

impl FsNodeOps for RemoteNode {
    fn create_file_ops(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let zxio = (*self.zxio).clone().map_err(|status| from_status_like_fdio!(status))?;
        if node.is_dir() {
            return Ok(Box::new(RemoteDirectoryObject::new(zxio)));
        }

        Ok(Box::new(RemoteFileObject::new(zxio)))
    }

    fn mknod(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        mut mode: FileMode,
        _dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let name = get_name_str(name)?;
        if !mode.is_reg() {
            log_warn!("Can only create regular files in remotefs.");
            return error!(EINVAL, name);
        }

        let zxio;
        let node_id;
        if supports_open2(node) {
            let mut attrs = zxio_node_attributes_t {
                has: zxio_node_attr_has_t { id: true, ..Default::default() },
                ..Default::default()
            };
            zxio = Arc::new(
                self.zxio
                    .open2(
                        name,
                        syncio::OpenOptions {
                            node_protocols: Some(fio::NodeProtocols {
                                file: Some(fio::FileProtocolFlags::default()),
                                ..Default::default()
                            }),
                            mode: fio::OpenMode::AlwaysCreate,
                            create_attr: Some(zxio_node_attributes_t {
                                mode: mode.bits(),
                                has: zxio_node_attr_has_t { mode: true, ..Default::default() },
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        Some(&mut attrs),
                    )
                    .map_err(|status| from_status_like_fdio!(status, name))?,
            );
            node_id = attrs.id;
        } else {
            let open_flags = fio::OpenFlags::CREATE
                | fio::OpenFlags::CREATE_IF_ABSENT
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::RIGHT_READABLE;
            zxio = Arc::new(
                self.zxio
                    .open(open_flags, name)
                    .map_err(|status| from_status_like_fdio!(status, name))?,
            );
            // Unfortunately, remote filesystems that don't support open2 require another
            // round-trip.
            let attrs = zxio.attr_get().map_err(|status| from_status_like_fdio!(status, name))?;
            mode = get_mode(&attrs)?;
            node_id = attrs.id;
        }

        let ops = Box::new(RemoteNode { zxio, rights: self.rights });
        let child =
            node.fs().create_node_with_id(ops, node_id, FsNodeInfo::new(node_id, mode, owner));
        Ok(child)
    }

    fn mkdir(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let name = get_name_str(name)?;
        let open_flags = fio::OpenFlags::CREATE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::DIRECTORY;
        let zxio = Arc::new(
            self.zxio
                .open(open_flags, name)
                .map_err(|status| from_status_like_fdio!(status, name))?,
        );

        // TODO: It's unfortunate to have another round-trip. We should be able
        // to set the mode based on the information we get during open.
        let attrs = zxio.attr_get().map_err(|status| from_status_like_fdio!(status, name))?;
        let ops = Box::new(RemoteNode { zxio, rights: self.rights });
        let child =
            node.fs().create_node_with_id(ops, attrs.id, FsNodeInfo::new(attrs.id, mode, owner));
        Ok(child)
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let name = get_name_str(name)?;
        let zxio;
        let mode;
        let node_id;
        if supports_open2(node) {
            let mut attrs = zxio_node_attributes_t {
                has: zxio_node_attr_has_t {
                    protocols: true,
                    mode: true,
                    id: true,
                    ..Default::default()
                },
                ..Default::default()
            };
            zxio = Arc::new(
                self.zxio
                    .open2(
                        name,
                        syncio::OpenOptions {
                            node_protocols: Some(fio::NodeProtocols {
                                directory: Some(Default::default()),
                                file: Some(Default::default()),
                                symlink: Some(Default::default()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        Some(&mut attrs),
                    )
                    .map_err(|status| from_status_like_fdio!(status, name))?,
            );
            if attrs.protocols & ZXIO_NODE_PROTOCOL_SYMLINK != 0 {
                // We don't set the mode for symbolic links, so we synthesize it instead.
                mode = FileMode::IFLNK | FileMode::ALLOW_ALL;
            } else {
                mode = FileMode::from_bits(attrs.mode);
            }
            node_id = attrs.id;
        } else {
            zxio = Arc::new(self.zxio.open(self.rights, name).map_err(|status| match status {
                // TODO: When the file is not found `PEER_CLOSED` is returned. In this case the peer
                // closed should be translated into ENOENT, so that the file may be created. This
                // logic creates a race when creating files in remote filesystems, between us and
                // any other client creating a file between here and `mknod`.
                zx::Status::PEER_CLOSED => errno!(ENOENT, name),
                status => from_status_like_fdio!(status, name),
            })?);

            // Unfortunately, remote filesystems that don't support open2 require another
            // round-trip.
            let attrs = zxio.attr_get().map_err(|status| from_status_like_fdio!(status))?;
            mode = get_mode(&attrs)?;
            node_id = attrs.id;
        }

        let ops = if mode.is_lnk() {
            Box::new(RemoteSymlink { zxio }) as Box<dyn FsNodeOps>
        } else {
            Box::new(RemoteNode { zxio, rights: self.rights }) as Box<dyn FsNodeOps>
        };
        let child = node.fs().create_node_with_id(
            ops,
            node_id,
            FsNodeInfo::new(node_id, mode, FsCred::root()),
        );
        Ok(child)
    }

    fn truncate(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        self.zxio.truncate(length).map_err(|status| from_status_like_fdio!(status))
    }

    fn update_info<'a>(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let attrs = self.zxio.attr_get().map_err(|status| from_status_like_fdio!(status))?;
        let mut info = info.write();
        update_into_from_attrs(&mut info, &attrs);
        Ok(RwLockWriteGuard::downgrade(info))
    }

    fn unlink(
        &self,
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
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let name = get_name_str(name)?;
        let zxio = Arc::new(
            self.zxio
                .create_symlink(name, target)
                .map_err(|status| from_status_like_fdio!(status))?,
        );
        let attrs = zxio.attr_get().map_err(|status| from_status_like_fdio!(status))?;
        let symlink = node.fs().create_node_with_id(
            Box::new(RemoteSymlink { zxio }),
            attrs.id,
            FsNodeInfo::new(attrs.id, get_mode(&attrs)?, owner),
        );
        Ok(symlink)
    }

    fn get_xattr(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        _max_size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        let value = self.zxio.xattr_get(name).map_err(|status| match status {
            zx::Status::NOT_FOUND => errno!(ENODATA),
            status => from_status_like_fdio!(status),
        })?;
        Ok(value.into())
    }

    fn set_xattr(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        value: &FsStr,
        op: XattrOp,
    ) -> Result<(), Errno> {
        let mode = match op {
            XattrOp::Set => XattrSetMode::Set,
            XattrOp::Create => XattrSetMode::Create,
            XattrOp::Replace => XattrSetMode::Replace,
        };

        self.zxio.xattr_set(name, value, mode).map_err(|status| match status {
            zx::Status::NOT_FOUND => errno!(ENODATA),
            status => from_status_like_fdio!(status),
        })
    }

    fn remove_xattr(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<(), Errno> {
        self.zxio.xattr_remove(name).map_err(|status| match status {
            zx::Status::NOT_FOUND => errno!(ENODATA),
            _ => from_status_like_fdio!(status),
        })
    }

    fn list_xattrs(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        self.zxio
            .xattr_list()
            .map(ValueOrSize::from)
            .map_err(|status| from_status_like_fdio!(status))
    }
}

fn zxio_read(zxio: &Zxio, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
    let total = data.available();
    let mut bytes = vec![0u8; total];
    let actual = zxio.read(&mut bytes).map_err(|status| from_status_like_fdio!(status))?;
    data.write_all(&bytes[0..actual])
}

fn zxio_read_at(zxio: &Zxio, offset: usize, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
    let total = data.available();
    let mut bytes = vec![0u8; total];
    let actual =
        zxio.read_at(offset as u64, &mut bytes).map_err(|status| from_status_like_fdio!(status))?;
    data.write_all(&bytes)?;
    Ok(actual)
}

fn zxio_write(
    zxio: &Zxio,
    _current_task: &CurrentTask,
    data: &mut dyn InputBuffer,
) -> Result<usize, Errno> {
    let bytes = data.peek_all()?;
    let actual = zxio.write(&bytes).map_err(|status| from_status_like_fdio!(status))?;
    data.advance(actual)?;
    Ok(actual)
}

fn zxio_write_at(
    zxio: &Zxio,
    _current_task: &CurrentTask,
    offset: usize,
    data: &mut dyn InputBuffer,
) -> Result<usize, Errno> {
    let bytes = data.peek_all()?;
    let actual =
        zxio.write_at(offset as u64, &bytes).map_err(|status| from_status_like_fdio!(status))?;
    data.advance(actual)?;
    Ok(actual)
}

pub fn zxio_wait_async(
    zxio: &Arc<Zxio>,
    waiter: &Waiter,
    events: FdEvents,
    handler: EventHandler,
) -> WaitCanceler {
    let zxio_clone = zxio.clone();
    let signal_handler = move |signals: zx::Signals| {
        let observed_zxio_signals = zxio_clone.wait_end(signals);
        let observed_events = get_events_from_zxio_signals(observed_zxio_signals);
        handler(observed_events);
    };

    let (handle, signals) = zxio.wait_begin(get_zxio_signals_from_events(events));
    // unwrap OK here as errors are only generated from misuse
    let zxio = Arc::downgrade(zxio);
    let canceler =
        waiter.wake_on_zircon_signals(&handle, signals, Box::new(signal_handler)).unwrap();
    WaitCanceler::new(move || {
        if let Some(zxio) = zxio.upgrade() {
            let (handle, signals) = zxio.wait_begin(ZxioSignals::NONE.bits());
            let did_cancel = canceler.cancel(handle);
            zxio.wait_end(signals);
            did_cancel
        } else {
            false
        }
    })
}

pub fn zxio_query_events(zxio: &Arc<Zxio>) -> FdEvents {
    let (handle, signals) = zxio.wait_begin(ZxioSignals::all().bits());
    // Wait can error out if the remote gets closed.
    let observed_signals = match handle.wait(signals, zx::Time::INFINITE_PAST) {
        Ok(signals) => signals,
        Err(_) => return FdEvents::POLLHUP,
    };
    let observed_zxio_signals = zxio.wait_end(observed_signals);
    get_events_from_zxio_signals(observed_zxio_signals)
}

/// Helper struct to track the context necessary to iterate over dir entries.
#[derive(Default)]
struct RemoteDirectoryIterator {
    iterator: Option<DirentIterator>,

    /// If the last attempt to write to the sink failed, this contains the entry
    /// that is pending to be added.
    pending_entry: Option<ZxioDirent>,
}

impl RemoteDirectoryIterator {
    fn get_or_init_iterator(&mut self, zxio: &Zxio) -> Result<&mut DirentIterator, Errno> {
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

    /// Returns the next dir entry. If no more entries are found, returns None.
    /// Returns an error if the iterator fails for other reasons described by
    /// the zxio library.
    pub fn next(&mut self, zxio: &Zxio) -> Option<Result<ZxioDirent, Errno>> {
        match self.pending_entry.take() {
            Some(entry) => Some(Ok(entry)),
            None => {
                let iterator = match self.get_or_init_iterator(zxio) {
                    Ok(iter) => iter,
                    Err(e) => return Some(Err(e)),
                };
                let result = iterator.next();
                match result {
                    Some(Ok(v)) => Some(Ok(v)),
                    Some(Err(status)) => Some(Err(from_status_like_fdio!(status))),
                    None => None,
                }
            }
        }
    }
}

struct RemoteDirectoryObject {
    /// The underlying Zircon I/O object.
    zxio: Zxio,

    iterator: Mutex<RemoteDirectoryIterator>,
}

impl RemoteDirectoryObject {
    pub fn new(zxio: Zxio) -> RemoteDirectoryObject {
        RemoteDirectoryObject { zxio, iterator: Mutex::new(RemoteDirectoryIterator::default()) }
    }
}

impl FileOps for RemoteDirectoryObject {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        let mut iterator = self.iterator.lock();
        let new_offset = default_seek(current_offset, target, |_| error!(EINVAL))?;
        let mut iterator_position = current_offset;

        if new_offset < iterator_position {
            // Our iterator only goes forward, so reset it here.
            *iterator = RemoteDirectoryIterator::default();
            iterator_position = 0;
        }

        if iterator_position != new_offset {
            iterator.pending_entry = None;
        }

        // Advance the iterator to catch up with the offset.
        for i in iterator_position..new_offset {
            match iterator.next(&self.zxio) {
                Some(Ok(_)) => continue,
                None => break, // No more entries.
                Some(Err(_)) => {
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
        _file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        // It is important to acquire the lock to the offset before the context,
        //  to avoid a deadlock where seek() tries to modify the context.
        let mut iterator = self.iterator.lock();

        let mut add_entry = |entry: &ZxioDirent| {
            let inode_num: ino_t = entry.id.ok_or_else(|| errno!(EIO))?;
            let entry_type = if entry.is_dir() {
                DirectoryEntryType::DIR
            } else if entry.is_file() {
                DirectoryEntryType::REG
            } else {
                DirectoryEntryType::UNKNOWN
            };
            sink.add(inode_num, sink.offset() + 1, entry_type, &entry.name)?;
            Ok(())
        };

        while let Some(entry) = iterator.next(&self.zxio) {
            if let Ok(entry) = entry {
                if let Err(e) = add_entry(&entry) {
                    iterator.pending_entry = Some(entry);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    fn to_handle(
        &self,
        _file: &FileHandle,
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
    /// The underlying Zircon I/O object.
    zxio: Arc<Zxio>,
}

impl RemoteFileObject {
    pub fn new(zxio: Zxio) -> RemoteFileObject {
        RemoteFileObject { zxio: Arc::new(zxio) }
    }
}

impl FileOps for RemoteFileObject {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        zxio_read_at(&self.zxio, offset, data)
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        zxio_write_at(&self.zxio, current_task, offset, data)
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        let has_execute = prot.contains(ProtectionFlags::EXEC);
        let vmar_flags = prot.to_vmar_flags() - zx::VmarFlags::PERM_EXECUTE;
        // TODO(tbodt): Consider caching the VMO handle instead of getting a new one on each call.
        let mut vmo =
            self.zxio.vmo_get(vmar_flags).map_err(|status| from_status_like_fdio!(status))?;
        if has_execute {
            vmo = vmo.replace_as_executable(&VMEX_RESOURCE).map_err(impossible_error)?;
        }
        Ok(Arc::new(vmo))
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(zxio_wait_async(&self.zxio, waiter, events, handler))
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(zxio_query_events(&self.zxio))
    }

    fn to_handle(
        &self,
        _file: &FileHandle,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.zxio
            .as_ref()
            .clone()
            .and_then(Zxio::release)
            .map(Some)
            .map_err(|status| from_status_like_fdio!(status))
    }
}

struct RemotePipeObject {
    /// The underlying Zircon I/O object.
    ///
    /// Shared with RemoteNode.
    zxio: Arc<syncio::Zxio>,
}

impl RemotePipeObject {
    fn new(zxio: Arc<Zxio>) -> Self {
        Self { zxio }
    }
}

impl FileOps for RemotePipeObject {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, || {
            zxio_read(&self.zxio, data)
        })
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLOUT | FdEvents::POLLHUP, None, || {
            zxio_write(&self.zxio, current_task, data)
        })
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(zxio_wait_async(&self.zxio, waiter, events, handler))
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(zxio_query_events(&self.zxio))
    }

    fn to_handle(
        &self,
        _file: &FileHandle,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.zxio
            .as_ref()
            .clone()
            .and_then(Zxio::release)
            .map(Some)
            .map_err(|status| from_status_like_fdio!(status))
    }
}

struct RemoteSymlink {
    zxio: Arc<syncio::Zxio>,
}

impl FsNodeOps for RemoteSymlink {
    fs_node_impl_symlink!();

    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path(
            self.zxio.read_link().map_err(|status| from_status_like_fdio!(status))?.to_vec(),
        ))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{arch::uapi::epoll_event, fs::buffers::VecOutputBuffer, mm::PAGE_SIZE, testing::*};
    use fidl::endpoints::Proxy;
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use fuchsia_fs::{directory, file};
    use fuchsia_zircon::HandleBased;
    use fxfs_testing::{TestFixture, TestFixtureOptions};

    #[::fuchsia::test]
    async fn test_tree() -> Result<(), anyhow::Error> {
        let (kernel, current_task) = create_kernel_and_task();
        let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
        let (server, client) = zx::Channel::create();
        fdio::open("/pkg", rights, server).expect("failed to open /pkg");
        let fs = RemoteFs::new_fs(&kernel, client, "/pkg", rights)?;
        let ns = Namespace::new(fs);
        let root = ns.root();
        let mut context = LookupContext::default();
        assert_eq!(
            root.lookup_child(&current_task, &mut context, b"nib").err(),
            Some(errno!(ENOENT))
        );
        let mut context = LookupContext::default();
        root.lookup_child(&current_task, &mut context, b"lib").unwrap();

        let mut context = LookupContext::default();
        let _test_file = root
            .lookup_child(&current_task, &mut context, b"bin/hello_starnix")?
            .open(&current_task, OpenFlags::RDONLY, true)?;
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_blocking_io() -> Result<(), anyhow::Error> {
        let (_kernel, current_task) = create_kernel_and_task();

        let (client, server) = zx::Socket::create_stream();
        let pipe = create_fuchsia_pipe(&current_task, client, OpenFlags::RDWR)?;

        let thread = std::thread::spawn(move || {
            assert_eq!(64, pipe.read(&current_task, &mut VecOutputBuffer::new(64)).unwrap());
        });

        // Wait for the thread to become blocked on the read.
        zx::Duration::from_seconds(2).sleep();

        let bytes = [0u8; 64];
        assert_eq!(64, server.write(&bytes)?);

        // The thread should unblock and join us here.
        let _ = thread.join();

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_poll() {
        let (_kernel, current_task) = create_kernel_and_task();

        let (client, server) = zx::Socket::create_stream();
        let pipe = create_fuchsia_pipe(&current_task, client, OpenFlags::RDWR)
            .expect("create_fuchsia_pipe");
        let server_zxio = Zxio::create(server.into_handle()).expect("Zxio::create");

        assert_eq!(pipe.query_events(&current_task), Ok(FdEvents::POLLOUT | FdEvents::POLLWRNORM));

        let epoll_object = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_object.downcast_file::<EpollFileObject>().unwrap();
        let event = epoll_event::new(FdEvents::POLLIN.bits(), 0);
        epoll_file.add(&current_task, &pipe, &epoll_object, event).expect("poll_file.add");

        let fds = epoll_file.wait(&current_task, 1, zx::Time::ZERO).expect("wait");
        assert!(fds.is_empty());

        assert_eq!(server_zxio.write(&[0]).expect("write"), 1);

        assert_eq!(
            pipe.query_events(&current_task),
            Ok(FdEvents::POLLOUT | FdEvents::POLLWRNORM | FdEvents::POLLIN | FdEvents::POLLRDNORM)
        );
        let fds = epoll_file.wait(&current_task, 1, zx::Time::ZERO).expect("wait");
        assert_eq!(fds.len(), 1);

        assert_eq!(pipe.read(&current_task, &mut VecOutputBuffer::new(64)).expect("read"), 1);

        assert_eq!(pipe.query_events(&current_task), Ok(FdEvents::POLLOUT | FdEvents::POLLWRNORM));
        let fds = epoll_file.wait(&current_task, 1, zx::Time::ZERO).expect("wait");
        assert!(fds.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_new_remote_directory() {
        let (kernel, current_task) = create_kernel_and_task();
        let pkg_channel: zx::Channel = directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .expect("failed to open /pkg")
        .into_channel()
        .expect("into_channel")
        .into();

        let fd =
            new_remote_file(&kernel, pkg_channel.into(), OpenFlags::RDWR).expect("new_remote_file");
        assert!(fd.node().is_dir());
        assert!(fd.to_handle(&current_task).expect("to_handle").is_some());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_new_remote_file() {
        let (kernel, current_task) = create_kernel_and_task();
        let content_channel: zx::Channel =
            file::open_in_namespace("/pkg/meta/contents", fio::OpenFlags::RIGHT_READABLE)
                .expect("failed to open /pkg/meta/contents")
                .into_channel()
                .expect("into_channel")
                .into();

        let fd = new_remote_file(&kernel, content_channel.into(), OpenFlags::RDONLY)
            .expect("new_remote_file");
        assert!(!fd.node().is_dir());
        assert!(fd.to_handle(&current_task).expect("to_handle").is_some());
    }

    #[::fuchsia::test]
    async fn test_new_remote_vmo() {
        let (kernel, current_task) = create_kernel_and_task();
        let vmo = zx::Vmo::create(*PAGE_SIZE).expect("Vmo::create");
        let fd = new_remote_file(&kernel, vmo.into(), OpenFlags::RDWR).expect("new_remote_file");
        assert!(!fd.node().is_dir());
        assert!(fd.to_handle(&current_task).expect("to_handle").is_some());
    }

    #[::fuchsia::test(threads = 2)]
    async fn test_symlink() {
        let fixture = TestFixture::new().await;

        {
            let (kernel, current_task) = create_kernel_and_task();
            let (server, client) = zx::Channel::create();
            fixture
                .root()
                .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into())
                .expect("clone failed");
            let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
            let fs = RemoteFs::new_fs(&kernel, client, "/", rights).expect("new_fs failed");
            let ns = Namespace::new(fs);
            let root = ns.root();
            root.create_symlink(&current_task, b"symlink", b"target").expect("symlink failed");

            let mut context = LookupContext::new(SymlinkMode::NoFollow);
            let child = root
                .lookup_child(&current_task, &mut context, b"symlink")
                .expect("lookup_child failed");

            match child.readlink(&current_task).expect("readlink failed") {
                SymlinkTarget::Path(path) => assert_eq!(path, b"target"),
                SymlinkTarget::Node(_) => panic!("readlink returned SymlinkTarget::Node"),
            }
        }

        fixture.close().await;
    }

    #[::fuchsia::test]
    async fn test_mode_persists() {
        let fixture = TestFixture::new().await;

        let (server, client) = zx::Channel::create();
        fixture
            .root()
            .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into())
            .expect("clone failed");

        const MODE: FileMode = FileMode::from_bits(FileMode::IFREG.bits() | 0o467);

        let (kernel, current_task) = create_kernel_and_task();
        fasync::unblock(move || {
            let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
            let fs = RemoteFs::new_fs(&kernel, client, "/", rights).expect("new_fs failed");
            let ns = Namespace::new(fs);
            current_task.fs().set_umask(FileMode::from_bits(0));
            ns.root()
                .create_node(&current_task, b"file", MODE, DeviceType::NONE)
                .expect("create_node failed");
        })
        .await;

        let fixture = TestFixture::open(
            fixture.close().await,
            TestFixtureOptions { encrypted: true, as_blob: false, format: false },
        )
        .await;

        let (server, client) = zx::Channel::create();
        fixture
            .root()
            .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into())
            .expect("clone failed");

        let (kernel, current_task) = create_kernel_and_task();
        fasync::unblock(move || {
            let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
            let fs = RemoteFs::new_fs(&kernel, client, "/", rights).expect("new_fs failed");
            let ns = Namespace::new(fs);
            let mut context = LookupContext::new(SymlinkMode::NoFollow);
            let child = ns
                .root()
                .lookup_child(&current_task, &mut context, b"file")
                .expect("lookup_child failed");
            assert_eq!(child.entry.node.info().mode, MODE);
        })
        .await;

        fixture.close().await;
    }
}
