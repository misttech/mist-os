// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`FxUnsealedBlob`] node type used to represent an uncompressed blob
//! in the process of being written/verified to persistent storage.

use {
    crate::fuchsia::{
        directory::FxDirectory,
        errors::map_to_status,
        fxblob::{blob::PURGED, directory::BlobDirectory},
        node::FxNode,
        volume::{info_to_filesystem_info, FxVolume},
    },
    anyhow::{anyhow, Context as _, Error},
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, NodeAttributeFlags, NodeAttributes, NodeMarker,
    },
    fuchsia_hash::Hash,
    fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::{self as zx, HandleBased as _, Status},
    fxfs::{
        errors::FxfsError,
        log::*,
        object_handle::{
            GetProperties, ObjectHandle, ObjectProperties, ReadObjectHandle, WriteObjectHandle,
        },
        object_store::{
            directory::{self, ObjectDescriptor},
            transaction::{LockKey, Options},
            StoreObjectHandle, Timestamp, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::{round_down, round_up},
        serialized_types::BlobMetadata,
    },
    lazy_static::lazy_static,
    std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    vfs::{
        attributes,
        common::rights_to_posix_mode_bits,
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, File, FileIo, FileOptions, SyncMode},
        path::Path,
        ToObjectRequest,
    },
};

lazy_static! {
    pub static ref RING_BUFFER_SIZE: u64 = 64 * (zx::system_get_page_size() as u64);
}

/// Interface for the fuchsia.storage.fxfs.BlobWriter protocol that uses a ring buffer to transfer
/// data using a VMO.
#[async_trait]
pub trait BlobWriterProtocol {
    /// Get the VMO to write to for this blob, and prepare the blob to accept `size` bytes.
    async fn get_vmo(&self, size: u64) -> Result<zx::Vmo, Error>;

    /// Signal that `bytes_written` bytes can be read out of the VMO provided by `get_vmo`.
    async fn bytes_ready(&self, bytes_written: u64) -> Result<(), Error>;
}

/// Represents a blob that is being written.
/// The blob cannot be read until writes complete and hash is verified.
/// Another blob of the same name (hash) cannot be written at the same time.
pub struct FxUnsealedBlob {
    parent: Arc<BlobDirectory>,
    hash: Hash,
    handle: StoreObjectHandle<FxVolume>,
    open_count: AtomicUsize,
    inner: Mutex<Inner>,
}

struct Inner {
    writing: bool,
    write_offset: u64,
    vmo: zx::Vmo,
    merkle_builder: Option<MerkleTreeBuilder>,
    buffer: Vec<u8>,
}

impl FxUnsealedBlob {
    pub fn new(
        parent: Arc<BlobDirectory>,
        hash: Hash,
        handle: StoreObjectHandle<FxVolume>,
    ) -> Arc<Self> {
        let file = Arc::new(Self {
            parent,
            hash,
            handle,
            open_count: AtomicUsize::new(0),
            inner: Mutex::new(Inner {
                writing: false,
                write_offset: 0,
                vmo: zx::Vmo::from_handle(zx::Handle::invalid()),
                merkle_builder: Some(MerkleTreeBuilder::new()),
                buffer: Vec::new(),
            }),
        });
        file
    }

    async fn complete(&self, size: u64, tree: MerkleTree) -> Result<(), Error> {
        self.handle.flush().await?;
        // Write the Merkle tree if it has more than a single hash.
        if tree.as_ref().len() > 1 {
            let mut hashes = Vec::new();
            for hash in &tree.as_ref()[0] {
                hashes.push(**hash);
            }
            let mut serialized = Vec::new();
            let metadata = BlobMetadata {
                hashes,
                chunk_size: 0,
                compressed_offsets: Vec::new(),
                uncompressed_size: size,
            };
            bincode::serialize_into(&mut serialized, &metadata).unwrap();
            // TODO(fxbug.dev/122125): Is this the best place to store merkle tree data?
            // (Inline attribute, prepended to data, ..?)
            self.handle.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized).await?;
        }

        // We've finished writing the blob, so promote this blob.
        let volume = self.handle.owner();
        let store = self.handle.store();

        let dir = volume
            .cache()
            .get(store.root_directory_object_id())
            .unwrap()
            .into_any()
            .downcast::<BlobDirectory>()
            .unwrap_or_else(|_| panic!("Expected blob directory"));

        let keys = [LockKey::object(store.store_object_id(), dir.object_id())];
        let mut transaction = store.filesystem().new_transaction(&keys, Options::default()).await?;

        let object_id = self.handle.object_id();
        store.remove_from_graveyard(&mut transaction, object_id);

        let name = format!("{}", self.hash);
        directory::replace_child_with_object(
            &mut transaction,
            Some((object_id, ObjectDescriptor::File)),
            (dir.directory().directory(), &name),
            0,
            Timestamp::now(),
        )
        .await?;

        let parent = self.parent().unwrap();
        transaction.commit_with_callback(|_| parent.did_add(&name)).await?;
        Ok(())
    }
}

impl DirectoryEntry for FxUnsealedBlob {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            if !path.is_empty() {
                return Err(Status::NOT_FILE);
            }
            object_request.spawn_connection(scope, self, flags, FidlIoConnection::create)
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.object_id(), fio::DirentType::File)
    }
}

#[async_trait]
impl FxNode for FxUnsealedBlob {
    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        Some(self.parent.directory().clone())
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {
        unreachable!()
    }

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        unimplemented!()
    }

    fn open_count_add_one(&self) {
        let old = self.open_count.fetch_add(1, Ordering::Relaxed);
        assert!(old != PURGED && old != PURGED - 1);
    }

    fn open_count_sub_one(self: Arc<Self>) {
        let old = self.open_count.fetch_sub(1, Ordering::Relaxed);
        assert!(old & !PURGED > 0);
        if old == PURGED + 1 {
            let store = self.handle.store();
            store
                .filesystem()
                .graveyard()
                .queue_tombstone(store.store_object_id(), self.object_id());
        }
    }
}

#[async_trait]
impl vfs::node::Node for FxUnsealedBlob {
    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        let props = self.handle.get_properties().await.map_err(map_to_status)?;
        Ok(NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ true, /*x*/ false),
            id: self.handle.object_id(),
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
    ) -> Result<fio::NodeAttributes2, Status> {
        let props = self.handle.get_properties().await.map_err(map_to_status)?;
        Ok(attributes!(
            requested_attributes,
            Mutable {
                creation_time: props.creation_time.as_nanos(),
                modification_time: props.modification_time.as_nanos(),
                mode: props.posix_attributes.map(|a| a.mode).unwrap_or(0),
                uid: props.posix_attributes.map(|a| a.uid).unwrap_or(0),
                gid: props.posix_attributes.map(|a| a.gid).unwrap_or(0),
                rdev: props.posix_attributes.map(|a| a.rdev).unwrap_or(0),
            },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                content_size: props.data_attribute_size,
                storage_size: props.allocated_size,
                link_count: props.refs,
                id: self.handle.object_id(),
            }
        ))
    }
}

#[async_trait]
impl File for FxUnsealedBlob {
    fn writable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        if length == 0 {
            // TODO(fxbug.dev/122125): There could be races when truncating concurrently.
            let tree = self.inner.lock().unwrap().merkle_builder.take().unwrap().finish();
            if tree.root() != self.hash {
                return Err(Status::IO_DATA_INTEGRITY);
            }
            return self.complete(0, tree).await.map_err(map_to_status);
        }
        // TODO(fxbug.dev/122125): This needs some locks.
        // TODO(fxbug.dev/122125): What if truncate has already been called.
        let mut transaction = self.handle.new_transaction().await.map_err(map_to_status)?;
        if let Err(e) = self
            .handle
            .preallocate_range(
                &mut transaction,
                0..round_up(length, self.handle.block_size()).unwrap(),
            )
            .await
        {
            if FxfsError::NoSpace.matches(&e) {
                let info = self.handle.store().filesystem().get_info();
                warn!(length, space = info.total_bytes - info.used_bytes, "No space for blob");
            }
            return Err(map_to_status(e));
        }
        self.handle.grow(&mut transaction, 0, length).await.map_err(map_to_status)?;
        transaction.commit().await.map_err(map_to_status)?;
        Ok(())
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Ok(self.handle.get_size())
    }

    async fn set_attrs(
        &self,
        _flags: NodeAttributeFlags,
        _attrs: NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::ACCESS_DENIED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        // TODO(fxbug.dev/122125): Implement this.
        Ok(())
    }

    fn query_filesystem(&self) -> Result<FilesystemInfo, Status> {
        let store = self.handle.store();
        Ok(info_to_filesystem_info(
            store.filesystem().get_info(),
            store.filesystem().block_size(),
            store.object_count(),
            self.handle.owner().id(),
        ))
    }

    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        unimplemented!()
    }
}

#[async_trait]
impl FileIo for FxUnsealedBlob {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
        // TODO(fxbug.dev/122125): We should disallow reading from a blob before it's verified.
        let mut buf = self.handle.allocate_buffer(buffer.len());
        let read = self.handle.read(offset, buf.as_mut()).await.map_err(map_to_status)?;
        buffer[..read].copy_from_slice(&buf.as_slice()[..read]);
        Ok(read as u64)
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.merkle_builder.is_none() || inner.writing {
                return Err(Status::BAD_STATE);
            }
            if offset != inner.write_offset {
                return Err(Status::NOT_SUPPORTED);
            }
            inner.writing = true;
        }

        // Clean up if we are dropped.
        struct CleanUp<'a>(&'a FxUnsealedBlob);
        impl Drop for CleanUp<'_> {
            fn drop(&mut self) {
                self.0.inner.lock().unwrap().writing = false;
            }
        }
        let clean_up = CleanUp(self);

        let size = self.handle.get_size();
        if content.len() as u64 > size - offset {
            return Err(Status::INVALID_ARGS);
        }

        let new_len = offset + content.len() as u64;

        let (write_offset, buffer, tree) = {
            let mut inner = self.inner.lock().unwrap();
            inner.buffer.extend_from_slice(&content);
            inner.write_offset = new_len;
            inner.merkle_builder.as_mut().unwrap().write(content);
            inner.writing = false;
            std::mem::forget(clean_up);

            let write_offset = new_len - inner.buffer.len() as u64;

            let tree = if new_len == size {
                // Verify the hash.
                let tree = inner.merkle_builder.take().unwrap().finish();
                if tree.root() != self.hash {
                    return Err(Status::IO_DATA_INTEGRITY);
                }
                Some(tree)
            } else {
                None
            };

            let buffer = if inner.buffer.len() >= 131072 || tree.is_some() {
                let bs = self.handle.block_size() as usize;
                let len = inner.buffer.len();
                let (len, aligned_len) = if tree.is_some() {
                    (len, round_down(len + bs - 1, bs))
                } else {
                    let len = round_down(len, bs);
                    (len, len)
                };
                let mut buffer = self.handle.allocate_buffer(aligned_len);
                buffer.as_mut_slice()[..len].copy_from_slice(&inner.buffer[..len]);
                buffer.as_mut_slice()[len..].fill(0);
                inner.buffer.drain(..len);
                Some(buffer)
            } else {
                None
            };

            (write_offset, buffer, tree)
        };

        if let Some(mut buffer) = buffer {
            self.handle
                .overwrite(write_offset, buffer.as_mut(), false)
                .await
                .map_err(map_to_status)?;
        }

        if let Some(tree) = tree {
            self.complete(size, tree).await.map_err(map_to_status)?;
        }

        Ok(content.len() as u64)
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[async_trait]
impl BlobWriterProtocol for FxUnsealedBlob {
    async fn get_vmo(&self, size: u64) -> Result<zx::Vmo, Error> {
        let _ = self.truncate(size).await?;
        let vmo = zx::Vmo::create(*RING_BUFFER_SIZE)?;
        let vmo_dup =
            vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("failed to duplicate VMO");
        {
            let mut inner = self.inner.lock().unwrap();
            inner.vmo = vmo;
        }
        Ok(vmo_dup)
    }

    async fn bytes_ready(&self, bytes_written: u64) -> Result<(), Error> {
        // TODO(https://fxbug.dev/126617): Remove extra copy.
        if bytes_written > *RING_BUFFER_SIZE {
            return Err(anyhow!("bytes written exceeded size of ring buffer"));
        }
        let mut buf = vec![0; bytes_written as usize];
        let write_offset;
        {
            let inner = self.inner.lock().unwrap();
            let vmo_offset = inner.write_offset % *RING_BUFFER_SIZE;
            if vmo_offset + bytes_written > *RING_BUFFER_SIZE {
                let split = (*RING_BUFFER_SIZE - vmo_offset) as usize;
                inner.vmo.read(&mut buf[0..split], vmo_offset)?;
                inner.vmo.read(&mut buf[split..], 0)?;
            } else {
                inner.vmo.read(&mut buf, vmo_offset)?;
            }
            write_offset = inner.write_offset;
        }
        self.write_at(write_offset, &buf).await.context("write_at failed")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use {
        super::*,
        crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture},
        fidl_fuchsia_fxfs, fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        rand::{thread_rng, Rng},
    };

    /// Tests for the new write API.
    #[fasync::run(10, test)]
    async fn test_new_write_empty_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![];

        // Build merkle root
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        {
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true, true)
                .await
                .expect("failed to create_and_serve the blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");
            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let _vmo = writer
                .get_vmo(data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_write_small_blob_no_wrap() {
        let fixture = new_blob_fixture().await;

        let mut data = vec![1; 196608];
        thread_rng().fill(&mut data[..]);

        // Build merkle root
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        {
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true, true)
                .await
                .expect("failed to create_and_serve the blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");

            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let vmo = writer
                .get_vmo(data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let vmo_size = vmo.get_size().expect("failed to get vmo size");
            // Write to disk using a ring buffer
            let list_of_writes =
                vec![(0, 8192), (8192, 24576), (24576, 32768), (32768, 98304), (98304, 196608)];
            let mut write_offset = 0;
            for write in list_of_writes {
                let len = (write.1 - write.0) as u64;
                vmo.write(&data[write.0..write.1], write_offset % vmo_size)
                    .expect("failed to write to vmo");
                let _ = writer
                    .bytes_ready(len)
                    .await
                    .expect("transport error on bytes_ready")
                    .expect("failed to write data to vmo");
                write_offset += len;
            }
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_write_large_blob_wraps_multiple_times() {
        let fixture = new_blob_fixture().await;

        let mut data = vec![1; 1024921];
        thread_rng().fill(&mut data[..]);

        // Build merkle root
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        {
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true, true)
                .await
                .expect("failed to create_and_serve the blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");

            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let vmo = writer
                .get_vmo(data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let vmo_size = vmo.get_size().expect("failed to get vmo size");

            // Write to disk using a ring buffer
            let list_of_writes = vec![
                (0, 8089),
                (8089, 24473),
                (24473, 57241),
                (57241, 122777),
                (122777, 253849),
                (253849, 510873),
                (510873, 767897),
                (767897, 1024921),
            ];
            let mut write_offset = 0;
            for write in list_of_writes {
                let len = write.1 - write.0;
                let vmo_offset = write_offset % vmo_size;
                let start = write.0 as usize;
                let end = write.1 as usize;
                if vmo_offset + len > vmo_size {
                    let split = vmo_size - vmo_offset;
                    vmo.write(&data[start..start + split as usize], vmo_offset)
                        .expect("failed to write to vmo");
                    vmo.write(&data[start + split as usize..end], 0)
                        .expect("failed to write to vmo");
                } else {
                    vmo.write(&data[start..end], vmo_offset).expect("failed to write to vmo");
                }
                let _ = writer
                    .bytes_ready(len)
                    .await
                    .expect("transport error on bytes_ready")
                    .expect("failed to write data to vmo");
                write_offset += len;
            }
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }
}
