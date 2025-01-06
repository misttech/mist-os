// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component::map_to_raw_status;
use crate::fuchsia::errors::map_to_status;
use crate::volumes_directory::VolumesDirectory;
use fidl_fuchsia_fxfs::DebugRequest;
use fidl_fuchsia_io as fio;
use fxfs::filesystem::FxFilesystem;
use fxfs::lsm_tree::types::LayerIterator;
use fxfs::lsm_tree::Query;
use fxfs::object_handle::{ObjectHandle, ReadObjectHandle, INVALID_OBJECT_ID};
use fxfs::object_store::{
    AttributeKey, DataObjectHandle, HandleOptions, ObjectKey, ObjectKeyData, ObjectStore,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use vfs::directory::dirents_sink::{self, AppendResult};
use vfs::directory::entry::{DirectoryEntry, GetEntryInfo, OpenRequest};
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::traversal_position::TraversalPosition;
use vfs::execution_scope::ExecutionScope;
use vfs::file::{FidlIoConnection, File, FileIo, FileLike, FileOptions, SyncMode};
use vfs::node::Node;
use vfs::{attributes, immutable_attributes, ObjectRequestRef, ToObjectRequest};
use zx::{self as zx, Status};

// To avoid dependency cycles, FxfsDebug stores weak references back to internal structures.  This
// convenience method returns an appropriate error when these internal structures are dropped.
fn upgrade_weak<T>(weak: &Weak<T>) -> Result<Arc<T>, Status> {
    weak.upgrade().ok_or(Status::CANCELED)
}

/// Immutable read-only access to internal Fxfs objects (attribute 0).
/// We open this as-required to avoid dealing with data that is otherwise cached in the handle
/// (specifically file size).
pub struct InternalFile {
    object_id: u64,
    store: Weak<ObjectStore>,
}

impl InternalFile {
    pub fn new(object_id: u64, store: Weak<ObjectStore>) -> Arc<Self> {
        Arc::new(Self { object_id, store })
    }

    /// Opens the file and returns a handle
    async fn handle(&self) -> Result<DataObjectHandle<ObjectStore>, zx::Status> {
        ObjectStore::open_object(
            &upgrade_weak(&self.store)?,
            self.object_id,
            HandleOptions::default(),
            None,
        )
        .await
        .map_err(map_to_status)
    }
}

impl DirectoryEntry for InternalFile {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_file(self)
    }
}

impl GetEntryInfo for InternalFile {
    fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
        vfs::directory::entry::EntryInfo::new(self.object_id, fio::DirentType::File)
    }
}

impl vfs::node::Node for InternalFile {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let props = self.handle().await?.get_properties().await.map_err(map_to_status)?;
        Ok(attributes!(
            requested_attributes,
            Mutable {
                creation_time: props.creation_time.as_nanos(),
                modification_time: props.modification_time.as_nanos()
            },
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

impl FileLike for InternalFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        FidlIoConnection::spawn(scope, self, options, object_request)
    }
}

/// Exposes a VFS directory containing debug entries for a given object store.
pub struct ObjectStoreDirectory {
    vfs_root: Arc<vfs::directory::immutable::Simple>,
    parent_store: Option<Weak<ObjectStore>>,
    // Only set if `parent_store` is, since we only track persistent layers here.
    layers: Option<Arc<vfs::directory::immutable::Simple>>,
}

impl ObjectStoreDirectory {
    pub fn new(
        store: Arc<ObjectStore>,
        parent_store: Option<Arc<ObjectStore>>,
    ) -> Result<Arc<Self>, Status> {
        let vfs_root = vfs::directory::immutable::simple();
        let layers_dir = if parent_store.is_some() {
            let layers_dir = vfs::directory::immutable::simple();
            vfs_root.add_entry("layers", layers_dir.clone())?;
            Some(layers_dir)
        } else {
            None
        };

        vfs_root.add_entry(
            "objects",
            Arc::new(ObjectDirectory {
                store: Arc::downgrade(&store),
                store_object_id: store.store_object_id(),
            }),
        )?;

        // TODO(b/313524454):
        //  * graveyard_dir
        //  * root_dir
        //  * '/lsm_tree' with full contents of the merged lsm_tree.

        let this = Arc::new(Self {
            vfs_root,
            parent_store: parent_store.as_ref().map(Arc::downgrade),
            layers: layers_dir,
        });
        this.update_from_store(store.as_ref())?;

        let this_clone = this.clone();
        store.set_flush_callback(move |store| {
            if let Err(e) = this_clone.update_from_store(store) {
                log::warn!(e:?; "debug: Failed to update store; debug info may be stale");
            }
        });

        Ok(this)
    }

    fn update_from_store(&self, store: &ObjectStore) -> Result<(), Status> {
        // If the store is flushed by the journal while it's still locked, store_info won't be
        // available yet.
        self.vfs_root.remove_entry("store_info.txt", false)?;
        if let Some(store_info) = store.store_info() {
            let store_info_txt = format!("{:?}", store_info);
            self.vfs_root.add_entry("store_info.txt", vfs::file::vmo::read_only(store_info_txt))?;
        }

        let (layers_dir, parent_store) = if let Some(layers) = self.layers.as_ref() {
            (layers, upgrade_weak(self.parent_store.as_ref().unwrap())?)
        } else {
            return Ok(());
        };
        layers_dir.remove_all_entries();
        let mut idx = 0;
        let layers = store
            .tree()
            .immutable_layer_set()
            .layers
            .iter()
            // NB: some layers in the immutable set might not be persistent layers yet (i.e. they
            // are sealed in-memory layers).  We still want to track the layer file indexes
            // correctly, so plumb them through as None here.
            .map(|layer| {
                layer
                    .handle()
                    .map(|h| InternalFile::new(h.object_id(), Arc::downgrade(&parent_store)))
            })
            .collect::<Vec<_>>();
        for layer in layers {
            if let Some(layer) = layer {
                layers_dir.add_entry(format!("{}", idx), layer)?;
            }
            idx += 1;
        }
        Ok(())
    }
}

/// Exposes a VFS directory containing all objects in a store with a data attribute.
/// Objects are named by their object_id in decimal.
pub struct ObjectDirectory {
    store: Weak<ObjectStore>,
    store_object_id: u64,
}

impl DirectoryEntry for ObjectDirectory {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_dir(self)
    }
}

impl GetEntryInfo for ObjectDirectory {
    fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
        vfs::directory::entry::EntryInfo::new(self.store_object_id, fio::DirentType::Directory)
    }
}

impl Node for ObjectDirectory {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        let id = upgrade_weak(&self.store)?.store_object_id();
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE
                    | fio::Operations::MODIFY_DIRECTORY,
                id: id
            }
        ))
    }
}

impl Directory for ObjectDirectory {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        mut path: vfs::path::Path,
        server_end: fidl::endpoints::ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            match path.next_with_ref() {
                (_, Some(name)) => {
                    // Lookup an object by id and return it.
                    let name = name.to_owned();
                    let object_id = name.parse().unwrap_or(INVALID_OBJECT_ID);
                    vfs::file::serve(
                        InternalFile::new(object_id, self.store.clone()),
                        scope,
                        &flags,
                        object_request,
                    )
                }
                (_, None) => object_request.spawn_connection(
                    scope,
                    self,
                    flags,
                    vfs::directory::immutable::connection::ImmutableConnection::create,
                ),
            }
        });
    }

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        mut path: vfs::path::Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        object_request.take().handle(|object_request| {
            match path.next_with_ref() {
                (_, Some(name)) => {
                    // Lookup an object by id and return it.
                    let name = name.to_owned();
                    let object_id = name.parse().unwrap_or(INVALID_OBJECT_ID);
                    vfs::file::serve(
                        InternalFile::new(object_id, self.store.clone()),
                        scope,
                        &flags,
                        object_request,
                    )
                }
                (_, None) => object_request.spawn_connection(
                    scope,
                    self,
                    flags,
                    vfs::directory::immutable::connection::ImmutableConnection::create,
                ),
            }
        });
        Ok(())
    }

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
            TraversalPosition::Bytes(_) => return Err(zx::Status::BAD_STATE),
            TraversalPosition::Index(object_id) => *object_id,
            TraversalPosition::End => u64::MAX,
        };
        let store = upgrade_weak(&self.store)?;
        let layer_set = store.tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger
            .query(Query::FullRange(&ObjectKey::object(object_id)))
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
    vfs_root: Arc<vfs::directory::immutable::Simple>,
    root_store: Weak<ObjectStore>,
    volumes_dir: Arc<vfs::directory::immutable::Simple>,
    volumes: Mutex<BTreeMap<String, Arc<ObjectStoreDirectory>>>,
}

impl FxfsDebug {
    pub fn new(
        fs: &Arc<FxFilesystem>,
        volumes: &Arc<VolumesDirectory>,
    ) -> Result<Arc<Self>, zx::Status> {
        let vfs_root = vfs::directory::immutable::simple();

        let root_parent_store = ObjectStoreDirectory::new(fs.root_parent_store(), None)?;
        vfs_root.add_entry("root_parent_store", root_parent_store.vfs_root.clone())?;
        let root_store = ObjectStoreDirectory::new(fs.root_store(), Some(fs.root_parent_store()))?;
        root_parent_store.vfs_root.add_entry("root_store", root_store.vfs_root.clone())?;
        root_parent_store.vfs_root.add_entry(
            "journal",
            InternalFile::new(
                fs.super_block_header().journal_object_id,
                Arc::downgrade(&fs.root_parent_store()),
            ),
        )?;

        // TODO(b/313524454): This should update dynamically.
        let superblock_header_txt = format!("{:?}", fs.super_block_header());
        root_store
            .vfs_root
            .add_entry("superblock_header.txt", vfs::file::vmo::read_only(superblock_header_txt))?;
        // TODO(b/313524454): Enumerate SuperBlockInstance::A and B.

        // TODO(b/313524454): Enumerate fs.object_manager().volumes_directory() under root_store to
        // find volumes which are not currently open.

        // TODO(b/313524454): Export Allocator info under root_store.

        let volumes_dir = vfs::directory::immutable::simple();
        root_store.vfs_root.add_entry("volumes", volumes_dir.clone())?;
        let this = Arc::new(Self {
            vfs_root,
            root_store: Arc::downgrade(&fs.root_store()),
            volumes_dir,
            volumes: Mutex::new(BTreeMap::new()),
        });

        let this_clone = this.clone();
        volumes.set_on_mount_callback(move |name, volume| {
            let add = volume.is_some();
            if let Err(e) = this_clone.add_volume(name, volume) {
                log::warn!(
                    "debug: Failed to {} volume in debug directory: {e:?}",
                    if add { "add" } else { "remove" }
                );
            }
        });

        Ok(this)
    }

    pub fn root(&self) -> Arc<vfs::directory::immutable::Simple> {
        self.vfs_root.clone()
    }

    fn add_volume(&self, name: &str, volume: Option<Arc<ObjectStore>>) -> Result<(), Status> {
        if let Some(volume) = volume {
            let object_store_dir =
                ObjectStoreDirectory::new(volume, Some(upgrade_weak(&self.root_store)?))?;
            let node = object_store_dir.vfs_root.clone();
            self.volumes.lock().unwrap().insert(name.to_string(), object_store_dir);
            self.volumes_dir.add_entry(name, node)
        } else {
            self.volumes.lock().unwrap().remove(name);
            self.volumes_dir.remove_entry(name, false).map(|_| ())
        }
    }
}

pub async fn handle_debug_request(
    fs: Arc<FxFilesystem>,
    volumes: Arc<VolumesDirectory>,
    request: DebugRequest,
) -> Result<(), fidl::Error> {
    match request {
        DebugRequest::Compact { responder } => {
            responder.send(fs.journal().compact().await.map_err(map_to_raw_status))
        }
        DebugRequest::DeleteProfile { responder, volume, profile } => responder
            .send(volumes.delete_profile(&volume, &profile).await.map_err(Status::into_raw)),
        DebugRequest::StopProfileTasks { responder } => {
            volumes.stop_profile_tasks().await;
            responder.send(Ok(()))
        }
    }
}
