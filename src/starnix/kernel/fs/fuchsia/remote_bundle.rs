// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::fuchsia::update_info_from_attrs,
    mm::{ProtectionFlags, VMEX_RESOURCE},
    task::{CurrentTask, EventHandler, Kernel, WaitCanceler, Waiter},
    vfs::{
        default_seek, emit_dotdot, fileops_impl_directory, fileops_impl_seekable,
        fs_node_impl_dir_readonly, fs_node_impl_not_dir, fs_node_impl_symlink, CacheConfig,
        CacheMode, DirectoryEntryType, DirentSink, FileObject, FileOps, FileSystem,
        FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo,
        FsNodeOps, FsStr, FsString, InputBuffer, OutputBuffer, SeekTarget, SymlinkTarget,
        ValueOrSize, DEFAULT_BYTES_PER_BLOCK,
    },
};
use anyhow::{anyhow, ensure, Error};
use ext4_metadata::{Metadata, Node, NodeInfo};
use fidl_fuchsia_io as fio;
use fuchsia_zircon::{
    HandleBased, {self as zx},
};
use starnix_logging::{impossible_error, log_warn};
use starnix_sync::{FileOpsCore, Locked, RwLock, RwLockReadGuard, RwLockWriteGuard, WriteOps};
use starnix_uapi::{
    auth::FsCred,
    errno, error,
    errors::{Errno, SourceContext},
    file_mode::FileMode,
    from_status_like_fdio, ino_t,
    mount_flags::MountFlags,
    off_t,
    open_flags::OpenFlags,
    statfs,
    vfs::default_statfs,
    vfs::FdEvents,
};
use std::{
    io::Read,
    sync::{Arc, Mutex},
};
use syncio::{zxio_node_attr_has_t, zxio_node_attributes_t};

const REMOTE_BUNDLE_NODE_LRU_CAPACITY: usize = 1024;

/// RemoteBundle is a remote, immutable filesystem that stores additional metadata that would
/// otherwise not be available.  The metadata exists in the "metadata.v1" file, which contains
/// directory, symbolic link and extended attribute information.  Only the content for files are
/// accessed remotely as normal.
pub struct RemoteBundle {
    metadata: Metadata,
    root: fio::DirectorySynchronousProxy,
    rights: fio::OpenFlags,
}

impl RemoteBundle {
    /// Returns a new RemoteBundle filesystem that can be found at `path` relative to `base`.
    pub fn new_fs(
        kernel: &Arc<Kernel>,
        base: &fio::DirectorySynchronousProxy,
        rights: fio::OpenFlags,
        path: &str,
    ) -> Result<FileSystemHandle, Error> {
        let (root, server_end) = fidl::endpoints::create_endpoints::<fio::NodeMarker>();
        base.open(rights, fio::ModeType::empty(), path, server_end)
            .map_err(|e| anyhow!("Failed to open root: {}", e))?;
        let root = fio::DirectorySynchronousProxy::new(root.into_channel());

        let metadata = {
            let (file, server_end) = fidl::endpoints::create_endpoints::<fio::NodeMarker>();
            root.open(
                fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "metadata.v1",
                server_end,
            )
            .source_context("open metadata file")?;
            let mut file: std::fs::File = fdio::create_fd(file.into_channel().into_handle())
                .source_context("create fd from metadata file (wrong mount path?)")?
                .into();
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).source_context("read metadata file")?;
            Metadata::deserialize(&buf).source_context("deserialize metadata file")?
        };

        // Make sure the root node exists.
        ensure!(
            metadata.get(ext4_metadata::ROOT_INODE_NUM).is_some(),
            "Root node does not exist in remote bundle"
        );

        let fs = FileSystem::new(
            kernel,
            CacheMode::Cached(CacheConfig { capacity: REMOTE_BUNDLE_NODE_LRU_CAPACITY }),
            RemoteBundle { metadata, root, rights },
            FileSystemOptions {
                source: path.into(),
                flags: if rights.contains(fio::OpenFlags::RIGHT_WRITABLE) {
                    MountFlags::empty()
                } else {
                    MountFlags::RDONLY
                },
                params: Default::default(),
            },
        )?;
        let mut root_node = FsNode::new_root(DirectoryObject);
        root_node.node_id = ext4_metadata::ROOT_INODE_NUM;
        fs.set_root_node(root_node);
        Ok(fs)
    }

    // Returns the bundle from the filesystem.  Panics if the filesystem isn't associated with a
    // RemoteBundle.
    fn from_fs(fs: &FileSystem) -> &RemoteBundle {
        fs.downcast_ops::<RemoteBundle>().unwrap()
    }

    // Returns a reference to the node identified by `inode_num`.  Panics if the node is not found
    // so this should only be used if the node is known to exist (e.g. the node must exist after
    // `lookup` has run for the relevant node).
    fn get_node(&self, inode_num: u64) -> &Node {
        self.metadata.get(inode_num).unwrap()
    }
}

impl FileSystemOps for RemoteBundle {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        const REMOTE_BUNDLE_FS_MAGIC: u32 = u32::from_be_bytes(*b"bndl");
        Ok(default_statfs(REMOTE_BUNDLE_FS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "remote_bundle".into()
    }
}

struct File {
    inner: Mutex<Inner>,
}

enum Inner {
    NeedsVmo(fio::FileSynchronousProxy),
    Vmo(Arc<zx::Vmo>),
}

impl Inner {
    fn get_vmo(&mut self) -> Result<Arc<zx::Vmo>, Errno> {
        if let Inner::NeedsVmo(file) = &*self {
            let vmo = match file
                .get_backing_memory(fio::VmoFlags::READ, zx::Time::INFINITE)
                .map_err(|err| errno!(EIO, format!("Error {err} on GetBackingMemory")))?
                .map_err(zx::Status::from_raw)
            {
                Ok(vmo) => Arc::new(vmo),
                Err(zx::Status::BAD_STATE) => {
                    // TODO(https://fxbug.dev/305272765): ZX_ERR_BAD_STATE is returned for the empty
                    // blob, but Blobfs/Fxblob should be changed to handle this case
                    // successfully.  Remove the error-swallowing when the behaviour is fixed.
                    Arc::new(
                        zx::Vmo::create_with_opts(zx::VmoOptions::empty(), 0)
                            .map_err(|status| from_status_like_fdio!(status))?,
                    )
                }
                Err(status) => return Err(from_status_like_fdio!(status)),
            };
            *self = Inner::Vmo(vmo);
        }
        let Inner::Vmo(vmo) = &*self else { unreachable!() };
        Ok(vmo.clone())
    }
}

impl FsNodeOps for File {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let vmo = self.inner.lock().unwrap().get_vmo()?;
        let size = usize::try_from(
            vmo.get_content_size().map_err(|status| from_status_like_fdio!(status))?,
        )
        .unwrap();
        Ok(Box::new(VmoFile { vmo, size }))
    }

    fn fetch_and_refresh_info<'a>(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let vmo = self.inner.lock().unwrap().get_vmo()?;
        let attrs = zxio_node_attributes_t {
            content_size: vmo
                .get_content_size()
                .map_err(|status| from_status_like_fdio!(status))?,
            // TODO(https://fxbug.dev/293607051): Plumb through storage size from underlying connection.
            storage_size: 0,
            link_count: 1,
            has: zxio_node_attr_has_t {
                content_size: true,
                storage_size: true,
                link_count: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut info = info.write();
        update_info_from_attrs(&mut info, &attrs);
        Ok(RwLockWriteGuard::downgrade(info))
    }

    fn get_xattr(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        _size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        let fs = node.fs();
        let bundle = RemoteBundle::from_fs(&fs);
        Ok(FsString::from(
            &bundle
                .get_node(node.node_id)
                .extended_attributes
                .get(&**name)
                .ok_or(errno!(ENOENT))?[..],
        )
        .into())
    }

    fn list_xattrs(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        _size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        let fs = node.fs();
        let bundle = RemoteBundle::from_fs(&fs);
        Ok(bundle
            .get_node(node.node_id)
            .extended_attributes
            .keys()
            .map(|k| FsString::from(&k[..]))
            .collect::<Vec<_>>()
            .into())
    }
}

// NB: This is different from VmoFileObject, which is designed to wrap a VMO that is owned and
// managed by Starnix.  This struct is a wrapper around a pager-backed VMO received from the
// filesystem backing the remote bundle.
// VmoFileObject does its own content size management, which is (a) incompatible with the content
// size management done for us by the remote filesystem, and (b) the content size is based on file
// attributes in the case of VmoFileObject, which we've intentionally avoided querying here for
// performance.  Specifically, VmoFile is designed to be opened as fast as possible, and requiring
// that we stat the file whilst opening it is counter to that goal.
// Note that VmoFile assumes that the underlying file is read-only and not resizable (which is the
// case for remote bundles since they're stored as blobs).
struct VmoFile {
    vmo: Arc<zx::Vmo>,
    size: usize,
}

impl FileOps for VmoFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        mut offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |buf| {
            let buflen = buf.len();
            let buf = &mut buf[..std::cmp::min(self.size.saturating_sub(offset), buflen)];
            if !buf.is_empty() {
                self.vmo
                    .read_uninit(buf, offset as u64)
                    .map_err(|status| from_status_like_fdio!(status))?;
                offset += buf.len();
            }
            Ok(buf.len())
        })
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Err(errno!(EPERM))
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        Ok(if prot.contains(ProtectionFlags::EXEC) {
            Arc::new(
                self.vmo
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .map_err(impossible_error)?
                    .replace_as_executable(&VMEX_RESOURCE)
                    .map_err(impossible_error)?,
            )
        } else {
            self.vmo.clone()
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
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::POLLIN)
    }
}

struct DirectoryObject;

impl FileOps for DirectoryObject {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        default_seek(current_offset, target, |_| error!(EINVAL))
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        let bundle = RemoteBundle::from_fs(&file.fs);
        let child_iter =
            bundle.get_node(file.node().node_id).directory().ok_or(errno!(EIO))?.children.iter();

        for (name, inode_num) in child_iter.skip(sink.offset() as usize - 2) {
            let node = bundle.metadata.get(*inode_num).ok_or(errno!(EIO))?;
            sink.add(
                *inode_num,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(FileMode::from_bits(node.mode.into())),
                name.as_str().into(),
            )?;
        }

        Ok(())
    }
}

impl FsNodeOps for DirectoryObject {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(DirectoryObject))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let name = std::str::from_utf8(name).map_err(|_| {
            log_warn!("bad utf8 in pathname! remote filesystems can't handle this");
            errno!(EINVAL)
        })?;

        let fs = node.fs();
        let bundle = RemoteBundle::from_fs(&fs);
        let metadata = &bundle.metadata;
        let inode_num = metadata
            .lookup(node.node_id, name)
            .map_err(|e| errno!(ENOENT, format!("Error: {e:?} opening {name}")))?;
        let metadata_node = metadata.get(inode_num).ok_or(errno!(EIO))?;
        let info = to_fs_node_info(inode_num, metadata_node);

        match metadata_node.info() {
            NodeInfo::Symlink(_) => {
                Ok(fs.create_node_with_id(current_task, SymlinkObject, inode_num, info))
            }
            NodeInfo::Directory(_) => {
                Ok(fs.create_node_with_id(current_task, DirectoryObject, inode_num, info))
            }
            NodeInfo::File(_) => {
                let (file, server_end) = fidl::endpoints::create_endpoints::<fio::NodeMarker>();
                bundle
                    .root
                    .open(
                        bundle.rights,
                        fio::ModeType::empty(),
                        &format!("{inode_num}"),
                        server_end,
                    )
                    .map_err(|_| errno!(EIO))?;
                let file = fio::FileSynchronousProxy::new(file.into_channel());
                Ok(fs.create_node_with_id(
                    current_task,
                    File { inner: Mutex::new(Inner::NeedsVmo(file)) },
                    inode_num,
                    info,
                ))
            }
        }
    }

    fn get_xattr(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        _size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        let fs = node.fs();
        let bundle = RemoteBundle::from_fs(&fs);
        let value = FsString::from(
            &bundle
                .get_node(node.node_id)
                .extended_attributes
                .get(&**name)
                .ok_or(errno!(ENOENT))?[..],
        );
        Ok(value.into())
    }

    fn list_xattrs(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        _size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        let fs = node.fs();
        let bundle = RemoteBundle::from_fs(&fs);
        Ok(bundle
            .get_node(node.node_id)
            .extended_attributes
            .keys()
            .map(|k| FsString::from(&**k))
            .collect::<Vec<_>>()
            .into())
    }
}

struct SymlinkObject;

impl FsNodeOps for SymlinkObject {
    fs_node_impl_symlink!();

    fn readlink(&self, node: &FsNode, _current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        let fs = node.fs();
        let bundle = RemoteBundle::from_fs(&fs);
        let target = bundle.get_node(node.node_id).symlink().ok_or(errno!(EIO))?.target.clone();
        Ok(SymlinkTarget::Path(target.into()))
    }
}

fn to_fs_node_info(inode_num: ino_t, metadata_node: &ext4_metadata::Node) -> FsNodeInfo {
    let mode = FileMode::from_bits(metadata_node.mode.into());
    let owner = FsCred { uid: metadata_node.uid.into(), gid: metadata_node.gid.into() };
    let mut info = FsNodeInfo::new(inode_num, mode, owner);
    // Set the information for directory and links. For file, they will be overwritten
    // by the FsNodeOps on first access.
    // For now, we just use some made up values. We might need to revisit this.
    info.size = 1;
    info.blocks = 1;
    info.blksize = DEFAULT_BYTES_PER_BLOCK;
    info.link_count = 1;
    info
}

#[cfg(test)]
mod test {
    use crate::{
        fs::fuchsia::RemoteBundle,
        testing::create_kernel_task_and_unlocked,
        vfs::{
            buffers::VecOutputBuffer, DirectoryEntryType, DirentSink, FsStr, LookupContext,
            Namespace, SymlinkMode, SymlinkTarget,
        },
    };
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use starnix_uapi::{errors::Errno, file_mode::FileMode, ino_t, off_t, open_flags::OpenFlags};
    use std::collections::{HashMap, HashSet};

    #[::fuchsia::test]
    async fn test_read_image() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
        let (server, client) = zx::Channel::create();
        fdio::open("/pkg", rights, server).expect("failed to open /pkg");
        let fs = RemoteBundle::new_fs(
            &kernel,
            &fio::DirectorySynchronousProxy::new(client),
            rights,
            "data/test-image",
        )
        .expect("new_fs failed");
        let ns = Namespace::new(fs);
        let root = ns.root();
        let mut context = LookupContext::default().with(SymlinkMode::NoFollow);

        let test_dir =
            root.lookup_child(&current_task, &mut context, "foo".into()).expect("lookup failed");

        let test_file = test_dir
            .lookup_child(&current_task, &mut context, "file".into())
            .expect("lookup failed")
            .open(&mut locked, &current_task, OpenFlags::RDONLY, true)
            .expect("open failed");

        let mut buffer = VecOutputBuffer::new(64);
        assert_eq!(
            test_file.read(&mut locked, &current_task, &mut buffer).expect("read failed"),
            6
        );
        let buffer: Vec<u8> = buffer.into();
        assert_eq!(&buffer[..6], b"hello\n");

        assert_eq!(
            &test_file
                .node()
                .get_xattr(&current_task, &test_dir.mount, "user.a".into(), usize::MAX)
                .expect("get_xattr failed")
                .unwrap(),
            "apple"
        );
        assert_eq!(
            &test_file
                .node()
                .get_xattr(&current_task, &test_dir.mount, "user.b".into(), usize::MAX)
                .expect("get_xattr failed")
                .unwrap(),
            "ball"
        );
        assert_eq!(
            test_file
                .node()
                .list_xattrs(&current_task, usize::MAX)
                .expect("list_xattr failed")
                .unwrap()
                .into_iter()
                .collect::<HashSet<_>>(),
            ["user.a".into(), "user.b".into()].into_iter().collect::<HashSet<_>>(),
        );

        {
            let info = test_file.node().info();
            assert_eq!(info.mode, FileMode::from_bits(0o100640));
            assert_eq!(info.uid, 49152); // These values come from the test image generated in
            assert_eq!(info.gid, 24403); // ext4_to_pkg.
        }

        let test_symlink = test_dir
            .lookup_child(&current_task, &mut context, "symlink".into())
            .expect("lookup failed");

        if let SymlinkTarget::Path(target) =
            test_symlink.readlink(&current_task).expect("readlink failed")
        {
            assert_eq!(&target, "file");
        } else {
            panic!("unexpected symlink type");
        }

        let opened_dir = test_dir
            .open(&mut locked, &current_task, OpenFlags::RDONLY, true)
            .expect("open failed");

        struct Sink {
            offset: off_t,
            entries: HashMap<Vec<u8>, (ino_t, DirectoryEntryType)>,
        }

        impl DirentSink for Sink {
            fn add(
                &mut self,
                inode_num: ino_t,
                offset: off_t,
                entry_type: DirectoryEntryType,
                name: &FsStr,
            ) -> Result<(), Errno> {
                assert_eq!(offset, self.offset + 1);
                self.entries.insert(name.to_vec(), (inode_num, entry_type));
                self.offset = offset;
                Ok(())
            }

            fn offset(&self) -> off_t {
                self.offset
            }
        }

        let mut sink = Sink { offset: 0, entries: HashMap::new() };
        opened_dir.readdir(&mut locked, &current_task, &mut sink).expect("readdir failed");

        assert_eq!(
            sink.entries,
            [
                (b".".into(), (test_dir.entry.node.node_id, DirectoryEntryType::DIR)),
                (b"..".into(), (root.entry.node.node_id, DirectoryEntryType::DIR)),
                (b"file".into(), (test_file.node().node_id, DirectoryEntryType::REG)),
                (b"symlink".into(), (test_symlink.entry.node.node_id, DirectoryEntryType::LNK))
            ]
            .into()
        );
    }
}
