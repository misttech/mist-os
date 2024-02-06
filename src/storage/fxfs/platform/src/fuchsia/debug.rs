// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{component::map_to_raw_status, fuchsia::errors::map_to_status},
    async_trait::async_trait,
    fidl_fuchsia_fxfs::DebugRequest,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, Status},
    fxfs::{
        filesystem::FxFilesystem,
        lsm_tree::types::LayerIterator,
        object_handle::{ObjectHandle, ReadObjectHandle, INVALID_OBJECT_ID},
        object_store::{
            AttributeKey, DataObjectHandle, HandleOptions, ObjectKey, ObjectKeyData, ObjectStore,
        },
    },
    std::{ops::Bound, sync::Arc},
    vfs::{
        attributes,
        common::rights_to_posix_mode_bits,
        directory::{
            dirents_sink::{self, AppendResult},
            entry::DirectoryEntry,
            entry_container::Directory,
            helper::DirectlyMutable,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, File, FileIo, FileOptions, SyncMode},
        node::Node,
        ToObjectRequest,
    },
};

/// Immutable read-only access to internal Fxfs objects (attribute 0).
/// We open this as-required to avoid dealing with data that is otherwise cached in the handle
/// (specifically file size).
pub struct InternalFile {
    object_id: u64,
    store: Arc<ObjectStore>,
}

impl InternalFile {
    pub fn new(object_id: u64, store: Arc<ObjectStore>) -> Arc<Self> {
        Arc::new(Self { object_id, store })
    }

    /// Opens the file and returns a handle
    async fn handle(&self) -> Result<DataObjectHandle<ObjectStore>, zx::Status> {
        ObjectStore::open_object(&self.store, self.object_id, HandleOptions::default(), None)
            .await
            .map_err(map_to_status)
    }
}

impl DirectoryEntry for InternalFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        _path: vfs::path::Path,
        server_end: fidl::endpoints::ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            object_request.spawn_connection(
                scope.clone(),
                self.clone(),
                flags,
                FidlIoConnection::create,
            )
        });
    }

    fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
        vfs::directory::entry::EntryInfo::new(self.object_id, fio::DirentType::File)
    }
}

#[async_trait]
impl vfs::node::Node for InternalFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        let props = self.handle().await?.get_properties().await.map_err(map_to_status)?;
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ true, /*x*/ false),
            id: self.object_id,
            content_size: props.data_attribute_size,
            storage_size: props.allocated_size,
            link_count: props.refs,
            creation_time: props.creation_time.as_nanos(),
            modification_time: props.modification_time.as_nanos(),
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let props = self.handle().await?.get_properties().await.map_err(map_to_status)?;
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                id: self.object_id,
                content_size: props.data_attribute_size,
                storage_size: props.allocated_size,
                link_count: props.refs,
            }
        ))
    }

    fn close(self: Arc<Self>) {}

    fn query_filesystem(&self) -> Result<fio::FilesystemInfo, Status> {
        // Nb: self.handle() is async so we can't call it here.
        Err(zx::Status::NOT_SUPPORTED)
    }
}

impl File for InternalFile {
    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, _length: u64) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        return Err(Status::NOT_SUPPORTED);
    }

    async fn get_size(&self) -> Result<u64, Status> {
        // TODO(ripper): Look up size in LSMTree on every request.
        Ok(self.handle().await?.get_size())
    }

    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        Ok(())
    }
}

impl FileIo for InternalFile {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
        // Deal with alignment. Handle requires aligned reads.
        let handle = self.handle().await?;
        let block_size = handle.owner().block_size();
        let start = fxfs::round::round_down(offset, block_size);
        let end = fxfs::round::round_up(offset + buffer.len() as u64, block_size).unwrap();
        let mut buf = handle.allocate_buffer((end - start) as usize).await;
        let bytes = handle.read(start, buf.as_mut()).await.map_err(map_to_status)?;
        let end = std::cmp::min(offset + buffer.len() as u64, start + bytes as u64);
        if end > offset {
            buffer[..(end - offset) as usize].copy_from_slice(
                &buf.as_slice()[(offset - start) as usize..(end - start) as usize],
            );
            Ok(end - start)
        } else {
            Ok(0)
        }
    }

    async fn write_at(&self, _offset: u64, _content: &[u8]) -> Result<u64, Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

fn object_store_dir(
    store: Arc<ObjectStore>,
) -> Result<Arc<vfs::directory::immutable::Simple>, Status> {
    let root = vfs::directory::immutable::simple();

    // TODO(b/313524454): This is currently frozen at creation time. Should be made dynamic.
    let store_info_txt = format!("{:?}", store.store_info());
    root.add_entry("store_info.txt", vfs::file::vmo::read_only(store_info_txt))?;
    root.add_entry("objects", Arc::new(ObjectDirectory { store }))?;

    // TODO(b/313524454):
    //  * graveyard_dir
    //  * root_dir
    //  * '/layers/*.txt' with contents of each layer file and the in-memory layer.
    //  * '/lsm_tree' with full contents of the merged lsm_tree.
    Ok(root)
}

/// Exposes a VFS directory containing all objects in a store with a data attribute.
/// Objects are named by their object_id in decimal.
pub struct ObjectDirectory {
    store: Arc<ObjectStore>,
}

impl DirectoryEntry for ObjectDirectory {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        mut path: vfs::path::Path,
        server_end: fidl::endpoints::ServerEnd<fio::NodeMarker>,
    ) {
        // Fail if the path contains further segments.
        match path.next_with_ref() {
            (path_ref, Some(name)) => {
                // Lookup an object by id and return it.
                let path = path_ref.clone();
                let name = name.to_owned();
                let object_id = name.parse().unwrap_or(INVALID_OBJECT_ID);
                InternalFile::new(object_id, self.store.clone())
                    .open(scope, flags, path, server_end);
            }
            (_, None) => {
                flags.to_object_request(server_end).handle(|object_request| {
                    object_request.spawn_connection(
                        scope,
                        self,
                        flags,
                        vfs::directory::immutable::connection::ImmutableConnection::create,
                    )
                });
            }
        }
    }

    fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
        vfs::directory::entry::EntryInfo::new(
            self.store.store_object_id(),
            fio::DirentType::Directory,
        )
    }
}

#[async_trait]
impl Node for ObjectDirectory {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_DIRECTORY
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false),
            id: self.store.store_object_id(),
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE
                    | fio::Operations::MODIFY_DIRECTORY,
                content_size: 0,
                storage_size: 0,
                link_count: 1,
                id: self.store.store_object_id(),
            }
        ))
    }
}

#[async_trait]
impl Directory for ObjectDirectory {
    /// Reads directory entries starting from `pos` by adding them to `sink`.
    /// Once finished, should return a sealed sink.
    // The lifetimes here are because of https://github.com/rust-lang/rust/issues/63033.
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        mut sink: Box<dyn dirents_sink::Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status> {
        let object_id = match pos {
            TraversalPosition::Start => 0,
            TraversalPosition::Name(_) => return Err(zx::Status::BAD_STATE),
            TraversalPosition::Index(object_id) => *object_id,
            TraversalPosition::End => u64::MAX,
        };
        let layer_set = self.store.tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger
            .seek(Bound::Included(&ObjectKey::object(object_id)))
            .await
            .map_err(map_to_status)?;
        while let Some(data) = iter.get() {
            match data.key {
                ObjectKey {
                    object_id,
                    data: ObjectKeyData::Attribute(0, AttributeKey::Attribute),
                } => {
                    sink = match sink.append(
                        &vfs::directory::entry::EntryInfo::new(
                            *object_id,
                            fio::DirentType::Directory,
                        ),
                        &object_id.to_string(),
                    ) {
                        AppendResult::Ok(sink) => sink,
                        AppendResult::Sealed(sink) => {
                            return Ok((TraversalPosition::Index(*object_id), sink));
                        }
                    };
                }
                _ => {}
            }
            iter.advance().await.map_err(map_to_status)?;
        }

        Ok((TraversalPosition::End, sink.seal()))
    }

    fn register_watcher(
        self: Arc<Self>,
        _scope: ExecutionScope,
        _mask: fio::WatchMask,
        _watcher: vfs::directory::entry_container::DirectoryWatcher,
    ) -> Result<(), Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn unregister_watcher(self: Arc<Self>, _key: usize) {}
}

pub struct FxfsDebug {
    root: Arc<vfs::directory::immutable::Simple>,
}

impl FxfsDebug {
    pub async fn new(fs: &Arc<FxFilesystem>) -> Result<Arc<Self>, zx::Status> {
        let root = vfs::directory::immutable::simple();

        let root_parent_store = object_store_dir(fs.root_parent_store())?;
        root.add_entry("root_parent_store", root_parent_store.clone())?;
        let root_store = object_store_dir(fs.root_store())?;
        root_parent_store.add_entry("root_store", root_store.clone())?;
        root_parent_store.add_entry(
            "journal",
            InternalFile::new(fs.super_block_header().journal_object_id, fs.root_parent_store()),
        )?;

        // TODO(b/313524454): This should update dynamically.
        let superblock_header_txt = format!("{:?}", fs.super_block_header());
        root_store
            .add_entry("superblock_header.txt", vfs::file::vmo::read_only(superblock_header_txt))?;
        // TODO(b/313524454): Enumerate SuperBlockInstance::A and B.

        // TODO(b/313524454): Enumerate fs.object_manager().volumes_directory() under root_store.
        // TODO(b/313524454): Export Allocator info under root_store.

        Ok(Arc::new(Self { root }))
    }

    pub fn root(&self) -> Arc<vfs::directory::immutable::Simple> {
        self.root.clone()
    }
}

pub async fn handle_debug_request(
    fs: Arc<FxFilesystem>,
    request: DebugRequest,
) -> Result<(), fidl::Error> {
    match request {
        DebugRequest::Compact { responder } => {
            responder.send(fs.journal().compact().await.map_err(map_to_raw_status))
        }
    }
}
