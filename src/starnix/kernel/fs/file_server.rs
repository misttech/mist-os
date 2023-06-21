// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{VecInputBuffer, VecOutputBuffer},
        DirEntry, DirectoryEntryType, DirentSink, FileHandle, FsStr, LookupContext, NamespaceNode,
        RenameFlags, SeekTarget, UnlinkKind,
    },
    mm::ProtectionFlags,
    task::CurrentTask,
    types::{errno, error, ino_t, off_t, DeviceType, Errno, FileMode, OpenFlags},
};
use async_trait::async_trait;
use fidl::{
    endpoints::{ClientEnd, ServerEnd},
    HandleBased,
};
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use std::sync::Arc;
use vfs::{attributes, directory, execution_scope, file, path, ToObjectRequest};

/// Returns a handle implementing a fuchsia.io.Node delegating to the given `file`.
pub fn serve_file(
    current_task: &CurrentTask,
    file: &FileHandle,
) -> Result<ClientEnd<fio::NodeMarker>, Errno> {
    let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::NodeMarker>();
    serve_file_at(server_end, current_task, file)?;
    Ok(client_end)
}

pub fn serve_file_at(
    server_end: ServerEnd<fio::NodeMarker>,
    current_task: &CurrentTask,
    file: &FileHandle,
) -> Result<(), Errno> {
    // Reopen file object to not share state with the given FileObject.
    let file = file.name.open(current_task, file.flags(), false)?;
    let open_flags = file.flags();
    let kernel = current_task.kernel();
    // TODO(security): Switching to the `system_task` here loses track of the credentials from
    //                 `current_task`. Do we need to retain these credentials?
    let system_task = kernel.kthreads.system_task();
    let starnix_file = StarnixNodeConnection::new(system_task.clone(), file);
    fasync::Task::spawn_on(&kernel.kthreads.ehandle, async move {
        let scope = execution_scope::ExecutionScope::new();
        directory::entry::DirectoryEntry::open(
            starnix_file,
            scope.clone(),
            open_flags.into(),
            path::Path::dot(),
            server_end,
        );
        scope.wait().await;
    })
    .detach();
    Ok(())
}

/// A representation of `file` for the rust vfs.
///
/// This struct implements the following trait from the rust vfs library:
/// - directory::entry_container::Directory
/// - directory::entry_container::MutableDirectory
/// - file::File
/// - file::RawFileIoConnection
/// - directory::entry::DirectoryEntry
///
/// Each method is delegated back to the starnix vfs, using `task` as the current task. Blocking
/// methods are run from the kernel dynamic thread pool so that the async dispatched do not block
/// on these.
struct StarnixNodeConnection {
    task: Arc<CurrentTask>,
    file: FileHandle,
}

impl StarnixNodeConnection {
    fn new(task: Arc<CurrentTask>, file: FileHandle) -> Arc<Self> {
        Arc::new(StarnixNodeConnection { task, file })
    }

    fn is_dir(&self) -> bool {
        self.file.node().is_dir()
    }

    /// Reopen the current `StarnixNodeConnection` with the given `OpenFlags`. The new file will not share
    /// state. It is equivalent to opening the same file, not dup'ing the file descriptor.
    fn reopen(&self, flags: fio::OpenFlags) -> Result<Arc<Self>, Errno> {
        let file = self.file.name.open(&self.task, flags.into(), true)?;
        Ok(StarnixNodeConnection::new(self.task.clone(), file))
    }

    /// Implementation of `vfs::directory::entry_container::Directory::directory_read_dirents`.
    fn directory_read_dirents<'a>(
        &'a self,
        pos: &'a directory::traversal_position::TraversalPosition,
        sink: Box<dyn directory::dirents_sink::Sink>,
    ) -> Result<
        (
            directory::traversal_position::TraversalPosition,
            Box<dyn directory::dirents_sink::Sealed>,
        ),
        Errno,
    > {
        struct DirentSinkAdapter<'a> {
            sink: Option<directory::dirents_sink::AppendResult>,
            offset: &'a mut off_t,
        }
        impl<'a> DirentSinkAdapter<'a> {
            fn append(
                &mut self,
                entry: &directory::entry::EntryInfo,
                name: &str,
            ) -> Result<(), Errno> {
                let sink = self.sink.take();
                self.sink = match sink {
                    s @ Some(directory::dirents_sink::AppendResult::Sealed(_)) => {
                        self.sink = s;
                        return error!(ENOSPC);
                    }
                    Some(directory::dirents_sink::AppendResult::Ok(sink)) => {
                        Some(sink.append(entry, name))
                    }
                    None => return error!(ENOTSUP),
                };
                Ok(())
            }
        }
        impl<'a> DirentSink for DirentSinkAdapter<'a> {
            fn add(
                &mut self,
                inode_num: ino_t,
                offset: off_t,
                entry_type: DirectoryEntryType,
                name: &FsStr,
            ) -> Result<(), Errno> {
                // Ignore ..
                if name != b".." {
                    // Ignore entries with unknown types.
                    if let Some(dirent_type) = fio::DirentType::from_primitive(entry_type.bits()) {
                        let entry_info = directory::entry::EntryInfo::new(inode_num, dirent_type);
                        self.append(&entry_info, &String::from_utf8_lossy(name))?
                    }
                }
                *self.offset = offset;
                Ok(())
            }
            fn offset(&self) -> off_t {
                *self.offset
            }
            fn actual(&self) -> usize {
                0
            }
        }
        let offset = match pos {
            directory::traversal_position::TraversalPosition::Start => 0,
            directory::traversal_position::TraversalPosition::Name(_) => return error!(EINVAL),
            directory::traversal_position::TraversalPosition::Index(v) => *v as i64,
            directory::traversal_position::TraversalPosition::End => {
                return Ok((directory::traversal_position::TraversalPosition::End, sink.seal()));
            }
        };
        if *self.file.offset.lock() != offset {
            self.file.seek(&self.task, SeekTarget::Set(offset))?;
        }
        let mut file_offset = self.file.offset.lock();
        let mut dirent_sink = DirentSinkAdapter {
            sink: Some(directory::dirents_sink::AppendResult::Ok(sink)),
            offset: &mut file_offset,
        };
        self.file.readdir(&self.task, &mut dirent_sink)?;
        match dirent_sink.sink {
            Some(directory::dirents_sink::AppendResult::Sealed(seal)) => {
                Ok((directory::traversal_position::TraversalPosition::End, seal))
            }
            Some(directory::dirents_sink::AppendResult::Ok(sink)) => Ok((
                directory::traversal_position::TraversalPosition::Index(*file_offset as u64),
                sink.seal(),
            )),
            None => error!(ENOTSUP),
        }
    }

    fn lookup_parent<'a>(&self, path: &'a FsStr) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        self.task.lookup_parent(&mut LookupContext::default(), self.file.name.clone(), path)
    }

    /// Implementation of `vfs::directory::entry::DirectoryEntry::open`.
    fn directory_entry_open(
        self: Arc<Self>,
        scope: execution_scope::ExecutionScope,
        flags: fio::OpenFlags,
        path: path::Path,
        server_end: &mut ServerEnd<fio::NodeMarker>,
    ) -> Result<(), Errno> {
        if self.is_dir() {
            if path.is_dot() {
                // Reopen the current directory.
                let dir = self.reopen(flags)?;
                flags
                    .to_object_request(std::mem::replace(server_end, zx::Handle::invalid().into()))
                    .handle(|object_request| {
                        object_request.spawn_connection(
                            scope,
                            dir,
                            flags,
                            directory::mutable::connection::io1::MutableConnection::create,
                        )
                    });
                return Ok(());
            }

            // Open a path under the current directory.
            let path = path.into_string();
            let (node, name) = self.lookup_parent(path.as_bytes())?;
            let must_create_directory =
                flags.contains(fio::OpenFlags::DIRECTORY | fio::OpenFlags::CREATE);
            let file = match self.task.open_namespace_node_at(
                node.clone(),
                name,
                flags.into(),
                FileMode::ALLOW_ALL,
            ) {
                Err(e) if e == errno!(EISDIR) && must_create_directory => {
                    let mode =
                        self.task.fs().apply_umask(FileMode::from_bits(0o777) | FileMode::IFDIR);
                    let name = node.create_node(&self.task, name, mode, DeviceType::NONE)?;
                    let flags =
                        flags & !(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT);
                    name.open(&self.task, flags.into(), false)?
                }
                f => f?,
            };

            let starnix_file = StarnixNodeConnection::new(self.task.clone(), file);
            directory::entry::DirectoryEntry::open(
                starnix_file,
                scope,
                flags,
                path::Path::dot(),
                std::mem::replace(server_end, zx::Handle::invalid().into()),
            );
            return Ok(());
        }

        // Reopen the current file.
        if !path.is_dot() {
            return error!(EINVAL);
        }
        let file = self.reopen(flags)?;
        flags
            .to_object_request(std::mem::replace(server_end, zx::Handle::invalid().into()))
            .handle(|object_request| {
                object_request.spawn_connection(scope, file, flags, file::RawIoConnection::create)
            });
        Ok(())
    }

    /// Implementation of `vfs::directory::entry_container::Directory::get_attrs` and
    /// `vfs::File::get_attrs`.
    fn get_attrs(&self) -> fio::NodeAttributes {
        let info = self.file.node().info();

        // This cast is necessary depending on the architecture.
        #[allow(clippy::unnecessary_cast)]
        let link_count = info.link_count as u64;

        fio::NodeAttributes {
            mode: info.mode.bits(),
            id: self.file.fs.dev_id.bits(),
            content_size: info.size as u64,
            storage_size: info.storage_size() as u64,
            link_count,
            creation_time: info.time_status_change.into_nanos() as u64,
            modification_time: info.time_modify.into_nanos() as u64,
        }
    }

    fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> fio::NodeAttributes2 {
        let info = self.file.node().info();

        // This cast is necessary depending on the architecture.
        #[allow(clippy::unnecessary_cast)]
        let link_count = info.link_count as u64;

        let (protocols, abilities) = if info.mode.contains(FileMode::IFDIR) {
            (
                fio::NodeProtocolKinds::DIRECTORY,
                fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE
                    | fio::Operations::MODIFY_DIRECTORY,
            )
        } else {
            (
                fio::NodeProtocolKinds::FILE,
                fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
            )
        };

        attributes!(
            requested_attributes,
            Mutable {
                creation_time: info.time_status_change.into_nanos() as u64,
                modification_time: info.time_modify.into_nanos() as u64,
                mode: info.mode.bits(),
                uid: info.uid,
                gid: info.gid,
                rdev: info.rdev.bits(),
            },
            Immutable {
                protocols: protocols,
                abilities: abilities,
                content_size: info.size as u64,
                storage_size: info.storage_size() as u64,
                link_count: link_count,
                id: self.file.fs.dev_id.bits(),
            }
        )
    }

    /// Implementation of `vfs::directory::entry_container::MutableDirectory::set_attrs` and
    /// `vfs::File::set_attrs`.
    fn set_attrs(
        &self,
        flags: fio::NodeAttributeFlags,
        attributes: fio::NodeAttributes,
    ) -> Result<(), Errno> {
        self.file.node().update_info(|info| {
            if flags.contains(fio::NodeAttributeFlags::CREATION_TIME) {
                info.time_status_change = zx::Time::from_nanos(attributes.creation_time as i64);
            }
            if flags.contains(fio::NodeAttributeFlags::MODIFICATION_TIME) {
                info.time_modify = zx::Time::from_nanos(attributes.modification_time as i64);
            }
            Ok(())
        })
    }
}

#[async_trait]
impl vfs::node::Node for StarnixNodeConnection {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Ok(StarnixNodeConnection::get_attrs(self))
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(StarnixNodeConnection::get_attributes(self, requested_attributes))
    }
}

#[async_trait]
impl directory::entry_container::Directory for StarnixNodeConnection {
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a directory::traversal_position::TraversalPosition,
        sink: Box<dyn directory::dirents_sink::Sink>,
    ) -> Result<
        (
            directory::traversal_position::TraversalPosition,
            Box<dyn directory::dirents_sink::Sealed>,
        ),
        zx::Status,
    > {
        StarnixNodeConnection::directory_read_dirents(self, pos, sink).map_err(Errno::into)
    }
    fn register_watcher(
        self: Arc<Self>,
        _scope: execution_scope::ExecutionScope,
        _mask: fio::WatchMask,
        _watcher: directory::entry_container::DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        // TODO: Implement watcher using inotify apis.
        Ok(())
    }
    fn unregister_watcher(self: Arc<Self>, _key: usize) {}
}

#[async_trait]
impl directory::entry_container::MutableDirectory for StarnixNodeConnection {
    async fn set_attrs(
        &self,
        flags: fio::NodeAttributeFlags,
        attributes: fio::NodeAttributes,
    ) -> Result<(), zx::Status> {
        StarnixNodeConnection::set_attrs(self, flags, attributes)?;
        Ok(())
    }
    async fn unlink(
        self: Arc<Self>,
        name: &str,
        must_be_directory: bool,
    ) -> Result<(), zx::Status> {
        let kind = if must_be_directory { UnlinkKind::Directory } else { UnlinkKind::NonDirectory };
        self.file.name.entry.unlink(&self.task, name.as_bytes(), kind, false)?;
        Ok(())
    }
    async fn sync(&self) -> Result<(), zx::Status> {
        Ok(())
    }
    async fn rename(
        self: Arc<Self>,
        src_dir: Arc<dyn directory::entry_container::MutableDirectory>,
        src_name: path::Path,
        dst_name: path::Path,
    ) -> Result<(), zx::Status> {
        let src_name = src_name.into_string();
        let dst_name = dst_name.into_string();
        let src_dir =
            src_dir.into_any().downcast::<StarnixNodeConnection>().map_err(|_| errno!(EXDEV))?;
        let (src_node, src_name) = src_dir.lookup_parent(src_name.as_bytes())?;
        let (dst_node, dst_name) = self.lookup_parent(dst_name.as_bytes())?;
        DirEntry::rename(
            &self.task,
            &src_node,
            src_name,
            &dst_node,
            dst_name,
            RenameFlags::empty(),
        )?;
        Ok(())
    }
}

#[async_trait]
impl file::File for StarnixNodeConnection {
    fn writable(&self) -> bool {
        true
    }
    async fn open_file(&self, _optionss: &file::FileOptions) -> Result<(), zx::Status> {
        Ok(())
    }
    async fn truncate(&self, length: u64) -> Result<(), zx::Status> {
        self.file.name.entry.node.truncate(&self.task, length)?;
        Ok(())
    }
    async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, zx::Status> {
        let mut prot_flags = ProtectionFlags::empty();
        if flags.contains(fio::VmoFlags::READ) {
            prot_flags |= ProtectionFlags::READ;
        }
        if flags.contains(fio::VmoFlags::WRITE) {
            prot_flags |= ProtectionFlags::WRITE;
        }
        if flags.contains(fio::VmoFlags::EXECUTE) {
            prot_flags |= ProtectionFlags::EXEC;
        }
        let vmo = self.file.get_vmo(&self.task, None, prot_flags)?;
        if flags.contains(fio::VmoFlags::PRIVATE_CLONE) {
            let size = vmo.get_size()?;
            vmo.create_child(zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE, 0, size)
        } else {
            vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)
        }
    }

    async fn get_size(&self) -> Result<u64, zx::Status> {
        Ok(self.file.node().info().size as u64)
    }
    async fn set_attrs(
        &self,
        flags: fio::NodeAttributeFlags,
        attributes: fio::NodeAttributes,
    ) -> Result<(), zx::Status> {
        StarnixNodeConnection::set_attrs(self, flags, attributes)?;
        Ok(())
    }
    async fn sync(&self, _mode: file::SyncMode) -> Result<(), zx::Status> {
        Ok(())
    }
}

#[async_trait]
impl file::RawFileIoConnection for StarnixNodeConnection {
    async fn read(&self, count: u64) -> Result<Vec<u8>, zx::Status> {
        let task = self.task.clone();
        let file = self.file.clone();
        Ok(self
            .task
            .kernel()
            .kthreads
            .pool
            .dispatch_and_get_result(move || -> Result<Vec<u8>, Errno> {
                let mut data = VecOutputBuffer::new(count as usize);
                file.read(&task, &mut data)?;
                Ok(data.into())
            })
            .await??)
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status> {
        let task = self.task.clone();
        let file = self.file.clone();
        Ok(self
            .task
            .kernel()
            .kthreads
            .pool
            .dispatch_and_get_result(move || -> Result<Vec<u8>, Errno> {
                let mut data = VecOutputBuffer::new(count as usize);
                file.read_at(&task, offset as usize, &mut data)?;
                Ok(data.into())
            })
            .await??)
    }

    async fn write(&self, content: &[u8]) -> Result<u64, zx::Status> {
        let task = self.task.clone();
        let file = self.file.clone();
        let mut data = VecInputBuffer::new(content);
        let written = self
            .task
            .kernel()
            .kthreads
            .pool
            .dispatch_and_get_result(move || -> Result<usize, Errno> {
                file.write(&task, &mut data)
            })
            .await??;
        Ok(written as u64)
    }
    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, zx::Status> {
        let task = self.task.clone();
        let file = self.file.clone();
        let mut data = VecInputBuffer::new(content);
        let written = self
            .task
            .kernel()
            .kthreads
            .pool
            .dispatch_and_get_result(move || -> Result<usize, Errno> {
                file.write_at(&task, offset as usize, &mut data)
            })
            .await??;
        Ok(written as u64)
    }
    async fn seek(&self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status> {
        let target = match origin {
            fio::SeekOrigin::Start => SeekTarget::Set(offset),
            fio::SeekOrigin::Current => SeekTarget::Cur(offset),
            fio::SeekOrigin::End => SeekTarget::End(offset),
        };
        Ok(self.file.seek(&self.task, target)? as u64)
    }
    fn update_flags(&self, flags: fio::OpenFlags) -> zx::Status {
        let settable_flags =
            OpenFlags::APPEND | OpenFlags::DIRECT | OpenFlags::NOATIME | OpenFlags::NONBLOCK;
        self.file.update_file_flags(flags.into(), settable_flags);
        zx::Status::OK
    }
}

#[async_trait]
impl directory::entry::DirectoryEntry for StarnixNodeConnection {
    fn open(
        self: Arc<Self>,
        scope: execution_scope::ExecutionScope,
        flags: fio::OpenFlags,
        path: path::Path,
        mut server_end: ServerEnd<fio::NodeMarker>,
    ) {
        if let Err(errno) = self.directory_entry_open(scope, flags, path, &mut server_end) {
            vfs::common::send_on_open_with_error(
                flags.contains(fio::OpenFlags::DESCRIBE),
                server_end,
                errno.into(),
            );
        }
    }

    fn entry_info(&self) -> directory::entry::EntryInfo {
        let dirent_type =
            if self.is_dir() { fio::DirentType::Directory } else { fio::DirentType::File };
        directory::entry::EntryInfo::new(0, dirent_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fs::tmpfs::TmpFs, testing::*};
    use std::collections::HashSet;
    use syncio::Zxio;

    fn assert_directory_content(zxio: &Zxio, content: &[&[u8]]) {
        let expected = content.iter().map(|x| x.to_vec()).collect::<HashSet<_>>();
        let mut iterator = zxio.create_dirent_iterator().expect("iterator");
        iterator.rewind().expect("iterator");
        let found =
            iterator.map(|x| x.as_ref().expect("dirent").name.clone()).collect::<HashSet<_>>();
        assert_eq!(found, expected);
    }

    #[::fuchsia::test]
    async fn access_file_system() {
        let (kernel, current_task) = create_kernel_and_task();

        fasync::unblock(move || {
            let fs = TmpFs::new_fs(&kernel);

            let root_handle = serve_file(
                &current_task,
                &fs.root().open_anonymous(&current_task, OpenFlags::RDWR).expect("open"),
            )
            .expect("serve");

            let root_zxio = Zxio::create(root_handle.into_handle()).expect("create");

            assert_directory_content(&root_zxio, &[b"."]);
            // Check that one can reiterate from the start.
            assert_directory_content(&root_zxio, &[b"."]);

            let attrs = root_zxio.attr_get().expect("attr_get");
            assert_eq!(attrs.id, fs.dev_id.bits());

            let mut attrs = syncio::zxio_node_attributes_t::default();
            attrs.has.creation_time = true;
            attrs.has.modification_time = true;
            attrs.creation_time = 0;
            attrs.modification_time = 42;
            root_zxio.attr_set(&attrs).expect("attr_set");
            let attrs = root_zxio.attr_get().expect("attr_get");
            assert_eq!(attrs.creation_time, 0);
            assert_eq!(attrs.modification_time, 42);

            assert_eq!(
                root_zxio
                    .open(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE, "foo")
                    .expect_err("open"),
                zx::Status::NOT_FOUND
            );
            let foo_zxio = root_zxio
                .open(
                    fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::CREATE,
                    "foo",
                )
                .expect("zxio_open");
            assert_directory_content(&root_zxio, &[b".", b"foo"]);

            assert_eq!(foo_zxio.write(b"hello").expect("write"), 5);
            assert_eq!(foo_zxio.write_at(2, b"ch").expect("write_at"), 2);
            let mut buffer = [0; 7];
            assert_eq!(foo_zxio.read_at(2, &mut buffer).expect("read_at"), 3);
            assert_eq!(&buffer[..3], b"cho");
            assert_eq!(foo_zxio.seek(syncio::SeekOrigin::Start, 0).expect("seek"), 0);
            assert_eq!(foo_zxio.read(&mut buffer).expect("read"), 5);
            assert_eq!(&buffer[..5], b"hecho");

            let attrs = foo_zxio.attr_get().expect("attr_get");
            assert_eq!(attrs.id, fs.dev_id.bits());

            let mut attrs = syncio::zxio_node_attributes_t::default();
            attrs.has.creation_time = true;
            attrs.has.modification_time = true;
            attrs.creation_time = 0;
            attrs.modification_time = 42;
            foo_zxio.attr_set(&attrs).expect("attr_set");
            let attrs = foo_zxio.attr_get().expect("attr_get");
            assert_eq!(attrs.creation_time, 0);
            assert_eq!(attrs.modification_time, 42);

            assert_eq!(
                root_zxio
                    .open(
                        fio::OpenFlags::DIRECTORY
                            | fio::OpenFlags::CREATE
                            | fio::OpenFlags::RIGHT_READABLE
                            | fio::OpenFlags::RIGHT_WRITABLE,
                        "bar/baz"
                    )
                    .expect_err("open"),
                zx::Status::NOT_FOUND
            );

            let bar_zxio = root_zxio
                .open(
                    fio::OpenFlags::DIRECTORY
                        | fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE,
                    "bar",
                )
                .expect("open");
            let baz_zxio = root_zxio
                .open(
                    fio::OpenFlags::DIRECTORY
                        | fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE,
                    "bar/baz",
                )
                .expect("open");
            assert_directory_content(&root_zxio, &[b".", b"foo", b"bar"]);
            assert_directory_content(&bar_zxio, &[b".", b"baz"]);

            bar_zxio.rename("baz", &root_zxio, "quz").expect("rename");
            assert_directory_content(&bar_zxio, &[b"."]);
            assert_directory_content(&root_zxio, &[b".", b"foo", b"bar", b"quz"]);
            assert_directory_content(&baz_zxio, &[b"."]);
        })
        .await;
    }
}
