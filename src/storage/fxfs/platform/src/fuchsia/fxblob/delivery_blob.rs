// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! RFC 0207 delivery blob writing support for FxBlob.

use {
    crate::fuchsia::{
        directory::FxDirectory,
        errors::map_to_status,
        fxblob::{blob::PURGED, directory::BlobDirectory},
        node::FxNode,
        volume::info_to_filesystem_info,
        volume::FxVolume,
    },
    anyhow::{Context, Error},
    async_trait::async_trait,
    delivery_blob::{DeliveryBlobError, Type1Blob},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, NodeAttributeFlags, NodeAttributes, NodeMarker,
    },
    fuchsia_hash::Hash,
    fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::{self as zx, Status},
    futures::lock::Mutex as AsyncMutex,
    fxfs::{
        errors::FxfsError,
        log::*,
        object_handle::{ObjectHandle, ObjectProperties, WriteObjectHandle},
        object_store::{
            directory::{replace_child_with_object, ObjectDescriptor},
            transaction::{LockKey, Options},
            StoreObjectHandle, Timestamp, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::{round_down, round_up},
        serialized_types::BlobMetadata,
    },
    std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    vfs::{
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, File, FileIo, FileOptions, SyncMode},
        node::Node,
        path::Path,
        ToObjectRequest,
    },
};

const PAYLOAD_BUFFER_FLUSH_THRESHOLD: usize = 131_072; /* 128 KiB */

/// Represents an RFC-0207 compliant delivery blob that is being written.
/// The blob cannot be read until writes complete and hash is verified.
pub struct FxDeliveryBlob {
    hash: Hash,
    handle: StoreObjectHandle<FxVolume>,
    parent: Arc<BlobDirectory>,
    open_count: AtomicUsize,
    inner: AsyncMutex<Inner>,
}

struct Inner {
    /// Total number of bytes we expect for the delivery blob to be fully written. This is the same
    /// number of bytes passed to truncate. We expect this to be non-zero for a delivery blob.
    delivery_size: Option<u64>,
    /// Write offset with respect to the fuchsia.io protocol.
    delivery_bytes_written: u64,
    /// Internal buffer of data being written via the write protocols.
    buffer: Vec<u8>,
    header: Option<Type1Blob>,
    tree_builder: MerkleTreeBuilder,
    /// How many bytes from the delivery blob payload have been written to disk so far.
    payload_persisted: u64,
    /// Latched write error.
    write_error: Option<Status>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            delivery_size: None,
            delivery_bytes_written: 0,
            buffer: Default::default(),
            header: None,
            tree_builder: Default::default(),
            payload_persisted: 0,
            write_error: None,
        }
    }
}

impl Inner {
    fn header(&self) -> &Type1Blob {
        self.header.as_ref().unwrap()
    }

    // Returns true if header is completed, false if more data is required to be buffered.
    // On completion, will update `inner.buffer` to contain remaining payload bytes.
    fn parse_header(&mut self) -> Result<bool, Status> {
        if self.header.is_some() {
            return Ok(true);
        }
        match Type1Blob::parse(&self.buffer) {
            Ok((header, payload)) => {
                // TODO: Reject header if the length doesn't line up with the delivery size.
                // Add some functions to Type1Blob to validate the payload length against the
                // total size of the delivery blob, or to extract this info
                // (e.g. header.total_expected_size())
                self.header = Some(header);
                self.buffer = Vec::from(payload);
                Ok(true)
            }
            // Don't have enough data to decode the delivery blob header.
            Err(DeliveryBlobError::BufferTooSmall) => Ok(false),
            // Unsupported delivery blob type.
            Err(DeliveryBlobError::InvalidType) => Err(Status::PROTOCOL_NOT_SUPPORTED),
            // Potentially corrupted delivery blob.
            e @ Err(DeliveryBlobError::BadMagic) | e @ Err(DeliveryBlobError::IntegrityError) => {
                error!(error = ?e, "Failed to decode delivery blob");
                Err(Status::IO_DATA_INTEGRITY)
            }
        }
    }

    async fn write_payload(&mut self, handle: &StoreObjectHandle<FxVolume>) -> Result<(), Status> {
        let final_write =
            (self.payload_persisted as usize + self.buffer.len()) == self.header().payload_length;
        let block_size = handle.block_size() as usize;
        let flush_threshold = std::cmp::max(block_size, PAYLOAD_BUFFER_FLUSH_THRESHOLD);
        // If we expect more data but haven't met the flush threshold, wait for more.
        if !final_write && self.buffer.len() < flush_threshold {
            return Ok(());
        }
        let len =
            if final_write { self.buffer.len() } else { round_down(self.buffer.len(), block_size) };
        // Move data into transfer buffer, zero pad if required.
        let aligned_len = round_up(len, block_size).unwrap();
        let mut buffer = handle.allocate_buffer(aligned_len);
        buffer.as_mut_slice()[..len].copy_from_slice(&self.buffer[..len]);
        buffer.as_mut_slice()[len..].fill(0);
        self.buffer.drain(..len);
        // Update Merkle tree and overwrite allocated bytes in the object's handle.
        self.tree_builder.write(&buffer.as_slice()[..len]);
        handle
            .overwrite(self.payload_persisted, buffer.as_mut(), false)
            .await
            .map_err(map_to_status)?;
        self.payload_persisted += len as u64;
        Ok(())
    }
}

impl FxDeliveryBlob {
    pub(crate) fn new(
        parent: Arc<BlobDirectory>,
        hash: Hash,
        handle: StoreObjectHandle<FxVolume>,
    ) -> Arc<Self> {
        let file = Arc::new(Self {
            hash,
            handle,
            parent,
            open_count: AtomicUsize::new(0),
            inner: Default::default(),
        });
        file
    }

    async fn allocate(&self, size: usize) -> Result<(), Error> {
        let mut transaction = self.handle.new_transaction().await?;
        let size: u64 = size.try_into()?;
        self.handle
            .preallocate_range(
                &mut transaction,
                0..round_up(size, self.handle.block_size()).unwrap(),
            )
            .await?;
        self.handle.grow(&mut transaction, 0, size).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn complete(&self, tree: MerkleTree, uncompressed_size: u64) -> Result<(), Error> {
        let calculated_hash = tree.root();
        if calculated_hash != self.hash {
            return Err(FxfsError::Inconsistent).with_context(|| {
                format!(
                    "Calculated Merkle root ({}) does not match blob name ({})",
                    calculated_hash, self.hash
                )
            });
        }

        self.handle.flush().await?;

        // TODO(fxbug.dev/127530): We *must* write metadata if the blob is compressed.
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
                uncompressed_size,
            };
            bincode::serialize_into(&mut serialized, &metadata).unwrap();
            self.handle.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized).await?;
        }

        // Promote this blob into an actual directory entry.
        let volume = self.handle.owner();
        let store = self.handle.store();

        let dir = volume
            .cache()
            .get(store.root_directory_object_id())
            .unwrap()
            .into_any()
            .downcast::<BlobDirectory>()
            .expect("Expected blob directory");

        let keys = [LockKey::object(store.store_object_id(), dir.object_id())];
        let mut transaction = store.filesystem().new_transaction(&keys, Options::default()).await?;

        let object_id = self.handle.object_id();
        store.remove_from_graveyard(&mut transaction, object_id);

        let name = format!("{}", self.hash);
        replace_child_with_object(
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

impl DirectoryEntry for FxDeliveryBlob {
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
impl FxNode for FxDeliveryBlob {
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
impl Node for FxDeliveryBlob {
    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        Err(Status::BAD_STATE)
    }
}

#[async_trait]
impl File for FxDeliveryBlob {
    fn writable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        let mut inner = self.inner.lock().await;
        if let Some(previous_size) = inner.delivery_size {
            error!(previous_size, "truncate already called on blob");
            return Err(Status::BAD_STATE);
        }
        if length < Type1Blob::HEADER.header_length as u64 {
            error!("specified size is too small for delivery blob");
            return Err(Status::INVALID_ARGS);
        }
        inner.delivery_size = Some(length);
        Ok(())
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Err(Status::BAD_STATE)
    }

    async fn set_attrs(
        &self,
        _flags: NodeAttributeFlags,
        _attrs: NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::BAD_STATE)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        // TODO(fxbug.dev/122125): Implement sync.
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
        return Err(Status::BAD_STATE);
    }
}

#[async_trait]
impl FileIo for FxDeliveryBlob {
    async fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<u64, Status> {
        return Err(Status::BAD_STATE);
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        let mut inner = self.inner.lock().await;
        if let Some(err) = inner.write_error {
            return Err(err); // Error was latched from previous write call.
        }
        if let None = inner.delivery_size {
            error!("truncate must be called before writing blobs");
            return Err(Status::BAD_STATE);
        }
        if offset != inner.delivery_bytes_written {
            error!("only append is supported when writing blobs");
            return Err(Status::NOT_SUPPORTED);
        }
        let content_len = content.len() as u64;
        if (inner.delivery_bytes_written + content_len) > inner.delivery_size.unwrap() {
            error!("write exceeds truncated blob length");
            return Err(Status::BUFFER_TOO_SMALL);
        }
        if let Some(ref header) = inner.header {
            if (inner.payload_persisted + content_len) > header.payload_length as u64 {
                error!("write exceeds delivery header payload length");
                return Err(Status::BUFFER_TOO_SMALL);
            }
        }

        inner.buffer.extend_from_slice(content);
        inner.delivery_bytes_written += content_len;

        // Any errors occurring below will be latched as the blob will be in an unrecoverable state.
        // Writers will need to close and try to rewrite the blob.
        async {
            if inner.header.is_none() {
                if !inner.parse_header()? {
                    debug_assert!(inner.buffer.len() < Type1Blob::HEADER.header_length);
                    return Ok(content_len); // Not enough data to decode header yet, wait for more.
                }
                let header = inner.header();
                // We just finished decoding the header, allocate space for the blob payload.
                self.allocate(header.payload_length).await.map_err(|e| {
                    error!(
                        "Failed to allocate {size} bytes for delivery blob {hash}: {error:?}",
                        size = header.payload_length,
                        hash = self.hash,
                        error = e
                    );
                    map_to_status(e)
                })?;

                // TODO(fxbug.dev/127530): Need to handle the case where the payload is compressed.
                // This involves decoding the seek table, transforming it into the format fxblob
                // stores it on disk, and decompressing the payload to calculate the Merkle tree.
                if header.is_compressed {
                    error!("Compressed delivery blobs are not supported yet in fxblob.");
                    return Err(Status::NOT_SUPPORTED);
                }
            }

            debug_assert!(inner.header.is_some());
            if !inner.buffer.is_empty() {
                inner.write_payload(&self.handle).await?;
            }

            let blob_complete = inner.delivery_bytes_written == inner.delivery_size.unwrap();
            if blob_complete {
                debug_assert!(inner.payload_persisted == inner.header().payload_length as u64);
                self.complete(
                    std::mem::take(&mut inner.tree_builder).finish(),
                    inner.payload_persisted,
                )
                .await
                .map_err(|e| {
                    error!(
                        "Failed to write delivery blob {hash}: {error:?}",
                        hash = self.hash,
                        error = e
                    );
                    map_to_status(e)
                })?;
            }

            Ok(content_len)
        }
        .await
        .map_err(|err| {
            inner.write_error = Some(err);
            err
        })
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        unimplemented!("use write_at instead")
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::fuchsia::{
            fxblob::testing::{new_blob_fixture, BlobFixture},
            testing::{open_file, open_file_checked, TestFixture},
        },
        delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob},
        fidl_fuchsia_io::{
            NodeAttributeFlags, NodeAttributes, OpenFlags, VmoFlags, MAX_TRANSFER_SIZE,
        },
        fuchsia_async as fasync,
        fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
        fuchsia_zircon::Status,
    };

    async fn resize_and_write(
        fixture: &TestFixture,
        path: &str,
        payload: &[u8],
    ) -> Result<(), Status> {
        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            path,
        )
        .await;
        blob.resize(payload.len() as u64).await.expect("fidl error").map_err(Status::from_raw)?;
        for chunk in payload.chunks(MAX_TRANSFER_SIZE as usize) {
            assert_eq!(
                blob.write(&chunk).await.expect("fidl error").map_err(Status::from_raw)?,
                chunk.len() as u64
            );
        }
        Ok(())
    }

    #[fasync::run(10, test)]
    async fn test_write_empty() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();
        let payload = Type1Blob::generate(&[], CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert!(fixture.read_blob(&format!("{}", hash)).await.is_empty());
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_small() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_large() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_block_aligned() {
        let fixture = new_blob_fixture().await;
        const NUM_BLOCKS: usize = 4;
        let amount = NUM_BLOCKS * fixture.fs().root_parent_store().block_size() as usize;
        let data = vec![3; amount];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_small_chunks() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;
        blob.resize(payload.len() as u64)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap();
        // Write in batches of 4 bytes. This ensures we handle incomplete headers gracefully.
        for chunk in payload.chunks(4) {
            assert_eq!(
                blob.write(&chunk).await.expect("fidl error").map_err(Status::from_raw).unwrap(),
                chunk.len() as u64
            );
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    /// A blob should fail to write if the calculated Merkle root doesn't match the filename.
    #[fasync::run(10, test)]
    async fn test_reject_bad_hash() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 1_000];

        let correct_hash = MerkleTree::from_reader(data.as_slice()).unwrap().root().to_string();
        let incorrect_hash =
            MerkleTree::from_reader(&data[..data.len() - 1]).unwrap().root().to_string();
        assert_ne!(correct_hash, incorrect_hash);

        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        let write_error =
            resize_and_write(&fixture, &delivery_blob_path(&incorrect_hash), &payload)
                .await
                .unwrap_err();
        assert_eq!(write_error, Status::IO_DATA_INTEGRITY);

        // Ensure we can't open either hash.
        assert!(open_file(fixture.root(), OpenFlags::RIGHT_READABLE, &correct_hash).await.is_err());
        assert!(open_file(fixture.root(), OpenFlags::RIGHT_READABLE, &incorrect_hash)
            .await
            .is_err());

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_reject_write_too_many_bytes() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 1_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root().to_string();
        // Append some more bytes onto the end of payload.
        let mut payload = Type1Blob::generate(&data, CompressionMode::Never);
        let original_payload_len = payload.len() as u64;
        payload.extend_from_slice(&[1, 2, 3, 4]);

        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;
        blob.resize(original_payload_len)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap();
        assert!(payload.len() < MAX_TRANSFER_SIZE as usize);
        assert_eq!(
            Status::BUFFER_TOO_SMALL,
            blob.write(&payload).await.expect("fidl error").map_err(Status::from_raw).unwrap_err()
        );

        fixture.close().await;
    }

    /// We should fail early when truncating a delivery blob if the size is too small.
    #[fasync::run(10, test)]
    async fn test_reject_too_small() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();
        // The smallest possible delivery blob should be an uncompressed null/empty Type 1 blob.
        let payload = Type1Blob::generate(&[], CompressionMode::Never);

        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(&hash),
        )
        .await;

        let resize_error = blob
            .resize((payload.len() - 1) as u64)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap_err();
        assert_eq!(resize_error, Status::INVALID_ARGS);

        fixture.close().await;
    }

    /// Ensure we cannot call certain fuchsia.io methods while a blob is being written.
    #[fasync::run(10, test)]
    async fn test_disallowed_io_methods() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();

        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;

        assert_eq!(
            Status::BAD_STATE,
            Status::from_raw(
                blob.get_backing_memory(VmoFlags::READ).await.expect("fidl error").unwrap_err()
            )
        );

        assert_eq!(
            Status::BAD_STATE,
            Status::from_raw(blob.get_attr().await.expect("fidl error").0)
        );

        assert_eq!(
            Status::BAD_STATE,
            Status::from_raw(
                blob.set_attr(
                    NodeAttributeFlags::CREATION_TIME,
                    &NodeAttributes {
                        mode: 0,
                        id: 0,
                        content_size: 0,
                        storage_size: 0,
                        link_count: 0,
                        creation_time: 0,
                        modification_time: 0,
                    }
                )
                .await
                .expect("fidl error")
            )
        );

        fixture.close().await;
    }

    // TODO(https://fxbug.dev/127530): Support writing compressed blobs.
    #[fasync::run(10, test)]
    async fn test_reject_compressed() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000_000];
        let hash = MerkleTree::from_reader(&data[..]).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Always);

        let write_error =
            resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap_err();
        assert_eq!(write_error, Status::NOT_SUPPORTED);

        fixture.close().await;
    }
}
