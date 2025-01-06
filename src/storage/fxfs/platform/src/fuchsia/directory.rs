// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fuchsia::device::BlockServer;
use crate::fuchsia::errors::map_to_status;
use crate::fuchsia::file::FxFile;
use crate::fuchsia::node::{FxNode, GetResult, OpenedNode};
use crate::fuchsia::symlink::FxSymlink;
use crate::fuchsia::volume::{info_to_filesystem_info, FxVolume, RootDir};
use anyhow::{bail, Error};
use either::{Left, Right};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use futures::future::BoxFuture;
use futures::FutureExt;
use fxfs::errors::FxfsError;
use fxfs::filesystem::SyncOptions;
use fxfs::log::*;
use fxfs::object_store::directory::{self, ReplacedChild};
use fxfs::object_store::transaction::{lock_keys, LockKey, Options, Transaction};
use fxfs::object_store::{self, Directory, ObjectDescriptor, ObjectStore, Timestamp};
use fxfs_macros::ToWeakNode;
use std::any::Any;
use std::sync::{Arc, Mutex};
use vfs::directory::dirents_sink::{self, AppendResult, Sink};
use vfs::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use vfs::directory::entry_container::{
    Directory as VfsDirectory, DirectoryWatcher, MutableDirectory,
};
use vfs::directory::mutable::connection::MutableConnection;
use vfs::directory::traversal_position::TraversalPosition;
use vfs::directory::watchers::event_producers::SingleNameEventProducer;
use vfs::directory::watchers::Watchers;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::{attributes, symlink, ObjectRequest, ObjectRequestRef, ProtocolsExt, ToObjectRequest};

#[derive(ToWeakNode)]
pub struct FxDirectory {
    // The root directory is the only directory which has no parent, and its parent can never
    // change, hence the Option can go on the outside.
    parent: Option<Mutex<Arc<FxDirectory>>>,
    directory: object_store::Directory<FxVolume>,
    watchers: Mutex<Watchers>,
}

impl RootDir for FxDirectory {
    fn as_directory_entry(self: Arc<Self>) -> Arc<dyn DirectoryEntry> {
        self
    }

    fn as_directory(self: Arc<Self>) -> Arc<dyn VfsDirectory> {
        self
    }

    fn as_node(self: Arc<Self>) -> Arc<dyn FxNode> {
        self as Arc<dyn FxNode>
    }
}

impl FxDirectory {
    pub(super) fn new(
        parent: Option<Arc<FxDirectory>>,
        directory: object_store::Directory<FxVolume>,
    ) -> Self {
        Self {
            parent: parent.map(|p| Mutex::new(p)),
            directory,
            watchers: Mutex::new(Watchers::new()),
        }
    }

    pub fn directory(&self) -> &object_store::Directory<FxVolume> {
        &self.directory
    }

    pub fn volume(&self) -> &Arc<FxVolume> {
        self.directory.owner()
    }

    pub fn store(&self) -> &ObjectStore {
        self.directory.store()
    }

    pub fn is_deleted(&self) -> bool {
        self.directory.is_deleted()
    }

    pub fn set_deleted(&self) {
        self.directory.set_deleted();
        self.watchers.lock().unwrap().send_event(&mut SingleNameEventProducer::deleted());
    }

    async fn lookup(
        self: &Arc<Self>,
        protocols: &dyn ProtocolsExt,
        mut path: Path,
        request: &ObjectRequest,
    ) -> Result<OpenedNode<dyn FxNode>, Error> {
        if path.is_empty() {
            return Ok(OpenedNode::new(self.clone()));
        }
        let store = self.store();
        let fs = store.filesystem();
        let mut current_node = self.clone() as Arc<dyn FxNode>;
        loop {
            let last_segment = path.is_single_component();
            let current_dir =
                current_node.into_any().downcast::<FxDirectory>().map_err(|_| FxfsError::NotDir)?;
            let name = path.next().unwrap();

            // Create the transaction here if we might need to create the object so that we have a
            // lock in place.
            let keys = lock_keys![LockKey::object(
                store.store_object_id(),
                current_dir.directory.object_id()
            )];
            let transaction_or_guard =
                if last_segment && protocols.creation_mode() != vfs::CreationMode::Never {
                    Left(fs.clone().new_transaction(keys, Options::default()).await?)
                } else {
                    // When child objects are created, the object is created along with the
                    // directory entry in the same transaction, and so we need to hold a read lock
                    // over the lookup and open calls.
                    Right(fs.lock_manager().read_lock(keys).await)
                };

            let child_descriptor = match self
                .directory
                .owner()
                .dirent_cache()
                .lookup(&(current_dir.object_id(), name))
            {
                Some(node) => {
                    let desc = node.object_descriptor();
                    Some((node, desc))
                }
                None => {
                    if let Some((object_id, object_descriptor)) =
                        current_dir.directory.lookup(name).await?
                    {
                        let child_node = self
                            .volume()
                            .get_or_load_node(
                                object_id,
                                object_descriptor.clone(),
                                Some(current_dir.clone()),
                            )
                            .await?;

                        self.directory.owner().dirent_cache().insert(
                            current_dir.object_id(),
                            name.to_owned(),
                            child_node.clone(),
                        );
                        Some((child_node, object_descriptor))
                    } else {
                        None
                    }
                }
            };

            match child_descriptor {
                Some((child_node, object_descriptor)) => {
                    if transaction_or_guard.is_left()
                        && protocols.creation_mode() == vfs::CreationMode::Always
                    {
                        bail!(FxfsError::AlreadyExists);
                    }
                    if last_segment {
                        match object_descriptor {
                            ObjectDescriptor::Directory => {
                                if !protocols.is_node() && !protocols.is_dir_allowed() {
                                    if protocols.is_file_allowed() {
                                        bail!(FxfsError::NotFile)
                                    } else {
                                        bail!(FxfsError::WrongType)
                                    }
                                }
                            }
                            ObjectDescriptor::File => {
                                if !protocols.is_node() && !protocols.is_file_allowed() {
                                    if protocols.is_dir_allowed() {
                                        bail!(FxfsError::NotDir)
                                    } else {
                                        bail!(FxfsError::WrongType)
                                    }
                                }
                            }
                            ObjectDescriptor::Symlink => {
                                if !protocols.is_node() && !protocols.is_symlink_allowed() {
                                    bail!(FxfsError::WrongType)
                                }
                            }
                            ObjectDescriptor::Volume => bail!(FxfsError::Inconsistent),
                        }
                    }
                    current_node = child_node;
                    if last_segment {
                        // We must make sure to take an open-count whilst we are holding a read
                        // lock.
                        return Ok(OpenedNode::new(current_node));
                    }
                }
                None => {
                    if let Left(mut transaction) = transaction_or_guard {
                        let new_node = current_dir
                            .create_child(
                                &mut transaction,
                                name,
                                protocols.create_directory(),
                                request.create_attributes(),
                            )
                            .await?;
                        let node = OpenedNode::new(new_node.clone());
                        if let GetResult::Placeholder(p) =
                            self.volume().cache().get_or_reserve(node.object_id()).await
                        {
                            transaction
                                .commit_with_callback(|_| {
                                    p.commit(&node);
                                    current_dir.did_add(name, Some(new_node));
                                })
                                .await?;
                            return Ok(node);
                        } else {
                            // We created a node, but the object ID was already used in the cache,
                            // which suggests a object ID was reused (which would either be a bug or
                            // corruption).
                            bail!(FxfsError::Inconsistent);
                        }
                    } else {
                        bail!(FxfsError::NotFound);
                    }
                }
            };
        }
    }

    async fn create_child(
        self: &Arc<Self>,
        transaction: &mut Transaction<'_>,
        name: &str,
        create_dir: bool, // If false, creates a file.
        create_attributes: Option<&fio::MutableNodeAttributes>,
    ) -> Result<Arc<dyn FxNode>, Error> {
        if create_dir {
            let dir = Arc::new(FxDirectory::new(
                Some(self.clone()),
                self.directory.create_child_dir(transaction, name).await?,
            ));
            if let Some(attrs) = create_attributes {
                dir.directory().handle().update_attributes(transaction, Some(&attrs), None).await?;
            }
            Ok(dir as Arc<dyn FxNode>)
        } else {
            let file = FxFile::new(self.directory.create_child_file(transaction, name).await?);
            if let Some(attrs) = create_attributes {
                file.handle()
                    .uncached_handle()
                    .update_attributes(transaction, Some(&attrs), None)
                    .await?;
            }
            Ok(file as Arc<dyn FxNode>)
        }
    }

    /// Called to indicate a file or directory was removed from this directory.
    pub(crate) fn did_remove(&self, name: &str) {
        self.directory.owner().dirent_cache().remove(&(self.directory.object_id(), name));
        self.watchers.lock().unwrap().send_event(&mut SingleNameEventProducer::removed(name));
    }

    /// Called to indicate a file or directory was added to this directory.
    pub(crate) fn did_add(&self, name: &str, node: Option<Arc<dyn FxNode>>) {
        if let Some(node) = node {
            self.directory.owner().dirent_cache().insert(
                self.directory.object_id(),
                name.to_owned(),
                node,
            );
        }
        self.watchers.lock().unwrap().send_event(&mut SingleNameEventProducer::added(name));
    }

    pub(crate) async fn link_object(
        &self,
        mut transaction: Transaction<'_>,
        name: &str,
        source_id: u64,
        kind: ObjectDescriptor,
    ) -> Result<(), zx::Status> {
        let store = self.store();
        if self.is_deleted() {
            return Err(zx::Status::ACCESS_DENIED);
        }
        if self.directory.lookup(&name).await.map_err(map_to_status)?.is_some() {
            return Err(zx::Status::ALREADY_EXISTS);
        }
        self.directory
            .insert_child(&mut transaction, &name, source_id, kind.clone())
            .await
            .map_err(map_to_status)?;
        store.adjust_refs(&mut transaction, source_id, 1).await.map_err(map_to_status)?;
        transaction
            .commit_with_callback(|_| self.did_add(&name, None))
            .await
            .map_err(map_to_status)?;
        Ok(())
    }

    async fn link_impl(
        self: Arc<Self>,
        name: String,
        source_dir: Arc<dyn Any + Send + Sync>,
        source_name: &str,
    ) -> Result<(), zx::Status> {
        let source_dir = source_dir.downcast::<Self>().unwrap();
        let store = self.store();
        let fs = store.filesystem().clone();
        let source_id =
            match source_dir.directory.lookup(source_name).await.map_err(map_to_status)? {
                Some((object_id, ObjectDescriptor::File)) => object_id,
                None => return Err(zx::Status::NOT_FOUND),
                _ => return Err(zx::Status::NOT_SUPPORTED),
            };
        // We don't need a lock on the source directory, as it will be unchanged (unless it is the
        // same as the destination directory). We just need a lock on the source object to ensure
        // that it hasn't been simultaneously unlinked. We need that lock anyway to update the ref
        // count.
        let transaction = fs
            .new_transaction(
                lock_keys![
                    LockKey::object(store.store_object_id(), self.object_id()),
                    LockKey::object(store.store_object_id(), source_id),
                ],
                Options::default(),
            )
            .await
            .map_err(map_to_status)?;
        // Ensure under lock that the file still exists there.
        match source_dir.directory.lookup(source_name).await.map_err(map_to_status)? {
            Some((_, ObjectDescriptor::File)) => {}
            None => return Err(zx::Status::NOT_FOUND),
            _ => return Err(zx::Status::NOT_SUPPORTED),
        };
        self.link_object(transaction, &name, source_id, ObjectDescriptor::File).await
    }

    async fn rename_impl(
        self: Arc<Self>,
        src_dir: Arc<dyn MutableDirectory>,
        src_name: Path,
        dst_name: Path,
    ) -> Result<(), zx::Status> {
        if !src_name.is_single_component() || !dst_name.is_single_component() {
            return Err(zx::Status::INVALID_ARGS);
        }
        let (src, dst) = (src_name.peek().unwrap(), dst_name.peek().unwrap());
        let src_dir =
            src_dir.into_any().downcast::<FxDirectory>().map_err(|_| Err(zx::Status::NOT_DIR))?;

        // Acquire the transaction that locks |src_dir|, |src_name|, |self|, and |dst_name| if they
        // exist, and also the ID and type of dst and src.
        let replace_context = self
            .directory
            .acquire_context_for_replace(Some((src_dir.directory(), src)), dst, false)
            .await
            .map_err(map_to_status)?;
        let mut transaction = replace_context.transaction;

        if self.is_deleted() {
            return Err(zx::Status::NOT_FOUND);
        }

        let (moved_id, moved_descriptor) =
            replace_context.src_id_and_descriptor.ok_or(zx::Status::NOT_FOUND)?;

        // Make sure the dst path is compatible with the moved node.
        if let ObjectDescriptor::File = moved_descriptor {
            if src_name.is_dir() || dst_name.is_dir() {
                return Err(zx::Status::NOT_DIR);
            }
        }

        // Now that we've ensured that the dst path is compatible with the moved node, we can check
        // for the trivial case.
        if src_dir.object_id() == self.object_id() && src == dst {
            return Ok(());
        }

        if let Some((_, dst_descriptor)) = replace_context.dst_id_and_descriptor.as_ref() {
            // dst is being overwritten; make sure it's a file iff src is.
            match (&moved_descriptor, dst_descriptor) {
                (ObjectDescriptor::Directory, ObjectDescriptor::Directory) => {}
                (
                    ObjectDescriptor::File | ObjectDescriptor::Symlink,
                    ObjectDescriptor::File | ObjectDescriptor::Symlink,
                ) => {}
                (ObjectDescriptor::Directory, _) => return Err(zx::Status::NOT_DIR),
                (ObjectDescriptor::File | ObjectDescriptor::Symlink, _) => {
                    return Err(zx::Status::NOT_FILE)
                }
                _ => return Err(zx::Status::IO_DATA_INTEGRITY),
            }
        }

        let moved_node = src_dir
            .volume()
            .get_or_load_node(moved_id, moved_descriptor.clone(), Some(src_dir.clone()))
            .await
            .map_err(map_to_status)?;

        if let ObjectDescriptor::Directory = moved_descriptor {
            // Lastly, ensure that self isn't a (transitive) child of the moved node.
            let mut node_opt = Some(self.clone());
            while let Some(node) = node_opt {
                if node.object_id() == moved_node.object_id() {
                    return Err(zx::Status::INVALID_ARGS);
                }
                node_opt = node.parent();
            }
        }

        let replace_result = directory::replace_child(
            &mut transaction,
            Some((src_dir.directory(), src)),
            (self.directory(), dst),
        )
        .await
        .map_err(map_to_status)?;

        transaction
            .commit_with_callback(|_| {
                moved_node.set_parent(self.clone());
                src_dir.did_remove(src);

                match replace_result {
                    ReplacedChild::None => {}
                    ReplacedChild::ObjectWithRemainingLinks(..) | ReplacedChild::Object(_) => {
                        self.did_remove(dst);
                    }
                    ReplacedChild::Directory(id) => {
                        self.did_remove(dst);
                        self.volume().mark_directory_deleted(id);
                    }
                }
                self.did_add(dst, Some(moved_node));
            })
            .await
            .map_err(map_to_status)?;

        if let ReplacedChild::Object(id) = replace_result {
            self.volume().maybe_purge_file(id).await.map_err(map_to_status)?;
        }
        Ok(())
    }
}

impl Drop for FxDirectory {
    fn drop(&mut self) {
        self.volume().cache().remove(self);
    }
}

impl FxNode for FxDirectory {
    fn object_id(&self) -> u64 {
        self.directory.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        self.parent.as_ref().map(|p| p.lock().unwrap().clone())
    }

    fn set_parent(&self, parent: Arc<FxDirectory>) {
        match &self.parent {
            Some(p) => *p.lock().unwrap() = parent,
            None => panic!("Called set_parent on root node"),
        }
    }

    // If these ever do anything, BlobDirectory might need to be fixed.
    fn open_count_add_one(&self) {}
    fn open_count_sub_one(self: Arc<Self>) {}

    fn object_descriptor(&self) -> ObjectDescriptor {
        ObjectDescriptor::Directory
    }
}

impl MutableDirectory for FxDirectory {
    fn link<'a>(
        self: Arc<Self>,
        name: String,
        source_dir: Arc<dyn Any + Send + Sync>,
        source_name: &'a str,
    ) -> BoxFuture<'a, Result<(), zx::Status>> {
        Box::pin(self.link_impl(name, source_dir, source_name))
    }

    async fn unlink(
        self: Arc<Self>,
        name: &str,
        must_be_directory: bool,
    ) -> Result<(), zx::Status> {
        let replace_context = self
            .directory
            .acquire_context_for_replace(None, name, true)
            .await
            .map_err(map_to_status)?;
        let mut transaction = replace_context.transaction;
        let object_descriptor = match replace_context.dst_id_and_descriptor {
            Some((_, object_descriptor)) => object_descriptor,
            None => return Err(zx::Status::NOT_FOUND),
        };
        if let ObjectDescriptor::Directory = object_descriptor {
        } else if must_be_directory {
            return Err(zx::Status::NOT_DIR);
        }
        match directory::replace_child(&mut transaction, None, (self.directory(), name))
            .await
            .map_err(map_to_status)?
        {
            ReplacedChild::None => return Err(zx::Status::NOT_FOUND),
            ReplacedChild::ObjectWithRemainingLinks(..) => {
                transaction
                    .commit_with_callback(|_| self.did_remove(name))
                    .await
                    .map_err(map_to_status)?;
            }
            ReplacedChild::Object(id) => {
                transaction
                    .commit_with_callback(|_| self.did_remove(name))
                    .await
                    .map_err(map_to_status)?;
                // If purging fails , we should still return success, since the file will appear
                // unlinked at this point anyways.  The file should be cleaned up on a later mount.
                if let Err(e) = self.volume().maybe_purge_file(id).await {
                    warn!(error:? = e; "Failed to purge file");
                }
            }
            ReplacedChild::Directory(id) => {
                transaction
                    .commit_with_callback(|_| {
                        self.did_remove(name);
                        self.volume().mark_directory_deleted(id);
                    })
                    .await
                    .map_err(map_to_status)?;
            }
        }
        Ok(())
    }

    async fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        let fs = self.store().filesystem();
        // TODO(b/365630582): Reconsider doing this as part of the transaction below.
        if let Some(casefold) = attributes.casefold {
            self.directory.set_casefold(casefold).await.map_err(map_to_status)?;
        }
        let transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    self.store().store_object_id(),
                    self.directory.object_id()
                )],
                Options { borrow_metadata_space: true, ..Default::default() },
            )
            .await
            .map_err(map_to_status)?;

        // TODO(https://fxbug.dev/298128836): atime can be updated but is not properly supported.
        self.directory
            .update_attributes(transaction, Some(&attributes), 0, Some(Timestamp::now()))
            .await
            .map_err(map_to_status)?;
        Ok(())
    }

    async fn sync(&self) -> Result<(), zx::Status> {
        // FDIO implements `syncfs` by calling sync on a directory, so replicate that behaviour.
        self.volume()
            .store()
            .filesystem()
            .sync(SyncOptions { flush_device: true, ..Default::default() })
            .await
            .map_err(map_to_status)
    }

    fn rename(
        self: Arc<Self>,
        src_dir: Arc<dyn MutableDirectory>,
        src_name: Path,
        dst_name: Path,
    ) -> BoxFuture<'static, Result<(), zx::Status>> {
        Box::pin(self.rename_impl(src_dir, src_name, dst_name))
    }

    async fn create_symlink(
        &self,
        name: String,
        target: Vec<u8>,
        connection: Option<ServerEnd<fio::SymlinkMarker>>,
    ) -> Result<(), zx::Status> {
        let store = self.store();
        let dir = &self.directory;
        let keys = lock_keys![LockKey::object(store.store_object_id(), dir.object_id())];
        let fs = store.filesystem();
        let mut transaction =
            fs.new_transaction(keys, Options::default()).await.map_err(map_to_status)?;
        if dir.lookup(&name).await.map_err(map_to_status)?.is_some() {
            return Err(zx::Status::ALREADY_EXISTS);
        }
        let object_id =
            dir.create_symlink(&mut transaction, &target, &name).await.map_err(map_to_status)?;
        if let Some(connection) = connection {
            if let GetResult::Placeholder(p) = self.volume().cache().get_or_reserve(object_id).await
            {
                transaction
                    .commit_with_callback(|_| {
                        let node = Arc::new(FxSymlink::new(self.volume().clone(), object_id));
                        p.commit(&(node.clone() as Arc<dyn FxNode>));
                        let scope = self.volume().scope();
                        scope.spawn(
                            symlink::Connection::new(scope.clone(), node)
                                .run(fio::OpenFlags::RIGHT_READABLE.to_object_request(connection)),
                        );
                    })
                    .await
            } else {
                // The node already exists in the cache which could only happen if the filesystem is
                // corrupt.
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
        } else {
            transaction.commit().await.map(|_| ())
        }
        .map_err(map_to_status)
    }

    async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, zx::Status> {
        self.directory.list_extended_attributes().await.map_err(map_to_status)
    }

    async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, zx::Status> {
        self.directory.get_extended_attribute(name).await.map_err(map_to_status)
    }

    async fn set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: Vec<u8>,
        mode: fio::SetExtendedAttributeMode,
    ) -> Result<(), zx::Status> {
        self.directory.set_extended_attribute(name, value, mode.into()).await.map_err(map_to_status)
    }

    async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), zx::Status> {
        self.directory.remove_extended_attribute(name).await.map_err(map_to_status)
    }
}

impl DirectoryEntry for FxDirectory {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_dir(self)
    }
}

impl GetEntryInfo for FxDirectory {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.object_id(), fio::DirentType::Directory)
    }
}

impl vfs::node::Node for FxDirectory {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let props = self.directory.get_properties().await.map_err(map_to_status)?;
        Ok(attributes!(
            requested_attributes,
            Mutable {
                creation_time: props.creation_time.as_nanos(),
                modification_time: props.modification_time.as_nanos(),
                access_time: props.access_time.as_nanos(),
                mode: props.posix_attributes.map(|a| a.mode),
                uid: props.posix_attributes.map(|a| a.uid),
                gid: props.posix_attributes.map(|a| a.gid),
                rdev: props.posix_attributes.map(|a| a.rdev),
                casefold: self.directory.casefold(),
                selinux_context: self
                    .directory
                    .handle()
                    .get_inline_selinux_context()
                    .await
                    .map_err(map_to_status)?,
                wrapping_key_id: props.wrapping_key_id.map(|a| a.to_le_bytes()),
            },
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE
                    | fio::Operations::MODIFY_DIRECTORY,
                content_size: props.data_attribute_size,
                storage_size: props.allocated_size,
                link_count: props.refs + 1 + props.sub_dirs,
                id: self.directory.object_id(),
                change_time: props.change_time.as_nanos(),
                verity_enabled: false,
            }
        ))
    }

    fn query_filesystem(&self) -> Result<fio::FilesystemInfo, zx::Status> {
        let store = self.directory.store();
        Ok(info_to_filesystem_info(
            store.filesystem().get_info(),
            store.filesystem().block_size(),
            store.object_count(),
            self.volume().id(),
        ))
    }
}

impl VfsDirectory for FxDirectory {
    fn open(
        self: Arc<Self>,
        _scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        // Ignore the provided scope which might be for the parent pseudo filesystem and use the
        // volume's scope instead.
        let scope = self.volume().scope().clone();
        flags.to_object_request(server_end).spawn(&scope.clone(), move |object_request| {
            Box::pin(async move {
                let node =
                    self.lookup(&flags, path, object_request).await.map_err(map_to_status)?;
                if node.is::<FxDirectory>() {
                    object_request.create_connection(
                        scope,
                        node.downcast::<FxDirectory>().unwrap_or_else(|_| unreachable!()).take(),
                        flags,
                        MutableConnection::create,
                    )
                } else if node.is::<FxFile>() {
                    let node = node.downcast::<FxFile>().unwrap_or_else(|_| unreachable!());
                    if flags.contains(fio::OpenFlags::RIGHT_WRITABLE) && node.verified_file() {
                        log::error!("Tried to open a verified file with the RIGHT_WRITABLE flag.");
                        return Err(zx::Status::NOT_SUPPORTED);
                    }
                    if flags.contains(fio::OpenFlags::BLOCK_DEVICE) {
                        if node.verified_file() {
                            log::error!("Tried to expose a verified file as a block device.");
                            return Err(zx::Status::NOT_SUPPORTED);
                        }
                        let mut server =
                            BlockServer::new(node, object_request.take().into_channel());
                        Ok(async move {
                            let _ = server.run().await;
                        }
                        .boxed())
                    } else {
                        FxFile::create_connection_async(node, scope, flags, object_request).await
                    }
                } else if node.is::<FxSymlink>() {
                    let node = node.downcast::<FxSymlink>().unwrap_or_else(|_| unreachable!());
                    object_request.create_connection(
                        scope.clone(),
                        node.take(),
                        flags,
                        |scope, symlink, protocols, object_request| {
                            symlink::Connection::create(scope, symlink, &protocols, object_request)
                        },
                    )
                } else {
                    unreachable!();
                }
            })
        });
    }

    fn open3(
        self: Arc<Self>,
        _scope: ExecutionScope,
        path: Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        // Ignore the provided scope which might be for the parent pseudo filesystem and use the
        // volume's scope instead.
        let scope = self.volume().scope().clone();
        object_request.take().spawn(&scope.clone(), move |object_request| {
            Box::pin(async move {
                let node =
                    self.lookup(&flags, path, object_request).await.map_err(map_to_status)?;
                if node.is::<FxDirectory>() {
                    object_request.create_connection(
                        scope,
                        node.downcast::<FxDirectory>().unwrap_or_else(|_| unreachable!()).take(),
                        flags,
                        MutableConnection::create,
                    )
                } else if node.is::<FxFile>() {
                    let node = node.downcast::<FxFile>().unwrap_or_else(|_| unreachable!());
                    FxFile::create_connection_async(node, scope, flags, object_request).await
                } else if node.is::<FxSymlink>() {
                    let node = node.downcast::<FxSymlink>().unwrap_or_else(|_| unreachable!());
                    object_request.create_connection(
                        scope.clone(),
                        node.take(),
                        flags,
                        |scope, symlink, flags, object_request| {
                            symlink::Connection::create(scope, symlink, &flags, object_request)
                        },
                    )
                } else {
                    unreachable!();
                }
            })
        });
        Ok(())
    }

    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        mut sink: Box<dyn Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), zx::Status> {
        if let TraversalPosition::End = pos {
            return Ok((TraversalPosition::End, sink.seal()));
        } else if let TraversalPosition::Index(_) = pos {
            // The VFS should never send this to us, since we never return it here.
            return Err(zx::Status::BAD_STATE);
        }

        let store = self.store();
        let fs = store.filesystem();
        let _read_guard = fs
            .lock_manager()
            .read_lock(lock_keys![LockKey::object(store.store_object_id(), self.object_id())])
            .await;
        if self.is_deleted() {
            return Ok((TraversalPosition::End, sink.seal()));
        }

        let layer_set = self.store().tree().layer_set();
        let mut merger = layer_set.merger();
        let starting_name = match pos {
            TraversalPosition::Start => {
                // Synthesize a "." entry if we're at the start of the stream.
                match sink
                    .append(&EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory), ".")
                {
                    AppendResult::Ok(new_sink) => sink = new_sink,
                    AppendResult::Sealed(sealed) => {
                        // Note that the VFS should have yielded an error since the first entry
                        // didn't fit. This is defensive in case the VFS' behaviour changes, so that
                        // we return a reasonable value.
                        return Ok((TraversalPosition::Start, sealed));
                    }
                }
                ""
            }
            TraversalPosition::Name(name) => name,
            _ => unreachable!(),
        };
        let mut iter =
            self.directory.iter_from(&mut merger, starting_name).await.map_err(map_to_status)?;
        while let Some((name, object_id, object_descriptor)) = iter.get() {
            let entry_type = match object_descriptor {
                ObjectDescriptor::File => fio::DirentType::File,
                ObjectDescriptor::Directory => fio::DirentType::Directory,
                ObjectDescriptor::Symlink => fio::DirentType::Symlink,
                ObjectDescriptor::Volume => return Err(zx::Status::IO_DATA_INTEGRITY),
            };

            let info = EntryInfo::new(object_id, entry_type);
            match sink.append(&info, &name) {
                AppendResult::Ok(new_sink) => sink = new_sink,
                AppendResult::Sealed(sealed) => {
                    // We did *not* add the current entry to the sink (e.g. because the sink was
                    // full), so mark |name| as the next position so that it's the first entry we
                    // process on a subsequent call of read_dirents.
                    // Note that entries inserted between the previous entry and this entry before
                    // the next call to read_dirents would not be included in the results (but
                    // there's no requirement to include them anyways).
                    return Ok((TraversalPosition::Name(name.to_string()), sealed));
                }
            }
            iter.advance().await.map_err(map_to_status)?;
        }

        Ok((TraversalPosition::End, sink.seal()))
    }

    fn register_watcher(
        self: Arc<Self>,
        scope: ExecutionScope,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        let controller =
            self.watchers.lock().unwrap().add(scope.clone(), self.clone(), mask, watcher);
        if mask.contains(fio::WatchMask::EXISTING) && !self.is_deleted() {
            scope.spawn(async move {
                let layer_set = self.store().tree().layer_set();
                let mut merger = layer_set.merger();
                let mut iter = match self.directory.iter_from(&mut merger, "").await {
                    Ok(iter) => iter,
                    Err(e) => {
                        error!(error:? = e; "Failed to iterate directory for watch",);
                        // TODO(https://fxbug.dev/42178164): This really should close the watcher connection
                        // with an epitaph so that the watcher knows.
                        return;
                    }
                };
                // TODO(https://fxbug.dev/42178165): It is possible that we'll duplicate entries that are added
                // as we iterate over directories.  I suspect fixing this might be non-trivial.
                controller.send_event(&mut SingleNameEventProducer::existing("."));
                while let Some((name, _, _)) = iter.get() {
                    controller.send_event(&mut SingleNameEventProducer::existing(name));
                    if let Err(e) = iter.advance().await {
                        error!(error:? = e; "Failed to iterate directory for watch",);
                        return;
                    }
                }
                controller.send_event(&mut SingleNameEventProducer::idle());
            });
        }
        Ok(())
    }

    fn unregister_watcher(self: Arc<Self>, key: usize) {
        self.watchers.lock().unwrap().remove(key);
    }
}

impl From<Directory<FxVolume>> for FxDirectory {
    fn from(dir: Directory<FxVolume>) -> Self {
        Self::new(None, dir)
    }
}

#[cfg(test)]
mod tests {
    use crate::directory::FxDirectory;
    use crate::file::FxFile;
    use crate::fuchsia::testing::{
        close_dir_checked, close_file_checked, open3_dir, open3_dir_checked, open_dir,
        open_dir_checked, open_file, open_file_checked, TestFixture, TestFixtureOptions,
    };
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_proxy, ClientEnd, Proxy, ServerEnd};
    use fuchsia_fs::directory::{DirEntry, DirentKind};
    use fuchsia_fs::file;
    use futures::StreamExt;
    use fxfs::object_store::Timestamp;
    use fxfs_crypto::FSCRYPT_PADDING;
    use fxfs_insecure_crypto::InsecureCrypt;
    use rand::Rng;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use storage_device::fake_device::FakeDevice;
    use storage_device::DeviceHolder;
    use vfs::common::rights_to_posix_mode_bits;
    use vfs::node::Node;
    use vfs::path::Path;
    use vfs::ObjectRequest;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    #[fuchsia::test]
    async fn test_open_root_dir() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();
        let _: Vec<_> = root.query().await.expect("query failed");
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_dir_persists() {
        let mut device = DeviceHolder::new(FakeDevice::new(8192, 512));
        for i in 0..2 {
            let fixture = TestFixture::open(
                device,
                TestFixtureOptions {
                    format: i == 0,
                    encrypted: true,
                    as_blob: false,
                    serve_volume: false,
                },
            )
            .await;
            let root = fixture.root();

            let flags = if i == 0 {
                fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE
            } else {
                fio::OpenFlags::RIGHT_READABLE
            };
            let dir = open_dir_checked(&root, flags | fio::OpenFlags::DIRECTORY, "foo").await;
            close_dir_checked(dir).await;

            device = fixture.close().await;
        }
    }

    #[fuchsia::test]
    async fn test_open_nonexistent_file() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        assert_eq!(
            open_file(&root, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY, "foo")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<zx::Status>()
                .expect("No status"),
            &zx::Status::NOT_FOUND,
        );

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_file() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let f = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "foo",
        )
        .await;
        close_file_checked(f).await;

        let f = open_file_checked(
            &root,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "foo",
        )
        .await;
        close_file_checked(f).await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_dir_nested() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let d = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;
        close_dir_checked(d).await;

        let d = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            "foo/bar",
        )
        .await;
        close_dir_checked(d).await;

        let d = open_dir_checked(
            &root,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            "foo/bar",
        )
        .await;
        close_dir_checked(d).await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_strict_create_file_fails_if_present() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let f = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::CREATE_IF_ABSENT
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            "foo",
        )
        .await;
        close_file_checked(f).await;

        assert_eq!(
            open_file(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::CREATE_IF_ABSENT
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                "foo",
            )
            .await
            .expect_err("Open succeeded")
            .root_cause()
            .downcast_ref::<zx::Status>()
            .expect("No status"),
            &zx::Status::ALREADY_EXISTS,
        );

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlink_file_with_no_refs_immediately_freed() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            "foo",
        )
        .await;

        // Fill up the file with a lot of data, so we can verify that the extents are freed.
        let buf = vec![0xaa as u8; 512];
        loop {
            match file::write(&file, buf.as_slice()).await {
                Ok(_) => {}
                Err(e) => {
                    if let fuchsia_fs::file::WriteError::WriteError(status) = e {
                        if status == zx::Status::NO_SPACE {
                            break;
                        }
                    }
                    panic!("Unexpected write error {:?}", e);
                }
            }
        }

        close_file_checked(file).await;

        root.unlink("foo", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        assert_eq!(
            open_file(&root, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY, "foo")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<zx::Status>()
                .expect("No status"),
            &zx::Status::NOT_FOUND,
        );

        // Create another file so we can verify that the extents were actually freed.
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            "bar",
        )
        .await;
        let buf = vec![0xaa as u8; 8192];
        file::write(&file, buf.as_slice()).await.expect("Failed to write new file");
        close_file_checked(file).await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlink_file() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            "foo",
        )
        .await;
        close_file_checked(file).await;

        root.unlink("foo", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        assert_eq!(
            open_file(&root, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY, "foo")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<zx::Status>()
                .expect("No status"),
            &zx::Status::NOT_FOUND,
        );

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlink_file_with_active_references() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            "foo",
        )
        .await;

        let buf = vec![0xaa as u8; 512];
        file::write(&file, buf.as_slice()).await.expect("write failed");

        root.unlink("foo", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        // The child should immediately appear unlinked...
        assert_eq!(
            open_file(&root, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY, "foo")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<zx::Status>()
                .expect("No status"),
            &zx::Status::NOT_FOUND,
        );

        // But its contents should still be readable from the other handle.
        file.seek(fio::SeekOrigin::Start, 0)
            .await
            .expect("seek failed")
            .map_err(zx::Status::from_raw)
            .expect("seek error");
        let rbuf = file::read(&file).await.expect("read failed");
        assert_eq!(rbuf, buf);
        close_file_checked(file).await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlink_dir_with_children_fails() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let dir = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;
        let f = open_file_checked(
            &dir,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            "bar",
        )
        .await;
        close_file_checked(f).await;

        assert_eq!(
            zx::Status::from_raw(
                root.unlink("foo", &fio::UnlinkOptions::default())
                    .await
                    .expect("FIDL call failed")
                    .expect_err("unlink succeeded")
            ),
            zx::Status::NOT_EMPTY
        );

        dir.unlink("bar", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");
        root.unlink("foo", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        close_dir_checked(dir).await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlink_dir_makes_directory_immutable() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let dir = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        root.unlink("foo", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        assert_eq!(
            open_file(
                &dir,
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::CREATE
                    | fio::OpenFlags::NOT_DIRECTORY,
                "bar"
            )
            .await
            .expect_err("Create file succeeded")
            .root_cause()
            .downcast_ref::<zx::Status>()
            .expect("No status"),
            &zx::Status::ACCESS_DENIED,
        );

        close_dir_checked(dir).await;

        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_unlink_directory_with_children_race() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        const PARENT: &str = "foo";
        const CHILD: &str = "bar";
        const GRANDCHILD: &str = "baz";
        open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            PARENT,
        )
        .await;

        let open_parent = || async {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                PARENT,
            )
            .await
        };
        let parent = open_parent().await;

        // Each iteration proceeds as follows:
        //  - Initialize a directory foo/bar/. (This might still be around from the previous
        //    iteration, which is fine.)
        //  - In one task, try to unlink foo/bar/.
        //  - In another task, try to add a file foo/bar/baz.
        for _ in 0..100 {
            let d = open_dir_checked(
                &parent,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                CHILD,
            )
            .await;
            close_dir_checked(d).await;

            let parent = open_parent().await;
            let deleter = fasync::Task::spawn(async move {
                let wait_time = rand::thread_rng().gen_range(0..5);
                fasync::Timer::new(Duration::from_millis(wait_time)).await;
                match parent
                    .unlink(CHILD, &fio::UnlinkOptions::default())
                    .await
                    .expect("FIDL call failed")
                    .map_err(zx::Status::from_raw)
                {
                    Ok(()) => {}
                    Err(zx::Status::NOT_EMPTY) => {}
                    Err(e) => panic!("Unexpected status from unlink: {:?}", e),
                };
                close_dir_checked(parent).await;
            });

            let parent = open_parent().await;
            let writer = fasync::Task::spawn(async move {
                let child_or = open_dir(
                    &parent,
                    fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::DIRECTORY,
                    CHILD,
                )
                .await;
                if let Err(e) = &child_or {
                    // The directory was already deleted.
                    assert_eq!(
                        e.root_cause().downcast_ref::<zx::Status>().expect("No status"),
                        &zx::Status::NOT_FOUND
                    );
                    close_dir_checked(parent).await;
                    return;
                }
                let child = child_or.unwrap();
                let _: Vec<_> = child.query().await.expect("query failed");
                match open_file(
                    &child,
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::NOT_DIRECTORY,
                    GRANDCHILD,
                )
                .await
                {
                    Ok(grandchild) => {
                        let _: Vec<_> = grandchild.query().await.expect("query failed");
                        close_file_checked(grandchild).await;
                        // We added the child before the directory was deleted; go ahead and
                        // clean up.
                        child
                            .unlink(GRANDCHILD, &fio::UnlinkOptions::default())
                            .await
                            .expect("FIDL call failed")
                            .expect("unlink failed");
                    }
                    Err(e) => {
                        // The directory started to be deleted before we created a child.
                        // Make sure we get the right error.
                        assert_eq!(
                            e.root_cause().downcast_ref::<zx::Status>().expect("No status"),
                            &zx::Status::ACCESS_DENIED,
                        );
                    }
                };
                close_dir_checked(child).await;
                close_dir_checked(parent).await;
            });
            writer.await;
            deleter.await;
        }

        close_dir_checked(parent).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_readdir() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent = Arc::new(open_dir().await);

        let files = ["eenie", "meenie", "minie", "moe"];
        for file in &files {
            let file = open_file_checked(
                parent.as_ref(),
                fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
                file,
            )
            .await;
            close_file_checked(file).await;
        }
        let dirs = ["fee", "fi", "fo", "fum"];
        for dir in &dirs {
            let dir = open_dir_checked(
                parent.as_ref(),
                fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY,
                dir,
            )
            .await;
            close_dir_checked(dir).await;
        }
        {
            parent
                .create_symlink("symlink", b"target", None)
                .await
                .expect("FIDL call failed")
                .expect("create_symlink failed");
        }

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let mut expected_entries =
            vec![DirEntry { name: ".".to_owned(), kind: DirentKind::Directory }];
        expected_entries.extend(
            files.iter().map(|&name| DirEntry { name: name.to_owned(), kind: DirentKind::File }),
        );
        expected_entries.extend(
            dirs.iter()
                .map(|&name| DirEntry { name: name.to_owned(), kind: DirentKind::Directory }),
        );
        expected_entries.push(DirEntry { name: "symlink".to_owned(), kind: DirentKind::Symlink });
        expected_entries.sort_unstable();
        assert_eq!(expected_entries, readdir(Arc::clone(&parent)).await);

        // Remove an entry.
        parent
            .unlink(&expected_entries.pop().unwrap().name, &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        assert_eq!(expected_entries, readdir(Arc::clone(&parent)).await);

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_readdir_multiple_calls() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let parent = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let files = ["a", "b"];
        for file in &files {
            let file = open_file_checked(
                &parent,
                fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
                file,
            )
            .await;
            close_file_checked(file).await;
        }

        // TODO(https://fxbug.dev/42177353): Magic number; can we get this from fuchsia.io?
        const DIRENT_SIZE: u64 = 10; // inode: u64, size: u8, kind: u8
        const BUFFER_SIZE: u64 = DIRENT_SIZE + 2; // Enough space for a 2-byte name.

        let parse_entries = |buf| {
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let expected_entries = vec![
            DirEntry { name: ".".to_owned(), kind: DirentKind::Directory },
            DirEntry { name: "a".to_owned(), kind: DirentKind::File },
        ];
        let (status, buf) = parent.read_dirents(2 * BUFFER_SIZE).await.expect("FIDL call failed");
        zx::Status::ok(status).expect("read_dirents failed");
        assert_eq!(expected_entries, parse_entries(&buf));

        let expected_entries = vec![DirEntry { name: "b".to_owned(), kind: DirentKind::File }];
        let (status, buf) = parent.read_dirents(2 * BUFFER_SIZE).await.expect("FIDL call failed");
        zx::Status::ok(status).expect("read_dirents failed");
        assert_eq!(expected_entries, parse_entries(&buf));

        // Subsequent calls yield nothing.
        let expected_entries: Vec<DirEntry> = vec![];
        let (status, buf) = parent.read_dirents(2 * BUFFER_SIZE).await.expect("FIDL call failed");
        zx::Status::ok(status).expect("read_dirents failed");
        assert_eq!(expected_entries, parse_entries(&buf));

        close_dir_checked(parent).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_set_large_extended_attribute_on_encrypted_directory() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        let dir = open_dir_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
            "fee",
        )
        .await;

        let xattr_name = b"xattr_name";
        let value_vec = vec![0x3; 300];

        dir.set_extended_attribute(
            xattr_name,
            fio::ExtendedAttributeValue::Bytes(value_vec.clone()),
            fio::SetExtendedAttributeMode::Set,
        )
        .await
        .expect("Failed to make FIDL call")
        .expect("Failed to set xattr with create");

        let subdir =
            open_dir_checked(&dir, fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY, "fo").await;
        close_dir_checked(dir).await;
        close_dir_checked(subdir).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        let encrypted_dir = Arc::new(
            open_dir_checked(parent.as_ref(), fio::OpenFlags::DIRECTORY, &encrypted_name).await,
        );

        assert_eq!(
            encrypted_dir
                .get_extended_attribute(xattr_name)
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get extended attribute"),
            fio::ExtendedAttributeValue::Bytes(value_vec)
        );

        let encrypted_subdir_entries = readdir(Arc::clone(&encrypted_dir)).await;
        for entry in encrypted_subdir_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                assert!(entry.kind == DirentKind::Directory)
            }
        }
        close_dir_checked(Arc::try_unwrap(encrypted_dir).unwrap()).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_set_large_extended_attribute_on_encrypted_file() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        let file = open_file_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            "fee",
        )
        .await;

        let buf = vec![0xaa as u8; 512];
        file::write(&file, buf.as_slice()).await.expect("write failed");

        let xattr_name = b"xattr_name";
        let value_vec = vec![0x3; 300];

        file.set_extended_attribute(
            xattr_name,
            fio::ExtendedAttributeValue::Bytes(value_vec.clone()),
            fio::SetExtendedAttributeMode::Set,
        )
        .await
        .expect("Failed to make FIDL call")
        .expect("Failed to set xattr with create");

        close_file_checked(file).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let crypt: Arc<InsecureCrypt> = new_fixture.crypt().unwrap();
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::File)
            }
        }

        let encrypted_file = Arc::new(
            open_file_checked(parent.as_ref(), fio::OpenFlags::NOT_DIRECTORY, &encrypted_name)
                .await,
        );

        assert_eq!(
            encrypted_file
                .get_extended_attribute(xattr_name)
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get extended attribute"),
            fio::ExtendedAttributeValue::Bytes(value_vec)
        );

        close_file_checked(Arc::try_unwrap(encrypted_file).unwrap()).await;

        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);

        let file = Arc::new(
            open_file_checked(
                parent.as_ref(),
                fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
                "fee",
            )
            .await,
        );

        let rbuf = file::read(&file).await.expect("read failed");
        assert_eq!(rbuf, buf);

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_encrypt_directory_in_unencrypted_volume() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let wrapping_key_id: u128 = 2;
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let _ = parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect_err("encrypting a dir in an unencrypted volume should fail");
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_encrypt_directory_with_large_extended_attribute() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let xattr_name = b"xattr_name";
        let value_vec = vec![0x3; 300];
        parent
            .set_extended_attribute(
                xattr_name,
                fio::ExtendedAttributeValue::Bytes(value_vec.clone()),
                fio::SetExtendedAttributeMode::Set,
            )
            .await
            .expect("Failed to make FIDL call")
            .expect("Failed to set xattr with create");

        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        let dir = open_dir_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
            "fee",
        )
        .await;

        let subdir =
            open_dir_checked(&dir, fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY, "fo").await;
        close_dir_checked(dir).await;
        close_dir_checked(subdir).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        assert_eq!(
            parent
                .get_extended_attribute(xattr_name)
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get extended attribute"),
            fio::ExtendedAttributeValue::Bytes(value_vec)
        );

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        let encrypted_dir = Arc::new(
            open_dir_checked(parent.as_ref(), fio::OpenFlags::DIRECTORY, &encrypted_name).await,
        );

        let encrypted_subdir_entries = readdir(Arc::clone(&encrypted_dir)).await;
        for entry in encrypted_subdir_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                assert!(entry.kind == DirentKind::Directory)
            }
        }
        close_dir_checked(Arc::try_unwrap(encrypted_dir).unwrap()).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlock_directory_during_readdir() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");

        // Need enough entries such that multiple read_dirents calls are required to drain all the
        // entries.
        for i in 0..300 {
            let dir = open_dir_checked(
                parent.as_ref(),
                fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
                &format!("plaintext_{}", i),
            )
            .await;
            close_dir_checked(dir).await;
        }

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let crypt: Arc<InsecureCrypt> = new_fixture.crypt().unwrap();
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                assert!(!entry.name.starts_with("plaintext_"), "{entry:?} isn't encrypted!");
                assert!(entry.kind == DirentKind::Directory)
            }
        }
        crypt.add_wrapping_key(2, [1; 32]);
        let unencrypted_entries = readdir(Arc::clone(&parent)).await;
        for entry in unencrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.starts_with("plaintext_"), "{entry:?} is still encrypted!");
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_readdir_locked_directory() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        let dir = open_dir_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
            "fee",
        )
        .await;

        let subdir =
            open_dir_checked(&dir, fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY, "fo").await;
        close_dir_checked(dir).await;
        close_dir_checked(subdir).await;

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let mut expected_entries =
            vec![DirEntry { name: ".".to_owned(), kind: DirentKind::Directory }];

        expected_entries.push(DirEntry { name: "fee".to_owned(), kind: DirentKind::Directory });
        expected_entries.sort_unstable();
        assert_eq!(expected_entries, readdir(Arc::clone(&parent)).await);

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        let encrypted_dir = Arc::new(
            open_dir_checked(parent.as_ref(), fio::OpenFlags::DIRECTORY, &encrypted_name).await,
        );

        let encrypted_subdir_entries = readdir(Arc::clone(&encrypted_dir)).await;
        for entry in encrypted_subdir_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                assert!(entry.kind == DirentKind::Directory)
            }
        }
        close_dir_checked(Arc::try_unwrap(encrypted_dir).unwrap()).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_encrypted_filename_does_not_have_slashes() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for _ in 0..100 {
            let one_char = || CHARSET[rand::thread_rng().gen_range(0..CHARSET.len())] as char;
            let filename: String = std::iter::repeat_with(one_char).take(100).collect();
            let dir = open_dir_checked(
                parent.as_ref(),
                fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
                &filename,
            )
            .await;
            close_dir_checked(dir).await;
        }

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                assert!(!entry.name.contains("/"));
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_stat_locked_file() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");

        let file = open_file_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
            "file",
        )
        .await;

        close_file_checked(file).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;

        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let (status, buf) = parent.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
        zx::Status::ok(status).expect("read_dirents failed");
        let mut encrypted_entries = vec![];
        for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
            encrypted_entries.push(res.expect("Failed to parse entry"));
        }
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::File)
            }
        }

        let file =
            open_file_checked(parent.as_ref(), fio::OpenFlags::NOT_DIRECTORY, &encrypted_name)
                .await;
        let (_mutable_attributes, _immutable_attributes) = file
            .get_attributes(
                fio::NodeAttributesQuery::CONTENT_SIZE
                    | fio::NodeAttributesQuery::STORAGE_SIZE
                    | fio::NodeAttributesQuery::LINK_COUNT
                    | fio::NodeAttributesQuery::MODIFICATION_TIME
                    | fio::NodeAttributesQuery::CHANGE_TIME,
            )
            .await
            .expect("FIDL call failed")
            .map_err(zx::Status::from_raw)
            .expect("get_attributes failed");
        close_file_checked(file).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_unlink_locked_directory() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        let dir = open_dir_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
            "fee",
        )
        .await;

        close_dir_checked(dir).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        parent
            .unlink(&encrypted_name, &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut count = 0;
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                assert!(entry.kind == DirentKind::Directory)
            }
            count += 1;
        }
        assert_eq!(count, 0);
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_rename_within_locked_encrypted_directory() {
        let fixture = TestFixture::new().await;
        let crypt: Arc<InsecureCrypt> = fixture.crypt().unwrap();
        let root = fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };

        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);
        let wrapping_key_id = 2;
        crypt.add_wrapping_key(wrapping_key_id, [1; 32]);
        parent
            .update_attributes(&fio::MutableNodeAttributes {
                wrapping_key_id: Some(wrapping_key_id.to_le_bytes()),
                ..Default::default()
            })
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
        let dir = open_dir_checked(
            parent.as_ref(),
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
            "fee",
        )
        .await;

        close_dir_checked(dir).await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        let device = fixture.close().await;
        let new_fixture = TestFixture::new_with_device(device).await;
        let crypt: Arc<InsecureCrypt> = new_fixture.crypt().unwrap();
        let root = new_fixture.root();
        let open_dir = || {
            open_dir_checked(
                &root,
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
        };
        let parent: Arc<fio::DirectoryProxy> = Arc::new(open_dir().await);

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            zx::Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry"));
            }
            entries
        };

        let encrypted_entries = readdir(Arc::clone(&parent)).await;
        let mut encrypted_name = String::new();
        for entry in encrypted_entries {
            if entry.name == ".".to_owned() {
                continue;
            } else {
                assert!(entry.name.len() >= FSCRYPT_PADDING);
                encrypted_name = entry.name;
                assert!(entry.kind == DirentKind::Directory)
            }
        }

        let (status, dst_token) = parent.get_token().await.expect("FIDL call failed");
        zx::Status::ok(status).expect("get_token failed");
        let new_encrypted_name = "aabbcc";
        parent
            .rename(&encrypted_name, zx::Event::from(dst_token.unwrap()), new_encrypted_name)
            .await
            .expect("FIDL call failed")
            .expect_err("rename should fail on a locked directory");
        let (status, dst_token) = parent.get_token().await.expect("FIDL call failed");
        zx::Status::ok(status).expect("get_token failed");
        crypt.add_wrapping_key(2, [1; 32]);
        parent
            .rename("fee", zx::Event::from(dst_token.unwrap()), "new_fee")
            .await
            .expect("FIDL call failed")
            .expect("rename should fail on a locked directory");

        let _dir = open_dir_checked(parent.as_ref(), fio::OpenFlags::DIRECTORY, "new_fee").await;
        close_dir_checked(Arc::try_unwrap(parent).unwrap()).await;
        new_fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_set_attrs() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let dir = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let (status, initial_attrs) = dir.get_attr().await.expect("FIDL call failed");
        zx::Status::ok(status).expect("get_attr failed");

        let crtime = initial_attrs.creation_time ^ 1u64;
        let mtime = initial_attrs.modification_time ^ 1u64;

        let mut attrs = initial_attrs.clone();
        attrs.creation_time = crtime;
        attrs.modification_time = mtime;
        let status = dir
            .set_attr(fio::NodeAttributeFlags::CREATION_TIME, &attrs)
            .await
            .expect("FIDL call failed");
        zx::Status::ok(status).expect("set_attr failed");

        let mut expected_attrs = initial_attrs.clone();
        expected_attrs.creation_time = crtime; // Only crtime is updated so far.
        let (status, attrs) = dir.get_attr().await.expect("FIDL call failed");
        zx::Status::ok(status).expect("get_attr failed");
        assert_eq!(expected_attrs, attrs);

        let mut attrs = initial_attrs.clone();
        attrs.creation_time = 0u64; // This should be ignored since we don't set the flag.
        attrs.modification_time = mtime;
        let status = dir
            .set_attr(fio::NodeAttributeFlags::MODIFICATION_TIME, &attrs)
            .await
            .expect("FIDL call failed");
        zx::Status::ok(status).expect("set_attr failed");

        let mut expected_attrs = initial_attrs.clone();
        expected_attrs.creation_time = crtime;
        expected_attrs.modification_time = mtime;
        let (status, attrs) = dir.get_attr().await.expect("FIDL call failed");
        zx::Status::ok(status).expect("get_attr failed");
        assert_eq!(expected_attrs, attrs);

        close_dir_checked(dir).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_symlink() {
        let fixture = TestFixture::new().await;

        {
            let root = fixture.root();

            root.create_symlink("symlink", b"target", None)
                .await
                .expect("FIDL call failed")
                .expect("create_symlink failed");

            let (proxy, server_end) = create_proxy::<fio::SymlinkMarker>();
            root.open(
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE,
                fio::ModeType::empty(),
                "symlink",
                ServerEnd::new(server_end.into_channel()),
            )
            .expect("open failed");

            let on_open = proxy
                .take_event_stream()
                .next()
                .await
                .expect("missing OnOpen event")
                .expect("failed to read event")
                .into_on_open_();

            if let Some((0, Some(node_info))) = on_open {
                assert_matches!(
                    *node_info,
                    fio::NodeInfoDeprecated::Symlink(fio::SymlinkObject { target, .. })
                        if target == b"target"
                );
            } else {
                panic!("Unexpected on_open {on_open:?}");
            }

            let (proxy, server_end) = create_proxy::<fio::SymlinkMarker>();
            root.create_symlink("symlink2", b"target2", Some(server_end))
                .await
                .expect("FIDL call failed")
                .expect("create_symlink failed");

            let node_info = proxy.describe().await.expect("FIDL call failed");
            assert_matches!(
                node_info,
                fio::SymlinkInfo { target: Some(target), .. } if target == b"target2"
            );

            // Unlink the second symlink.
            root.unlink("symlink2", &fio::UnlinkOptions::default())
                .await
                .expect("FIDL call failed")
                .expect("unlink failed");

            // Rename over the first symlink.
            open_file_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE,
                "target",
            )
            .await;
            let (status, dst_token) = root.get_token().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("get_token failed");
            root.rename("target", zx::Event::from(dst_token.unwrap()), "symlink")
                .await
                .expect("FIDL call failed")
                .expect("rename failed");

            let (status, _) = proxy.get_attr().await.expect("FIDL call failed");
            assert_eq!(zx::Status::from_raw(status), zx::Status::NOT_FOUND);
            assert_matches!(
                proxy.describe().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FOUND, .. })
            );
        }

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_hard_link_to_symlink() {
        let fixture = TestFixture::new().await;

        {
            let root = fixture.root();

            root.create_symlink("symlink", b"target", None)
                .await
                .expect("FIDL call failed")
                .expect("create_symlink failed");

            async fn open_symlink(root: &fio::DirectoryProxy, path: &str) -> fio::SymlinkProxy {
                let (proxy, server_end) = create_proxy::<fio::SymlinkMarker>();
                root.open(
                    fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE,
                    fio::ModeType::empty(),
                    path,
                    ServerEnd::new(server_end.into_channel()),
                )
                .expect("open failed");

                let on_open = proxy
                    .take_event_stream()
                    .next()
                    .await
                    .expect("missing OnOpen event")
                    .expect("failed to read event")
                    .into_on_open_();

                if let Some((0, Some(node_info))) = on_open {
                    assert_matches!(
                        *node_info,
                        fio::NodeInfoDeprecated::Symlink(fio::SymlinkObject { target, .. })
                            if target == b"target"
                    );
                } else {
                    panic!("Unexpected on_open {on_open:?}");
                }

                proxy
            }

            let proxy = open_symlink(&root, "symlink").await;

            let (status, dst_token) = root.get_token().await.expect("FIDL call failed");
            zx::Status::ok(status).expect("get_token failed");
            proxy
                .link_into(zx::Event::from(dst_token.unwrap()), "symlink2")
                .await
                .expect("link_into (FIDL) failed")
                .expect("link_into failed");

            open_symlink(&root, "symlink2").await;
        }

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_symlink_stat() {
        let fixture = TestFixture::new().await;

        {
            let root = fixture.root();

            root.create_symlink("symlink", b"target", None)
                .await
                .expect("FIDL call failed")
                .expect("create_symlink failed");

            let root = fuchsia_fs::directory::clone(root).expect("clone failed");

            fasync::unblock(|| {
                let root: std::os::fd::OwnedFd =
                    fdio::create_fd(root.into_channel().unwrap().into_zx_channel().into())
                        .expect("create_fd failed");

                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                let name = std::ffi::CString::new("symlink").expect("CString::new failed");
                assert_eq!(
                    unsafe { libc::fstatat(root.as_raw_fd(), name.as_ptr(), &mut stat, 0) },
                    0
                );
            })
            .await;
        }

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_remove_dir_all_with_symlink() {
        // This test makes sure that remove_dir_all works.  At time of writing remove_dir_all uses
        // d_type from the directory entry to determine whether or not to recurse into directories,
        // so this tests that is working correctly.

        let fixture = TestFixture::new().await;

        {
            let root = fixture.root();

            let dir = open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "dir",
            )
            .await;

            dir.create_symlink("symlink", b"target", None)
                .await
                .expect("FIDL call failed")
                .expect("create_symlink failed");

            let namespace = fdio::Namespace::installed().expect("Unable to get namespace");
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = format!("/test_symlink_stat.{}", COUNTER.fetch_add(1, Ordering::Relaxed));
            let root = fuchsia_fs::directory::clone(root).expect("clone failed");
            namespace
                .bind(&path, ClientEnd::new(root.into_channel().unwrap().into_zx_channel()))
                .expect("bind failed");
            let path_copy = path.clone();
            scopeguard::defer!({
                let _ = namespace.unbind(&path_copy);
            });

            fasync::unblock(move || {
                assert_matches!(std::fs::remove_dir_all(&format!("{path}/dir")), Ok(()));
            })
            .await;
        }

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn extended_attributes() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let file = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let name = b"security.selinux";
        let value_vec = b"bar".to_vec();

        {
            let (iterator_client, iterator_server) =
                fidl::endpoints::create_proxy::<fio::ExtendedAttributeIteratorMarker>();
            file.list_extended_attributes(iterator_server).expect("Failed to make FIDL call");
            let (chunk, last) = iterator_client
                .get_next()
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get next iterator chunk");
            assert!(last);
            assert_eq!(chunk, Vec::<Vec<u8>>::new());
        }
        assert_eq!(
            file.get_extended_attribute(name)
                .await
                .expect("Failed to make FIDL call")
                .expect_err("Got successful message back for missing attribute"),
            zx::Status::NOT_FOUND.into_raw(),
        );

        file.set_extended_attribute(
            name,
            fio::ExtendedAttributeValue::Bytes(value_vec.clone()),
            fio::SetExtendedAttributeMode::Set,
        )
        .await
        .expect("Failed to make FIDL call")
        .expect("Failed to set extended attribute");

        {
            let (iterator_client, iterator_server) =
                fidl::endpoints::create_proxy::<fio::ExtendedAttributeIteratorMarker>();
            file.list_extended_attributes(iterator_server).expect("Failed to make FIDL call");
            let (chunk, last) = iterator_client
                .get_next()
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get next iterator chunk");
            assert!(last);
            assert_eq!(chunk, vec![name]);
        }
        assert_eq!(
            file.get_extended_attribute(name)
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get extended attribute"),
            fio::ExtendedAttributeValue::Bytes(value_vec)
        );

        file.remove_extended_attribute(name)
            .await
            .expect("Failed to make FIDL call")
            .expect("Failed to remove extended attribute");

        {
            let (iterator_client, iterator_server) =
                fidl::endpoints::create_proxy::<fio::ExtendedAttributeIteratorMarker>();
            file.list_extended_attributes(iterator_server).expect("Failed to make FIDL call");
            let (chunk, last) = iterator_client
                .get_next()
                .await
                .expect("Failed to make FIDL call")
                .expect("Failed to get next iterator chunk");
            assert!(last);
            assert_eq!(chunk, Vec::<Vec<u8>>::new());
        }
        assert_eq!(
            file.get_extended_attribute(name)
                .await
                .expect("Failed to make FIDL call")
                .expect_err("Got successful message back for missing attribute"),
            zx::Status::NOT_FOUND.into_raw(),
        );

        close_dir_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn extended_attribute_set_modes() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let dir = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let name = b"security.selinux";
        let value_vec = b"bar".to_vec();
        let value2_vec = b"new value".to_vec();

        // Can't replace an attribute that doesn't exist yet.
        assert_eq!(
            dir.set_extended_attribute(
                name,
                fio::ExtendedAttributeValue::Bytes(value_vec.clone()),
                fio::SetExtendedAttributeMode::Replace
            )
            .await
            .expect("Failed to make FIDL call")
            .expect_err("Got successful message back from replacing a nonexistent attribute"),
            zx::Status::NOT_FOUND.into_raw()
        );

        // Create works when it doesn't exist.
        dir.set_extended_attribute(
            name,
            fio::ExtendedAttributeValue::Bytes(value_vec.clone()),
            fio::SetExtendedAttributeMode::Create,
        )
        .await
        .expect("Failed to make FIDL call")
        .expect("Failed to set xattr with create");

        // Create doesn't work once it exists though.
        assert_eq!(
            dir.set_extended_attribute(
                name,
                fio::ExtendedAttributeValue::Bytes(value2_vec.clone()),
                fio::SetExtendedAttributeMode::Create
            )
            .await
            .expect("Failed to make FIDL call")
            .expect_err("Got successful message back from replacing a nonexistent attribute"),
            zx::Status::ALREADY_EXISTS.into_raw()
        );

        // But replace does.
        dir.set_extended_attribute(
            name,
            fio::ExtendedAttributeValue::Bytes(value2_vec.clone()),
            fio::SetExtendedAttributeMode::Replace,
        )
        .await
        .expect("Failed to make FIDL call")
        .expect("Failed to set xattr with create");

        close_dir_checked(dir).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_remove_large_xattr() {
        let fixture = TestFixture::new().await;
        {
            let root = fixture.root();
            let dir = open_dir_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                "foo",
            )
            .await;

            dir.set_extended_attribute(
                "name".as_bytes(),
                fio::ExtendedAttributeValue::Bytes(vec![17u8; 300]),
                fio::SetExtendedAttributeMode::Create,
            )
            .await
            .expect("FIDL call failed")
            .expect("Set xattr failed");

            dir.remove_extended_attribute("name".as_bytes())
                .await
                .expect("FIDL call failed")
                .expect("Set xattr failed");
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_dir_with_mutable_node_attributes() {
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.volume().root_dir();

            let path_str = "foo";
            let path = Path::validate_and_split(path_str).unwrap();

            let (_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let mode = fio::MODE_TYPE_DIRECTORY
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false);
            let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MAYBE_CREATE;
            let options = fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    mode: Some(mode),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let request = ObjectRequest::new(flags, &options, server_end.into_channel());
            let dir = root_dir.lookup(&flags, path, &request).await.expect("lookup failed");

            let attrs = dir
                .clone()
                .into_any()
                .downcast::<FxDirectory>()
                .expect("Not a directory")
                .get_attributes(
                    fio::NodeAttributesQuery::MODE
                        | fio::NodeAttributesQuery::UID
                        | fio::NodeAttributesQuery::ACCESS_TIME,
                )
                .await
                .expect("FIDL call failed");
            assert_eq!(attrs.mutable_attributes.mode.unwrap(), mode);
            // Since the POSIX mode attribute was set, we expect default values for the other POSIX
            // attributes.
            assert_eq!(attrs.mutable_attributes.uid.unwrap(), 0);
            // Expect these attributes to be None as they were not queried in `get_attributes(..)`
            assert!(attrs.mutable_attributes.gid.is_none());
            assert!(attrs.mutable_attributes.rdev.is_none());
            assert!(attrs.mutable_attributes.access_time.is_some());
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_dir_with_default_mutable_node_attributes() {
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.volume().root_dir();

            let path_str = "foo";
            let path = Path::validate_and_split(path_str).unwrap();

            let (_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MAYBE_CREATE;
            let options = fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes { ..Default::default() }),
                ..Default::default()
            };

            let request = ObjectRequest::new(flags, &options, server_end.into_channel());
            let dir = root_dir.lookup(&flags, path, &request).await.expect("lookup failed");

            let attrs = dir
                .clone()
                .into_any()
                .downcast::<FxDirectory>()
                .expect("Not a directory")
                .get_attributes(fio::NodeAttributesQuery::MODE)
                .await
                .expect("FIDL call failed");
            // Although mode was requested, it was not set when creating the directory. So we
            // expect None.
            assert!(attrs.mutable_attributes.mode.is_none());
            // The attributes not requested should be None.
            assert!(attrs.mutable_attributes.uid.is_none());
            assert!(attrs.mutable_attributes.gid.is_none());
            assert!(attrs.mutable_attributes.rdev.is_none());
            assert!(attrs.mutable_attributes.creation_time.is_none());
            assert!(attrs.mutable_attributes.modification_time.is_none());
            assert!(attrs.mutable_attributes.access_time.is_none());
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_dir_using_flags_and_options() {
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.volume().root_dir();

            let path_str = "foo";
            let path = Path::validate_and_split(path_str).unwrap();

            let (_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let mode = fio::MODE_TYPE_DIRECTORY
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false);
            let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MAYBE_CREATE;
            let options = fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    mode: Some(mode),
                    ..Default::default()
                }),
                ..Default::default()
            };

            // Create directory node.
            let request = ObjectRequest::new(flags, &options, server_end.into());
            let dir = root_dir.lookup(&flags, path, &request).await.expect("lookup failed");

            // Verify that the node was created with the attributes requested.
            let attrs = dir
                .clone()
                .into_any()
                .downcast::<FxDirectory>()
                .expect("Not a directory")
                .get_attributes(
                    fio::NodeAttributesQuery::MODE
                        | fio::NodeAttributesQuery::UID
                        | fio::NodeAttributesQuery::ACCESS_TIME,
                )
                .await
                .expect("FIDL call failed");
            assert_eq!(attrs.mutable_attributes.mode.unwrap(), mode);
            // Since the POSIX mode attribute was set, we expect default values for the other POSIX
            // attributes.
            assert_eq!(attrs.mutable_attributes.uid.unwrap(), 0);
            // Expect these attributes to be None as they were not queried in `get_attributes(..)`
            assert!(attrs.mutable_attributes.gid.is_none());
            assert!(attrs.mutable_attributes.rdev.is_none());
            assert!(attrs.mutable_attributes.access_time.is_some());
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_file_with_mutable_node_attributes() {
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.volume().root_dir();

            let path_str = "foo";
            let path = Path::validate_and_split(path_str).unwrap();

            let (_proxy, server_end) = create_proxy::<fio::FileMarker>();
            let mode = fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false);
            let uid = 1;
            let gid = 2;
            let rdev = 3;
            let modification_time = Timestamp::now().as_nanos();

            let flags = fio::Flags::PROTOCOL_FILE | fio::Flags::FLAG_MAYBE_CREATE;
            let options = fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    modification_time: Some(modification_time),
                    mode: Some(mode),
                    uid: Some(uid),
                    gid: Some(gid),
                    rdev: Some(rdev),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let request = ObjectRequest::new(flags, &options, server_end.into_channel());
            let file = root_dir.lookup(&flags, path, &request).await.expect("lookup failed");

            let attributes = file
                .clone()
                .into_any()
                .downcast::<FxFile>()
                .expect("Not a file")
                .get_attributes(
                    fio::NodeAttributesQuery::CREATION_TIME
                        | fio::NodeAttributesQuery::MODIFICATION_TIME
                        | fio::NodeAttributesQuery::CHANGE_TIME
                        | fio::NodeAttributesQuery::MODE
                        | fio::NodeAttributesQuery::UID
                        | fio::NodeAttributesQuery::GID
                        | fio::NodeAttributesQuery::RDEV,
                )
                .await
                .expect("FIDL call failed");
            assert_eq!(mode, attributes.mutable_attributes.mode.unwrap());
            assert_eq!(uid, attributes.mutable_attributes.uid.unwrap());
            assert_eq!(gid, attributes.mutable_attributes.gid.unwrap());
            assert_eq!(rdev, attributes.mutable_attributes.rdev.unwrap());
            assert_eq!(modification_time, attributes.mutable_attributes.modification_time.unwrap());
            assert!(attributes.mutable_attributes.creation_time.is_some());
            assert!(attributes.immutable_attributes.change_time.is_some());
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_file_with_default_mutable_node_attributes() {
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.volume().root_dir();

            let path_str = "foo";
            let path = Path::validate_and_split(path_str).unwrap();

            let (_proxy, server_end) = create_proxy::<fio::FileMarker>();

            let flags = fio::Flags::PROTOCOL_FILE | fio::Flags::FLAG_MAYBE_CREATE;
            let options = Default::default();

            let request = ObjectRequest::new(flags, &options, server_end.into_channel());
            let file = root_dir.lookup(&flags, path, &request).await.expect("lookup failed");

            let attrs = file
                .clone()
                .into_any()
                .downcast::<FxFile>()
                .expect("Not a directory")
                .get_attributes(fio::NodeAttributesQuery::MODE)
                .await
                .expect("FIDL call failed");
            // Although mode was requested, it was not set when creating the directory. So we
            // expect that it is None.
            assert!(attrs.mutable_attributes.mode.is_none());
            // The attributes not requested should be None.
            assert!(attrs.mutable_attributes.uid.is_none());
            assert!(attrs.mutable_attributes.gid.is_none());
            assert!(attrs.mutable_attributes.rdev.is_none());
            assert!(attrs.mutable_attributes.creation_time.is_none());
            assert!(attrs.mutable_attributes.modification_time.is_none());
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_create_file_using_flags_and_options() {
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.volume().root_dir();

            let path_str = "foo";
            let path = Path::validate_and_split(path_str).unwrap();

            let (_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let mode = fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false);
            let uid = 1;
            let gid = 2;
            let rdev = 3;
            let modification_time = Timestamp::now().as_nanos();
            let flags = fio::Flags::PROTOCOL_FILE | fio::Flags::FLAG_MAYBE_CREATE;
            let options = fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    modification_time: Some(modification_time),
                    mode: Some(mode),
                    uid: Some(uid),
                    gid: Some(gid),
                    rdev: Some(rdev),
                    ..Default::default()
                }),
                ..Default::default()
            };

            // Create file node.
            let request = ObjectRequest::new(flags, &options, server_end.into());
            let file = root_dir.lookup(&flags, path, &request).await.expect("lookup failed");

            // Verify that the node was created with the attributes requested.
            let attributes = file
                .clone()
                .into_any()
                .downcast::<FxFile>()
                .expect("Not a file")
                .get_attributes(
                    fio::NodeAttributesQuery::CREATION_TIME
                        | fio::NodeAttributesQuery::MODIFICATION_TIME
                        | fio::NodeAttributesQuery::CHANGE_TIME
                        | fio::NodeAttributesQuery::MODE
                        | fio::NodeAttributesQuery::UID
                        | fio::NodeAttributesQuery::GID
                        | fio::NodeAttributesQuery::RDEV,
                )
                .await
                .expect("FIDL call failed");
            assert_eq!(mode, attributes.mutable_attributes.mode.unwrap());
            assert_eq!(uid, attributes.mutable_attributes.uid.unwrap());
            assert_eq!(gid, attributes.mutable_attributes.gid.unwrap());
            assert_eq!(rdev, attributes.mutable_attributes.rdev.unwrap());
            assert_eq!(modification_time, attributes.mutable_attributes.modification_time.unwrap());
            assert!(attributes.mutable_attributes.creation_time.is_some());
            assert!(attributes.immutable_attributes.change_time.is_some());
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_update_attributes_also_updates_ctime() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let dir = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let (_mutable_attributes, immutable_attributes) = dir
            .get_attributes(fio::NodeAttributesQuery::CHANGE_TIME)
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("get_attributes failed");

        dir.update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(Timestamp::now().as_nanos()),
            mode: Some(111),
            gid: Some(222),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::ok)
        .expect("update_attributes failed");

        let (_mutable_attributes, immutable_attributes_after_update) = dir
            .get_attributes(fio::NodeAttributesQuery::CHANGE_TIME)
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("get_attributes failed");
        assert!(immutable_attributes_after_update.change_time > immutable_attributes.change_time);
        fixture.close().await;
    }

    async fn open_to_get_selinux_context(
        root_dir: &fio::DirectoryProxy,
        path: &str,
        protocol: fio::Flags,
    ) -> Option<fio::SelinuxContext> {
        // Reopen, querying for the value.
        let flags =
            protocol | fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PERM_GET_ATTRIBUTES;
        let options = fio::Options {
            attributes: Some(fio::NodeAttributesQuery::SELINUX_CONTEXT),
            ..Default::default()
        };
        let (node, server_end) = create_proxy::<fio::NodeMarker>();
        root_dir.open3(path, flags, &options, server_end.into_channel()).expect("Reopening node");
        let repr = node
            .take_event_stream()
            .next()
            .await
            .expect("Need representation")
            .expect("Failed to read")
            .into_on_representation()
            .unwrap();
        match repr {
            fio::Representation::Directory(fio::DirectoryInfo {
                attributes: Some(attr), ..
            })
            | fio::Representation::File(fio::FileInfo { attributes: Some(attr), .. })
            | fio::Representation::Symlink(fio::SymlinkInfo { attributes: Some(attr), .. }) => {
                attr.mutable_attributes.selinux_context
            }
            _ => panic!("Wrong type returned."),
        }
    }

    #[fuchsia::test]
    async fn test_selinux_context_via_open() {
        const CONTEXT: &str = "valid";
        const CONTEXT2: &str = "also_valid";
        let node_info: Vec<(&str, fio::Flags)> =
            vec![("dir", fio::Flags::PROTOCOL_DIRECTORY), ("file", fio::Flags::PROTOCOL_FILE)];
        let fixture = TestFixture::new().await;
        {
            let root_dir = fixture.root();

            for (path, protocol) in node_info {
                // Create node with the context.
                let flags =
                    protocol | fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::FLAG_MAYBE_CREATE;
                let options = fio::Options {
                    create_attributes: Some(fio::MutableNodeAttributes {
                        selinux_context: Some(fio::SelinuxContext::Data(CONTEXT.into())),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let (node, server_end) = create_proxy::<fio::NodeMarker>();
                root_dir
                    .open3(path, flags, &options, server_end.into_channel())
                    .expect("Creating node");
                // Check event stream to allow the creation to complete.
                assert!(node
                    .take_event_stream()
                    .next()
                    .await
                    .expect("Need representation")
                    .expect("Failed to read")
                    .into_on_representation()
                    .is_some());

                // Fetches the set value just fine.
                assert_eq!(
                    open_to_get_selinux_context(&root_dir, &path, protocol).await,
                    Some(fio::SelinuxContext::Data(CONTEXT.into()))
                );

                // See that it is synced with the xattr.
                node.set_extended_attribute(
                    fio::SELINUX_CONTEXT_NAME.as_bytes(),
                    fio::ExtendedAttributeValue::Bytes(CONTEXT2.into()),
                    fio::SetExtendedAttributeMode::Replace,
                )
                .await
                .unwrap()
                .expect("Updating xattr");
                assert_eq!(
                    open_to_get_selinux_context(&root_dir, &path, protocol).await,
                    Some(fio::SelinuxContext::Data(CONTEXT2.into()))
                );

                // Make it too long so that it must use the xattr interface.
                let vmo = zx::Vmo::create(4000).expect("Creating VMO");
                node.set_extended_attribute(
                    fio::SELINUX_CONTEXT_NAME.as_bytes(),
                    fio::ExtendedAttributeValue::Buffer(vmo),
                    fio::SetExtendedAttributeMode::Replace,
                )
                .await
                .unwrap()
                .expect("Updating xattr");
                assert_matches!(
                    open_to_get_selinux_context(&root_dir, &path, protocol).await,
                    Some(fio::SelinuxContext::UseExtendedAttributes(fio::EmptyStruct {}))
                );

                node.remove_extended_attribute(fio::SELINUX_CONTEXT_NAME.as_bytes())
                    .await
                    .unwrap()
                    .expect("Deleting xattr");
                assert_matches!(
                    open_to_get_selinux_context(&root_dir, &path, protocol).await,
                    None
                );
            }
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_selinux_context_via_open_symlink() {
        const CONTEXT: &str = "valid";
        let fixture = TestFixture::new().await;
        {
            let path = "symlink";
            let root_dir = fixture.root();
            // Create node with the context.
            let (node, server_end) = create_proxy::<fio::SymlinkMarker>();
            root_dir
                .create_symlink(path, ".".as_bytes(), Some(server_end))
                .await
                .expect("Fidl query")
                .expect("Create symlink");

            node.set_extended_attribute(
                fio::SELINUX_CONTEXT_NAME.as_bytes(),
                fio::ExtendedAttributeValue::Bytes(CONTEXT.into()),
                fio::SetExtendedAttributeMode::Create,
            )
            .await
            .unwrap()
            .expect("Updating xattr");

            // Fetches the set value just fine.
            assert_eq!(
                open_to_get_selinux_context(&root_dir, &path, fio::Flags::PROTOCOL_SYMLINK).await,
                Some(fio::SelinuxContext::Data(CONTEXT.into()))
            );

            // Make it too long so that it must use the xattr interface.
            let vmo = zx::Vmo::create(4000).expect("Creating VMO");
            node.set_extended_attribute(
                fio::SELINUX_CONTEXT_NAME.as_bytes(),
                fio::ExtendedAttributeValue::Buffer(vmo),
                fio::SetExtendedAttributeMode::Replace,
            )
            .await
            .unwrap()
            .expect("Updating xattr");
            assert_matches!(
                open_to_get_selinux_context(&root_dir, &path, fio::Flags::PROTOCOL_SYMLINK).await,
                Some(fio::SelinuxContext::UseExtendedAttributes(fio::EmptyStruct {}))
            );

            // Erase it comes back with empty string.
            node.remove_extended_attribute(fio::SELINUX_CONTEXT_NAME.as_bytes())
                .await
                .unwrap()
                .expect("Deleting xattr");
            assert_matches!(
                open_to_get_selinux_context(&root_dir, &path, fio::Flags::PROTOCOL_SYMLINK).await,
                None
            );
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_open_deleted_self() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let dir =
            open_dir_checked(&root, fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY, "foo")
                .await;

        root.unlink("foo", &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        assert_eq!(
            open_dir(&root, fio::OpenFlags::DIRECTORY, "foo")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<zx::Status>()
                .expect("No status"),
            &zx::Status::NOT_FOUND,
        );

        open_dir_checked(&dir, fio::OpenFlags::DIRECTORY, ".").await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_open3_deleted_self() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        const PATH: &str = "foo";

        let dir =
            open_dir_checked(&root, fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY, PATH).await;

        root.unlink(PATH, &fio::UnlinkOptions::default())
            .await
            .expect("FIDL call failed")
            .expect("unlink failed");

        assert_eq!(
            open3_dir(
                &root,
                fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_SEND_REPRESENTATION,
                &fio::Options::default(),
                PATH
            )
            .await
            .expect_err("Open3 succeeded unexpectedly")
            .root_cause()
            .downcast_ref::<zx::Status>()
            .expect("No status"),
            &zx::Status::NOT_FOUND,
        );

        open3_dir_checked(
            &dir,
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_SEND_REPRESENTATION,
            &fio::Options::default(),
            ".",
        )
        .await;

        fixture.close().await;
    }
}
