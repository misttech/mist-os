// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::ProtectionFlags;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
use crate::vfs::{
    DirectoryEntryType, DirentSink, FileHandle, FileObject, FsStr, LookupContext, NamespaceNode,
    RenameFlags, SeekTarget, UnlinkKind,
};
use fidl::endpoints::{ClientEnd, ServerEnd};
use fidl::HandleBased;
use fidl_fuchsia_io as fio;
use fuchsia_runtime::UtcInstant;
use futures::future::BoxFuture;
use starnix_logging::{log_error, track_stub};
use starnix_sync::{Locked, Unlocked};
use starnix_types::convert::{FromFidl as _, IntoFidl as _};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{AccessCheck, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::ResolveFlags;
use starnix_uapi::{errno, error, ino_t, off_t};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};
use vfs::directory::entry_container::Directory;
use vfs::directory::{self};
use vfs::{
    attributes, execution_scope, file, path, ObjectRequestRef, ProtocolsExt, ToObjectRequest,
};

/// Returns a handle implementing a fuchsia.io.Node delegating to the given `file`.
pub fn serve_file(
    current_task: &CurrentTask,
    file: &FileObject,
) -> Result<(ClientEnd<fio::NodeMarker>, execution_scope::ExecutionScope), Errno> {
    let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::NodeMarker>();
    let scope = serve_file_at(server_end, current_task, file)?;
    Ok((client_end, scope))
}

pub fn serve_file_at(
    server_end: ServerEnd<fio::NodeMarker>,
    current_task: &CurrentTask,
    file: &FileObject,
) -> Result<execution_scope::ExecutionScope, Errno> {
    let kernel = current_task.kernel().clone();
    let scope = execution_scope::ExecutionScope::new();
    current_task.kernel().kthreads.spawn_future({
        let file = file.weak_handle.upgrade().unwrap();
        let scope = scope.clone();
        async move {
            let open_flags = file.flags();
            let starnix_file = {
                let current_task = SystemTaskRef { kernel: kernel.clone() };
                // Reopen file object to not share state with the given FileObject.
                // TODO(security): Switching to the `system_task` here loses track of the credentials from
                //                 `current_task`. Do we need to retain these credentials?
                let file = match file.name.open(
                    kernel.kthreads.unlocked_for_async().deref_mut(),
                    &current_task,
                    file.flags(),
                    AccessCheck::skip(),
                ) {
                    Ok(file) => file,
                    Err(e) => {
                        log_error!("Unable to reopen file: {e:?}");
                        return;
                    }
                };
                StarnixNodeConnection::new(Arc::downgrade(&kernel), file)
            };
            starnix_file.open(scope.clone(), open_flags.into_fidl(), path::Path::dot(), server_end);
            scope.wait().await;
        }
    });
    Ok(scope)
}

fn to_open_flags(flags: &impl ProtocolsExt) -> OpenFlags {
    let rights = flags.rights().unwrap_or_default();
    let mut open_flags = if rights.contains(fio::Operations::WRITE_BYTES) {
        if rights.contains(fio::Operations::READ_BYTES) {
            OpenFlags::RDWR
        } else {
            OpenFlags::WRONLY
        }
    } else {
        OpenFlags::RDONLY
    };

    if flags.create_directory() {
        open_flags |= OpenFlags::DIRECTORY;
    }

    match flags.creation_mode() {
        vfs::CreationMode::Always => open_flags |= OpenFlags::CREAT | OpenFlags::EXCL,
        vfs::CreationMode::AllowExisting => open_flags |= OpenFlags::CREAT,
        vfs::CreationMode::Never => {}
    };

    if flags.is_truncate() {
        open_flags |= OpenFlags::TRUNC;
    }

    if flags.is_append() {
        open_flags |= OpenFlags::APPEND;
    }

    open_flags
}

/// A representation of `file` for the rust vfs.
///
/// This struct implements the following trait from the rust vfs library:
/// - directory::entry_container::Directory
/// - directory::entry_container::MutableDirectory
/// - file::File
/// - file::RawFileIoConnection
///
/// Each method is delegated back to the starnix vfs, using `task` as the current task. Blocking
/// methods are run from the kernel dynamic thread spawner so that the async dispatched do not
/// block on these.
struct StarnixNodeConnection {
    kernel: Weak<Kernel>,
    file: FileHandle,
}

struct SystemTaskRef {
    kernel: Arc<Kernel>,
}

impl Deref for SystemTaskRef {
    type Target = CurrentTask;

    fn deref(&self) -> &CurrentTask {
        self.kernel.kthreads.system_task()
    }
}

impl StarnixNodeConnection {
    fn new(kernel: Weak<Kernel>, file: FileHandle) -> Arc<Self> {
        Arc::new(StarnixNodeConnection { kernel, file })
    }

    fn kernel(&self) -> Result<Arc<Kernel>, Errno> {
        self.kernel.upgrade().ok_or_else(|| errno!(ESRCH))
    }

    fn task(&self) -> Result<impl Deref<Target = CurrentTask>, Errno> {
        Ok(SystemTaskRef { kernel: self.kernel()? })
    }

    fn is_dir(&self) -> bool {
        self.file.node().is_dir()
    }

    /// Reopen the current `StarnixNodeConnection` with the given `OpenFlags`. The new file will not share
    /// state. It is equivalent to opening the same file, not dup'ing the file descriptor.
    fn reopen(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        flags: &impl ProtocolsExt,
    ) -> Result<Arc<Self>, Errno> {
        let file = self.file.name.open(
            locked,
            current_task,
            to_open_flags(flags),
            AccessCheck::default(),
        )?;
        Ok(StarnixNodeConnection::new(self.kernel.clone(), file))
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
        let current_task = self.task()?;
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
                if name != ".." {
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
        }
        let offset = match pos {
            directory::traversal_position::TraversalPosition::Start => 0,
            directory::traversal_position::TraversalPosition::Name(_) => return error!(EINVAL),
            directory::traversal_position::TraversalPosition::Bytes(_) => return error!(EINVAL),
            directory::traversal_position::TraversalPosition::Index(v) => *v as i64,
            directory::traversal_position::TraversalPosition::End => {
                return Ok((directory::traversal_position::TraversalPosition::End, sink.seal()));
            }
        };
        let kernel = self.kernel().unwrap();
        if *self.file.offset.lock() != offset {
            self.file.seek(
                kernel.kthreads.unlocked_for_async().deref_mut(),
                &current_task,
                SeekTarget::Set(offset),
            )?;
        }
        let mut file_offset = self.file.offset.lock();
        let mut dirent_sink = DirentSinkAdapter {
            sink: Some(directory::dirents_sink::AppendResult::Ok(sink)),
            offset: &mut file_offset,
        };
        self.file.readdir(
            kernel.kthreads.unlocked_for_async().deref_mut(),
            &current_task,
            &mut dirent_sink,
        )?;
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

    fn lookup_parent<'a>(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        self.task()?.lookup_parent(locked, &mut LookupContext::default(), &self.file.name, path)
    }

    /// Implementation of `vfs::directory::entry::DirectoryEntry::open`.
    fn directory_entry_open(
        self: Arc<Self>,
        scope: execution_scope::ExecutionScope,
        flags: impl ProtocolsExt,
        path: path::Path,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        let current_task = self.task()?;
        let kernel = self.kernel().unwrap();
        if self.is_dir() {
            if path.is_dot() {
                // Reopen the current directory.
                let dir = self.reopen(
                    kernel.kthreads.unlocked_for_async().deref_mut(),
                    &current_task,
                    &flags,
                )?;
                return object_request.spawn_connection(
                    scope,
                    dir,
                    flags,
                    directory::mutable::connection::MutableConnection::create,
                );
            }

            // Open a path under the current directory.
            let mut locked = kernel.kthreads.unlocked_for_async();
            let path = path.into_string();
            let (node, name) = self.lookup_parent(&mut locked, path.as_bytes().into())?;
            let create_directory = flags.creation_mode() != vfs::common::CreationMode::Never
                && flags.create_directory();
            let open_flags = to_open_flags(&flags);
            let file = match current_task.open_namespace_node_at(
                &mut locked,
                node.clone(),
                name,
                open_flags,
                FileMode::ALLOW_ALL,
                ResolveFlags::empty(),
                AccessCheck::default(),
            ) {
                Err(e) if e == errno!(EISDIR) && create_directory => {
                    let mode =
                        current_task.fs().apply_umask(FileMode::from_bits(0o777) | FileMode::IFDIR);
                    let name =
                        node.create_node(&mut locked, &current_task, name, mode, DeviceType::NONE)?;
                    name.open(
                        &mut locked,
                        &current_task,
                        open_flags & !(OpenFlags::CREAT | OpenFlags::EXCL),
                        AccessCheck::skip(),
                    )?
                }
                f => f?,
            };
            std::mem::drop(locked);

            let starnix_file = StarnixNodeConnection::new(self.kernel.clone(), file);
            return starnix_file.directory_entry_open(
                scope,
                flags,
                path::Path::dot(),
                object_request,
            );
        }

        // Reopen the current file.
        if !path.is_dot() {
            return Err(zx::Status::NOT_DIR);
        }
        let file =
            self.reopen(kernel.kthreads.unlocked_for_async().deref_mut(), &current_task, &flags)?;
        object_request.spawn_connection(scope, file, flags, file::RawIoConnection::create)
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

    fn update_attributes(&self, attributes: fio::MutableNodeAttributes) {
        self.file.node().update_info(|info| {
            if let Some(time) = attributes.creation_time {
                info.time_status_change = UtcInstant::from_nanos(time as i64);
            }
            if let Some(time) = attributes.modification_time {
                info.time_modify = UtcInstant::from_nanos(time as i64);
            }
            if let Some(mode) = attributes.mode {
                info.mode = FileMode::from_bits(mode);
            }
            if let Some(uid) = attributes.uid {
                info.uid = uid;
            }
            if let Some(gid) = attributes.gid {
                info.gid = gid;
            }
            if let Some(rdev) = attributes.rdev {
                info.rdev = DeviceType::from_bits(rdev);
            }
        });
    }
}

impl vfs::node::Node for StarnixNodeConnection {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(StarnixNodeConnection::get_attributes(self, requested_attributes))
    }
}

impl directory::entry::GetEntryInfo for StarnixNodeConnection {
    fn entry_info(&self) -> directory::entry::EntryInfo {
        let dirent_type =
            if self.is_dir() { fio::DirentType::Directory } else { fio::DirentType::File };
        directory::entry::EntryInfo::new(0, dirent_type)
    }
}

impl directory::entry_container::Directory for StarnixNodeConnection {
    fn open(
        self: Arc<Self>,
        scope: execution_scope::ExecutionScope,
        flags: fio::OpenFlags,
        path: path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags
            .to_object_request(server_end)
            .handle(|object_request| self.directory_entry_open(scope, flags, path, object_request));
    }

    fn open3(
        self: Arc<Self>,
        scope: execution_scope::ExecutionScope,
        path: path::Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        self.directory_entry_open(scope, flags, path, object_request)
    }

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
        track_stub!(TODO("https://fxbug.dev/322875605"), "register directory watcher");
        Ok(())
    }
    fn unregister_watcher(self: Arc<Self>, _key: usize) {}
}

impl directory::entry_container::MutableDirectory for StarnixNodeConnection {
    async fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        StarnixNodeConnection::update_attributes(self, attributes);
        Ok(())
    }
    async fn unlink(
        self: Arc<Self>,
        name: &str,
        must_be_directory: bool,
    ) -> Result<(), zx::Status> {
        let kind = if must_be_directory { UnlinkKind::Directory } else { UnlinkKind::NonDirectory };
        let current_task = self.task()?;
        self.file.name.entry.unlink(
            self.kernel().unwrap().kthreads.unlocked_for_async().deref_mut(),
            &current_task,
            &self.file.name.mount,
            name.into(),
            kind,
            false,
        )?;
        Ok(())
    }
    async fn sync(&self) -> Result<(), zx::Status> {
        Ok(())
    }
    fn rename(
        self: Arc<Self>,
        src_dir: Arc<dyn directory::entry_container::MutableDirectory>,
        src_name: path::Path,
        dst_name: path::Path,
    ) -> BoxFuture<'static, Result<(), zx::Status>> {
        Box::pin(async move {
            let kernel = self.kernel().unwrap();
            let mut locked = kernel.kthreads.unlocked_for_async();
            let src_name = src_name.into_string();
            let dst_name = dst_name.into_string();
            let src_dir = src_dir
                .into_any()
                .downcast::<StarnixNodeConnection>()
                .map_err(|_| errno!(EXDEV))?;
            let (src_node, src_name) =
                src_dir.lookup_parent(&mut locked, src_name.as_str().into())?;
            let (dst_node, dst_name) = self.lookup_parent(&mut locked, dst_name.as_str().into())?;
            NamespaceNode::rename(
                &mut locked,
                &*self.task()?,
                &src_node,
                src_name,
                &dst_node,
                dst_name,
                RenameFlags::empty(),
            )?;
            Ok(())
        })
    }
}

impl file::File for StarnixNodeConnection {
    fn writable(&self) -> bool {
        true
    }
    async fn open_file(&self, _optionss: &file::FileOptions) -> Result<(), zx::Status> {
        Ok(())
    }
    async fn truncate(&self, length: u64) -> Result<(), zx::Status> {
        let current_task = self.task()?;
        self.file.name.truncate(
            self.kernel().unwrap().kthreads.unlocked_for_async().deref_mut(),
            &current_task,
            length,
        )?;
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
        let current_task = &*self.task()?;
        let memory = self.file.get_memory(
            self.kernel().unwrap().kthreads.unlocked_for_async().deref_mut(),
            current_task,
            None,
            prot_flags,
        )?;
        let vmo = memory.as_vmo().ok_or_else(|| errno!(ENOTSUP))?;
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
    async fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        StarnixNodeConnection::update_attributes(self, attributes);
        Ok(())
    }
    async fn sync(&self, _mode: file::SyncMode) -> Result<(), zx::Status> {
        Ok(())
    }
}

impl file::RawFileIoConnection for StarnixNodeConnection {
    async fn read(&self, count: u64) -> Result<Vec<u8>, zx::Status> {
        let file = self.file.clone();
        let kernel = self.kernel()?;
        Ok(kernel
            .kthreads
            .spawner()
            .spawn_and_get_result(move |locked, current_task| -> Result<Vec<u8>, Errno> {
                let mut data = VecOutputBuffer::new(count as usize);
                file.read(locked, current_task, &mut data)?;
                Ok(data.into())
            })
            .await??)
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status> {
        let file = self.file.clone();
        let kernel = self.kernel()?;
        Ok(kernel
            .kthreads
            .spawner()
            .spawn_and_get_result(move |locked, current_task| -> Result<Vec<u8>, Errno> {
                let mut data = VecOutputBuffer::new(count as usize);
                file.read_at(locked, current_task, offset as usize, &mut data)?;
                Ok(data.into())
            })
            .await??)
    }

    async fn write(&self, content: &[u8]) -> Result<u64, zx::Status> {
        let file = self.file.clone();
        let kernel = self.kernel()?;
        let mut data = VecInputBuffer::new(content);
        let written = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result(move |locked, current_task| -> Result<usize, Errno> {
                file.write(locked, current_task, &mut data)
            })
            .await??;
        Ok(written as u64)
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, zx::Status> {
        let file = self.file.clone();
        let kernel = self.kernel()?;
        let mut data = VecInputBuffer::new(content);
        let written = kernel
            .kthreads
            .spawner()
            .spawn_and_get_result(move |locked, current_task| -> Result<usize, Errno> {
                file.write_at(locked, current_task, offset as usize, &mut data)
            })
            .await??;
        Ok(written as u64)
    }

    async fn seek(&self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status> {
        let kernel = self.kernel()?;
        let target = match origin {
            fio::SeekOrigin::Start => SeekTarget::Set(offset),
            fio::SeekOrigin::Current => SeekTarget::Cur(offset),
            fio::SeekOrigin::End => SeekTarget::End(offset),
        };
        let seek_result = self.file.seek(
            kernel.kthreads.unlocked_for_async().deref_mut(),
            &*self.task()?,
            target,
        )?;
        Ok(seek_result as u64)
    }

    fn update_flags(&self, flags: fio::OpenFlags) -> zx::Status {
        let flags = OpenFlags::from_fidl(flags);

        // `fcntl(F_SETFL)` also supports setting `O_NOATIME`, but it's allowed
        // only when the calling process has the same EUID as the UID of the
        // file. We don't know caller's EUID here, so we cannot check if
        // O_NOATIME should be allowed. It's not a problem here because
        // `fidl::fuchsia::io::OpenFlags` doesn't have an equivalent of
        // `O_NOATIME`.
        assert!(!flags.contains(OpenFlags::NOATIME));

        let settable_flags = OpenFlags::APPEND | OpenFlags::DIRECT | OpenFlags::NONBLOCK;
        self.file.update_file_flags(flags, settable_flags);

        zx::Status::OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::tmpfs::TmpFs;
    use crate::testing::*;
    use crate::vfs::FsString;
    use fuchsia_async as fasync;
    use std::collections::HashSet;
    use syncio::{zxio_node_attr_has_t, Zxio, ZxioOpenOptions};

    fn assert_directory_content(zxio: &Zxio, content: &[&[u8]]) {
        let expected = content.iter().map(|&x| FsString::from(x)).collect::<HashSet<_>>();
        let mut iterator = zxio.create_dirent_iterator().expect("iterator");
        iterator.rewind().expect("iterator");
        let found =
            iterator.map(|x| x.as_ref().expect("dirent").name.clone()).collect::<HashSet<_>>();
        assert_eq!(found, expected);
    }

    #[::fuchsia::test]
    async fn access_file_system() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let fs = TmpFs::new_fs(&kernel);

        let file =
            &fs.root().open_anonymous(&mut locked, &current_task, OpenFlags::RDWR).expect("open");
        let (root_handle, scope) = serve_file(&current_task, file).expect("serve");

        // Capture information from the filesystem in the main thread. The filesystem must not be
        // transferred to the other thread.
        let fs_dev_id = fs.dev_id;
        fasync::unblock(move || {
            let root_zxio = Zxio::create(root_handle.into_handle()).expect("create");

            assert_directory_content(&root_zxio, &[b"."]);
            // Check that one can reiterate from the start.
            assert_directory_content(&root_zxio, &[b"."]);

            let attrs = root_zxio
                .attr_get(zxio_node_attr_has_t { id: true, ..Default::default() })
                .expect("attr_get");
            assert_eq!(attrs.id, fs_dev_id.bits());

            let mut attrs = syncio::zxio_node_attributes_t::default();
            attrs.has.creation_time = true;
            attrs.has.modification_time = true;
            attrs.creation_time = 0;
            attrs.modification_time = 42;
            root_zxio.attr_set(&attrs).expect("attr_set");
            let attrs = root_zxio
                .attr_get(zxio_node_attr_has_t {
                    creation_time: true,
                    modification_time: true,
                    ..Default::default()
                })
                .expect("attr_get");
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

            let attrs = foo_zxio
                .attr_get(zxio_node_attr_has_t { id: true, ..Default::default() })
                .expect("attr_get");
            assert_eq!(attrs.id, fs_dev_id.bits());

            let mut attrs = syncio::zxio_node_attributes_t::default();
            attrs.has.creation_time = true;
            attrs.has.modification_time = true;
            attrs.creation_time = 0;
            attrs.modification_time = 42;
            foo_zxio.attr_set(&attrs).expect("attr_set");
            let attrs = foo_zxio
                .attr_get(zxio_node_attr_has_t {
                    creation_time: true,
                    modification_time: true,
                    ..Default::default()
                })
                .expect("attr_get");
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
        scope.shutdown();
        // Yield once to let task in scope in the current thread finish.
        futures_lite::future::yield_now().await;
        scope.wait().await;
        // This ensures fs cannot be captures in the thread.
        std::mem::drop(fs);
    }

    #[::fuchsia::test]
    async fn open3() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let fs = TmpFs::new_fs(&kernel);

        let file = &fs
            .root()
            .open_anonymous(&mut locked, &current_task, OpenFlags::RDWR)
            .expect("open_anonymous failed");
        let (root_handle, scope) = serve_file(&current_task, file).expect("serve_file failed");

        fasync::unblock(move || {
            let root_zxio = Zxio::create(root_handle.into_handle()).expect("zxio create failed");

            assert_directory_content(&root_zxio, &[b"."]);
            assert_eq!(
                root_zxio
                    .open3(
                        "foo",
                        fio::Flags::PERM_READ | fio::Flags::PERM_WRITE,
                        ZxioOpenOptions::default()
                    )
                    .expect_err("open3 passed unexpectedly"),
                zx::Status::NOT_FOUND
            );
            root_zxio
                .open3(
                    "foo",
                    fio::Flags::PROTOCOL_FILE
                        | fio::Flags::PERM_READ
                        | fio::Flags::PERM_WRITE
                        | fio::Flags::FLAG_MUST_CREATE,
                    ZxioOpenOptions::default(),
                )
                .expect("open3 failed");
            assert_directory_content(&root_zxio, &[b".", b"foo"]);

            assert_eq!(
                root_zxio
                    .open3(
                        "bar/baz",
                        fio::Flags::PROTOCOL_DIRECTORY
                            | fio::Flags::PERM_READ
                            | fio::Flags::PERM_WRITE
                            | fio::Flags::FLAG_MUST_CREATE,
                        ZxioOpenOptions::default()
                    )
                    .expect_err("open3 passed unexpectedly"),
                zx::Status::NOT_FOUND
            );
            let bar_zxio = root_zxio
                .open3(
                    "bar",
                    fio::Flags::PROTOCOL_DIRECTORY
                        | fio::Flags::PERM_READ
                        | fio::Flags::PERM_WRITE
                        | fio::Flags::FLAG_MUST_CREATE,
                    ZxioOpenOptions::default(),
                )
                .expect("open3 failed");
            root_zxio
                .open3(
                    "bar/baz",
                    fio::Flags::PROTOCOL_DIRECTORY
                        | fio::Flags::PERM_READ
                        | fio::Flags::PERM_WRITE
                        | fio::Flags::FLAG_MUST_CREATE,
                    ZxioOpenOptions::default(),
                )
                .expect("open3 failed");
            assert_directory_content(&root_zxio, &[b".", b"foo", b"bar"]);
            assert_directory_content(&bar_zxio, &[b".", b"baz"]);
        })
        .await;
        scope.shutdown();
        // Yield once to let task in scope in the current thread finish.
        futures_lite::future::yield_now().await;
        scope.wait().await;

        // This ensures fs cannot be captured in the thread.
        std::mem::drop(fs);
    }
}
