// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Typesafe wrappers around the /blob filesystem.

use fidl::endpoints::{Proxy as _, ServerEnd};
use fuchsia_hash::{Hash, ParseHashError};
use futures::{stream, StreamExt as _};
use log::{error, info, warn};
use std::collections::HashSet;
use thiserror::Error;
use vfs::common::send_on_open_with_error;
use vfs::execution_scope::ExecutionScope;
use vfs::file::StreamIoConnection;
use vfs::{ObjectRequest, ObjectRequestRef, ProtocolsExt, ToObjectRequest as _};
use zx::{self as zx, AsHandleRef as _, Status};
use {fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg};

pub mod mock;
pub use mock::Mock;

/// Blobfs client errors.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum BlobfsError {
    #[error("while opening blobfs dir")]
    OpenDir(#[from] fuchsia_fs::node::OpenError),

    #[error("while cloning the blobfs dir")]
    CloneDir(#[from] fuchsia_fs::node::CloneError),

    #[error("while listing blobfs dir")]
    ReadDir(#[source] fuchsia_fs::directory::EnumerateError),

    #[error("while deleting blob")]
    Unlink(#[source] Status),

    #[error("while sync'ing")]
    Sync(#[source] Status),

    #[error("while parsing blob merkle hash")]
    ParseHash(#[from] ParseHashError),

    #[error("FIDL error")]
    Fidl(#[from] fidl::Error),

    #[error("while connecting to fuchsia.fxfs/BlobCreator")]
    ConnectToBlobCreator(#[source] anyhow::Error),

    #[error("while connecting to fuchsia.fxfs/BlobReader")]
    ConnectToBlobReader(#[source] anyhow::Error),

    #[error("while setting the VmexResource")]
    InitVmexResource(#[source] anyhow::Error),
}

/// An error encountered while creating a blob
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum CreateError {
    #[error("the blob already exists or is being concurrently written")]
    AlreadyExists,

    #[error("while creating the blob")]
    Io(#[source] fuchsia_fs::node::OpenError),

    #[error("while converting the proxy into a client end")]
    ConvertToClientEnd,

    #[error("FIDL error")]
    Fidl(#[from] fidl::Error),

    #[error("while calling fuchsia.fxfs/BlobCreator.Create: {0:?}")]
    BlobCreator(ffxfs::CreateBlobError),
}

impl From<ffxfs::CreateBlobError> for CreateError {
    fn from(e: ffxfs::CreateBlobError) -> Self {
        match e {
            ffxfs::CreateBlobError::AlreadyExists => CreateError::AlreadyExists,
            e @ ffxfs::CreateBlobError::Internal => CreateError::BlobCreator(e),
        }
    }
}

/// A builder for [`Client`]
#[derive(Default)]
pub struct ClientBuilder {
    use_reader: Reader,
    use_creator: bool,
    readable: bool,
    writable: bool,
    executable: bool,
}

#[derive(Default)]
enum Reader {
    #[default]
    DontUse,
    Use {
        use_vmex: bool,
    },
}

impl ClientBuilder {
    /// Opens the /blob directory in the component's namespace with readable, writable, and/or
    /// executable flags. Connects to the fuchsia.fxfs.BlobCreator and BlobReader if requested.
    /// Connects to and initializes the VmexResource if `use_vmex` is set. Returns a `Client`.
    pub async fn build(self) -> Result<Client, BlobfsError> {
        let mut flags = fio::Flags::empty();
        if self.readable {
            flags |= fio::PERM_READABLE
        }
        if self.writable {
            flags |= fio::PERM_WRITABLE
        }
        if self.executable {
            flags |= fio::PERM_EXECUTABLE
        }
        let dir = fuchsia_fs::directory::open_in_namespace("/blob", flags)?;
        let reader = match self.use_reader {
            Reader::DontUse => None,
            Reader::Use { use_vmex } => {
                if use_vmex {
                    if let Ok(client) = fuchsia_component::client::connect_to_protocol::<
                        fidl_fuchsia_kernel::VmexResourceMarker,
                    >() {
                        if let Ok(vmex) = client.get().await {
                            info!("Got vmex resource");
                            vmo_blob::init_vmex_resource(vmex)
                                .map_err(BlobfsError::InitVmexResource)?;
                        }
                    }
                }
                Some(
                    fuchsia_component::client::connect_to_protocol::<ffxfs::BlobReaderMarker>()
                        .map_err(BlobfsError::ConnectToBlobReader)?,
                )
            }
        };

        let creator = if self.use_creator {
            Some(
                fuchsia_component::client::connect_to_protocol::<ffxfs::BlobCreatorMarker>()
                    .map_err(BlobfsError::ConnectToBlobCreator)?,
            )
        } else {
            None
        };

        Ok(Client { dir, creator, reader })
    }

    /// `Client` will connect to and use fuchsia.fxfs/BlobReader for reads. Sets the VmexResource
    /// for `Client`. The VmexResource is used by `get_backing_memory` to mark blobs as executable.
    pub fn use_reader(self) -> Self {
        Self { use_reader: Reader::Use { use_vmex: true }, ..self }
    }

    /// `Client` will connect to and use fuchsia.fxfs/BlobReader for reads. Does not set the
    /// VmexResource.
    pub fn use_reader_no_vmex(self) -> Self {
        Self { use_reader: Reader::Use { use_vmex: false }, ..self }
    }

    /// If set, `Client` will connect to and use fuchsia.fxfs/BlobCreator for writes.
    pub fn use_creator(self) -> Self {
        Self { use_creator: true, ..self }
    }

    /// If set, `Client` will connect to /blob in the current component's namespace with
    /// OPEN_RIGHT_READABLE.
    pub fn readable(self) -> Self {
        Self { readable: true, ..self }
    }

    /// If set, `Client` will connect to /blob in the current component's namespace with
    /// OPEN_RIGHT_WRITABLE. WRITABLE is needed so that `delete_blob` can call
    /// fuchsia.io/Directory.Unlink.
    pub fn writable(self) -> Self {
        Self { writable: true, ..self }
    }

    /// If set, `Client` will connect to /blob in the current component's namespace with
    /// OPEN_RIGHT_EXECUTABLE.
    pub fn executable(self) -> Self {
        Self { executable: true, ..self }
    }
}

impl Client {
    /// Create an empty `ClientBuilder`
    pub fn builder() -> ClientBuilder {
        Default::default()
    }
}
/// Blobfs client
#[derive(Debug, Clone)]
pub struct Client {
    dir: fio::DirectoryProxy,
    creator: Option<ffxfs::BlobCreatorProxy>,
    reader: Option<ffxfs::BlobReaderProxy>,
}

impl Client {
    /// Returns a client connected to the given blob directory, BlobCreatorProxy, and
    /// BlobReaderProxy. If `vmex` is passed in, sets the VmexResource, which is used to mark blobs
    /// as executable. If `creator` or `reader` is not supplied, writes or reads respectively will
    /// be performed through the blob directory.
    pub fn new(
        dir: fio::DirectoryProxy,
        creator: Option<ffxfs::BlobCreatorProxy>,
        reader: Option<ffxfs::BlobReaderProxy>,
        vmex: Option<zx::Resource>,
    ) -> Result<Self, anyhow::Error> {
        if let Some(vmex) = vmex {
            vmo_blob::init_vmex_resource(vmex)?;
        }
        Ok(Self { dir, creator, reader })
    }

    /// Creates a new client backed by the returned request stream. This constructor should not be
    /// used outside of tests.
    ///
    /// # Panics
    ///
    /// Panics on error
    pub fn new_test() -> (Self, fio::DirectoryRequestStream) {
        let (dir, stream) = fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>();

        (Self { dir, creator: None, reader: None }, stream)
    }

    /// Creates a new client backed by the returned mock. This constructor should not be used
    /// outside of tests.
    ///
    /// # Panics
    ///
    /// Panics on error
    pub fn new_mock() -> (Self, mock::Mock) {
        let (dir, stream) = fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>();

        (Self { dir, creator: None, reader: None }, mock::Mock { stream })
    }

    /// Returns the read-only VMO backing the blob.
    pub async fn get_blob_vmo(&self, hash: &Hash) -> Result<zx::Vmo, GetBlobVmoError> {
        if let Some(reader) = &self.reader {
            reader
                .get_vmo(hash)
                .await
                .map_err(GetBlobVmoError::Fidl)?
                .map_err(|s| GetBlobVmoError::GetVmo(Status::from_raw(s)))
        } else {
            let blob =
                fuchsia_fs::directory::open_file(&self.dir, &hash.to_string(), fio::PERM_READABLE)
                    .await
                    .map_err(GetBlobVmoError::OpenBlob)?;
            blob.get_backing_memory(fio::VmoFlags::READ)
                .await
                .map_err(GetBlobVmoError::Fidl)?
                .map_err(|s| GetBlobVmoError::GetVmo(Status::from_raw(s)))
        }
    }

    /// Open a blob for read. `scope` will only be used if the client was configured to use
    /// fuchsia.fxfs.BlobReader.
    pub fn open_blob_for_read(
        &self,
        blob: &Hash,
        flags: fio::OpenFlags,
        scope: ExecutionScope,
        server_end: ServerEnd<fio::NodeMarker>,
    ) -> Result<(), fidl::Error> {
        let describe = flags.contains(fio::OpenFlags::DESCRIBE);
        // Reject requests that attempt to open blobs as writable.
        if flags.contains(fio::OpenFlags::RIGHT_WRITABLE) {
            send_on_open_with_error(describe, server_end, zx::Status::ACCESS_DENIED);
            return Ok(());
        }
        // Reject requests that attempt to create new blobs.
        if flags.intersects(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT) {
            send_on_open_with_error(describe, server_end, zx::Status::NOT_SUPPORTED);
            return Ok(());
        }
        // Use blob reader protocol if available, otherwise fallback to fuchsia.io/Directory.Open.
        if let Some(reader) = &self.reader {
            let object_request = flags.to_object_request(server_end);
            let () = open_blob_with_reader(reader.clone(), *blob, scope, flags, object_request);
            Ok(())
        } else {
            self.dir.deprecated_open(flags, fio::ModeType::empty(), &blob.to_string(), server_end)
        }
    }

    /// Open a blob for read using open3. `scope` will only be used if the client was configured to
    /// use fuchsia.fxfs.BlobReader.
    pub fn open3_blob_for_read(
        &self,
        blob: &Hash,
        flags: fio::Flags,
        scope: ExecutionScope,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        // Reject requests that attempt to open blobs as writable.
        if flags.rights().is_some_and(|rights| rights.contains(fio::Operations::WRITE_BYTES)) {
            return Err(zx::Status::ACCESS_DENIED);
        }
        // Reject requests that attempt to create new blobs.
        if flags.creation_mode() != vfs::CreationMode::Never {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        // Errors below will be communicated via the `object_request` channel.
        let object_request = object_request.take();
        // Use blob reader protocol if available, otherwise fallback to fuchsia.io/Directory.Open3.
        if let Some(reader) = &self.reader {
            let () = open_blob_with_reader(reader.clone(), *blob, scope, flags, object_request);
        } else {
            let _: Result<(), ()> = self
                .dir
                .open(
                    &blob.to_string(),
                    flags,
                    &object_request.options(),
                    object_request.into_channel(),
                )
                .map_err(|fidl_error| warn!("Failed to open blob {:?}: {:?}", blob, fidl_error));
        }
        Ok(())
    }

    /// Returns the list of known blobs in blobfs.
    pub async fn list_known_blobs(&self) -> Result<HashSet<Hash>, BlobfsError> {
        // fuchsia.io.Directory.ReadDirents uses a per-connection index into the array of
        // directory entries. To prevent contention over this index by concurrent calls (either
        // from concurrent calls to list_known_blobs on this object, or on clones of this object,
        // or other clones of the DirectoryProxy this object was made from), create a new
        // connection which will have its own index.
        let private_connection = fuchsia_fs::directory::clone(&self.dir)?;
        fuchsia_fs::directory::readdir(&private_connection)
            .await
            .map_err(BlobfsError::ReadDir)?
            .into_iter()
            .filter(|entry| entry.kind == fuchsia_fs::directory::DirentKind::File)
            .map(|entry| entry.name.parse().map_err(BlobfsError::ParseHash))
            .collect()
    }

    /// Delete the blob with the given merkle hash.
    pub async fn delete_blob(&self, blob: &Hash) -> Result<(), BlobfsError> {
        self.dir
            .unlink(&blob.to_string(), &fio::UnlinkOptions::default())
            .await?
            .map_err(|s| BlobfsError::Unlink(Status::from_raw(s)))
    }

    /// Open a new blob for write.
    pub async fn open_blob_for_write(&self, blob: &Hash) -> Result<fpkg::BlobWriter, CreateError> {
        Ok(if let Some(blob_creator) = &self.creator {
            fpkg::BlobWriter::Writer(blob_creator.create(blob, false).await??)
        } else {
            fpkg::BlobWriter::File(
                self.open_blob_proxy_from_dir_for_write(blob)
                    .await?
                    .into_channel()
                    .map_err(|_: fio::FileProxy| CreateError::ConvertToClientEnd)?
                    .into_zx_channel()
                    .into(),
            )
        })
    }

    /// Open a new blob for write, unconditionally using the blob directory.
    async fn open_blob_proxy_from_dir_for_write(
        &self,
        blob: &Hash,
    ) -> Result<fio::FileProxy, CreateError> {
        let flags = fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_WRITABLE | fio::PERM_READABLE;

        let path = delivery_blob::delivery_blob_path(blob);
        fuchsia_fs::directory::open_file(&self.dir, &path, flags).await.map_err(|e| match e {
            fuchsia_fs::node::OpenError::OpenError(Status::ACCESS_DENIED) => {
                CreateError::AlreadyExists
            }
            other => CreateError::Io(other),
        })
    }

    /// Returns whether blobfs has a blob with the given hash.
    /// On c++blobfs, this should only be called if there are no concurrent attempts to write the
    /// blob. On c++blobfs, open connections to even partially written blobs keep the blob alive,
    /// and so if this call overlaps with a concurrent attempt to create the blob that fails and
    /// is then retried, this open connection will prevent the partially written blob from being
    /// removed and block the creation of the new write connection.
    /// TODO(https://fxbug.dev/294286136) Add GetVmo support to c++blobfs.
    pub async fn has_blob(&self, blob: &Hash) -> bool {
        if let Some(reader) = &self.reader {
            // TODO(https://fxbug.dev/295552228): Use faster API for determining blob presence.
            matches!(reader.get_vmo(blob).await, Ok(Ok(_)))
        } else {
            let file = match fuchsia_fs::directory::open_file_async(
                &self.dir,
                &blob.to_string(),
                fio::Flags::FLAG_SEND_REPRESENTATION | fio::PERM_READABLE,
            ) {
                Ok(file) => file,
                Err(_) => return false,
            };

            let mut events = file.take_event_stream();

            let event = match events.next().await {
                None => return false,
                Some(event) => match event {
                    Err(_) => return false,
                    Ok(event) => match event {
                        fio::FileEvent::OnOpen_ { s, info } => {
                            if Status::from_raw(s) != Status::OK {
                                return false;
                            }

                            match info {
                                Some(info) => match *info {
                                    fio::NodeInfoDeprecated::File(fio::FileObject {
                                        event: Some(event),
                                        stream: _, // TODO(https://fxbug.dev/293606235): Use stream
                                    }) => event,
                                    _ => return false,
                                },
                                _ => return false,
                            }
                        }
                        fio::FileEvent::OnRepresentation { payload } => match payload {
                            fio::Representation::File(fio::FileInfo {
                                observer: Some(event),
                                stream: _, // TODO(https://fxbug.dev/293606235): Use stream
                                ..
                            }) => event,
                            _ => return false,
                        },
                        fio::FileEvent::_UnknownEvent { .. } => return false,
                    },
                },
            };

            // Check that the USER_0 signal has been asserted on the file's event to make sure we
            // return false on the edge case of the blob is current being written.
            match event.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST) {
                Ok(_) => true,
                Err(status) => {
                    if status != Status::TIMED_OUT {
                        warn!("blobfs: unknown error asserting blob existence: {}", status);
                    }
                    false
                }
            }
        }
    }

    /// Determines which blobs of `candidates` are missing from blobfs.
    /// TODO(https://fxbug.dev/338477132) This fn is used during resolves after a meta.far is
    /// fetched to determine which content blobs and subpackage meta.fars need to be fetched.
    /// On c++blobfs, opening a partially written blob keeps that blob alive, creating the
    /// following race condition:
    /// 1. blob is partially written by resolve A
    /// 2. blob is opened by this fn to check for presence by concurrent resolve B
    /// 3. resolve A encounters an error and retries the fetch, which attempts to open the blob for
    ///    write, which collides with the partially written blob from (1) that is being kept alive
    ///    by (2) and so fails
    pub async fn filter_to_missing_blobs(&self, candidates: &HashSet<Hash>) -> HashSet<Hash> {
        // Attempt to open each blob instead of using ReadDirents to catch more forms of filesystem
        // metadata corruption.
        // We don't use ReadDirents even as a pre-filter because emulator testing suggests
        // ReadDirents on an fxblob with 1,000 blobs takes as long as ~60 sequential has_blob calls
        // on missing blobs, and it's about 5x worse on c++blobfs (on which both ReadDirents is
        // slower and has_blob is faster). The minor speedup on packages with a great number of
        // missing blobs is not worth a rarely taken branch deep within package resolution.
        stream::iter(candidates.clone())
            .map(move |blob| async move {
                if self.has_blob(&blob).await {
                    None
                } else {
                    Some(blob)
                }
            })
            // Emulator testing suggests both c++blobfs and fxblob show diminishing returns after
            // even three concurrent `has_blob` calls.
            .buffer_unordered(10)
            .filter_map(|blob| async move { blob })
            .collect()
            .await
    }

    /// Call fuchsia.io/Node.Sync on the blobfs directory.
    pub async fn sync(&self) -> Result<(), BlobfsError> {
        self.dir.sync().await?.map_err(zx::Status::from_raw).map_err(BlobfsError::Sync)
    }
}

/// Spawns a task on `scope` to attempt opening `blob` via `reader`. Creates a file connection to
/// the blob using [`vmo_blob::VmoBlob`]. Errors will be sent via `object_request` asynchronously.
fn open_blob_with_reader<P: ProtocolsExt + Send>(
    reader: ffxfs::BlobReaderProxy,
    blob_hash: Hash,
    scope: ExecutionScope,
    protocols: P,
    object_request: ObjectRequest,
) {
    object_request.spawn(&scope.clone(), move |object_request| {
        Box::pin(async move {
            let get_vmo_result = reader.get_vmo(&blob_hash.into()).await.map_err(|fidl_error| {
                if let fidl::Error::ClientChannelClosed { status, .. } = fidl_error {
                    error!("Blob reader channel closed: {:?}", status);
                    status
                } else {
                    error!("Transport error on get_vmo: {:?}", fidl_error);
                    zx::Status::INTERNAL
                }
            })?;
            let vmo = get_vmo_result.map_err(zx::Status::from_raw)?;
            let vmo_blob = vmo_blob::VmoBlob::new(vmo);
            object_request.create_connection(scope, vmo_blob, protocols, StreamIoConnection::create)
        })
    });
}

#[derive(thiserror::Error, Debug)]
#[allow(missing_docs)]
pub enum GetBlobVmoError {
    #[error("getting the vmo")]
    GetVmo(#[source] Status),

    #[error("opening the blob")]
    OpenBlob(#[source] fuchsia_fs::node::OpenError),

    #[error("making a fidl request")]
    Fidl(#[source] fidl::Error),
}

#[cfg(test)]
impl Client {
    /// Constructs a new [`Client`] connected to the provided [`BlobfsRamdisk`]. Tests in this
    /// crate should use this constructor rather than [`BlobfsRamdisk::client`], which returns
    /// the non-cfg(test) build of this crate's [`blobfs::Client`]. While tests could use the
    /// [`blobfs::Client`] returned by [`BlobfsRamdisk::client`], it will be a different type than
    /// [`super::Client`], and the tests could not access its private members or any cfg(test)
    /// specific functionality.
    ///
    /// # Panics
    ///
    /// Panics on error.
    pub fn for_ramdisk(blobfs: &blobfs_ramdisk::BlobfsRamdisk) -> Self {
        Self::new(
            blobfs.root_dir_proxy().unwrap(),
            blobfs.blob_creator_proxy().unwrap(),
            blobfs.blob_reader_proxy().unwrap(),
            None,
        )
        .unwrap()
    }
}

#[cfg(test)]
#[allow(clippy::bool_assert_comparison)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use blobfs_ramdisk::BlobfsRamdisk;
    use fuchsia_async as fasync;
    use futures::stream::TryStreamExt;
    use maplit::hashset;
    use std::io::Write as _;
    use std::sync::Arc;

    #[fasync::run_singlethreaded(test)]
    async fn list_known_blobs_empty() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        assert_eq!(client.list_known_blobs().await.unwrap(), HashSet::new());
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn list_known_blobs() {
        let blobfs = BlobfsRamdisk::builder()
            .with_blob(&b"blob 1"[..])
            .with_blob(&b"blob 2"[..])
            .start()
            .await
            .unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let expected = blobfs.list_blobs().unwrap().into_iter().collect();
        assert_eq!(client.list_known_blobs().await.unwrap(), expected);
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn delete_blob_and_then_list() {
        let blobfs = BlobfsRamdisk::builder()
            .with_blob(&b"blob 1"[..])
            .with_blob(&b"blob 2"[..])
            .start()
            .await
            .unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let merkle = fuchsia_merkle::from_slice(&b"blob 1"[..]).root();
        assert_matches!(client.delete_blob(&merkle).await, Ok(()));

        let expected = hashset! {fuchsia_merkle::from_slice(&b"blob 2"[..]).root()};
        assert_eq!(client.list_known_blobs().await.unwrap(), expected);
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn delete_non_existing_blob() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);
        let blob_merkle = Hash::from([1; 32]);

        assert_matches!(
            client.delete_blob(&blob_merkle).await,
            Err(BlobfsError::Unlink(Status::NOT_FOUND))
        );
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn delete_blob_mock() {
        let (client, mut stream) = Client::new_test();
        let blob_merkle = Hash::from([1; 32]);
        fasync::Task::spawn(async move {
            match stream.try_next().await.unwrap().unwrap() {
                fio::DirectoryRequest::Unlink { name, responder, .. } => {
                    assert_eq!(name, blob_merkle.to_string());
                    responder.send(Ok(())).unwrap();
                }
                other => panic!("unexpected request: {other:?}"),
            }
        })
        .detach();

        assert_matches!(client.delete_blob(&blob_merkle).await, Ok(()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn has_blob() {
        let blobfs = BlobfsRamdisk::builder().with_blob(&b"blob 1"[..]).start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        assert_eq!(client.has_blob(&fuchsia_merkle::from_slice(&b"blob 1"[..]).root()).await, true);
        assert_eq!(client.has_blob(&Hash::from([1; 32])).await, false);

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn has_blob_fxblob() {
        let blobfs =
            BlobfsRamdisk::builder().fxblob().with_blob(&b"blob 1"[..]).start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        assert!(client.has_blob(&fuchsia_merkle::from_slice(&b"blob 1"[..]).root()).await);
        assert!(!client.has_blob(&Hash::from([1; 32])).await);

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn has_blob_return_false_if_blob_is_partially_written() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let blob = [3; 1024];
        let hash = fuchsia_merkle::from_slice(&blob).root();

        let mut file = blobfs.root_dir().unwrap().write_file(hash.to_string(), 0o777).unwrap();
        assert_eq!(client.has_blob(&hash).await, false);
        file.set_len(blob.len() as u64).unwrap();
        assert_eq!(client.has_blob(&hash).await, false);
        file.write_all(&blob[..512]).unwrap();
        assert_eq!(client.has_blob(&hash).await, false);
        file.write_all(&blob[512..]).unwrap();
        assert_eq!(client.has_blob(&hash).await, true);

        blobfs.stop().await.unwrap();
    }

    async fn resize(blob: &fio::FileProxy, size: usize) {
        let () = blob.resize(size as u64).await.unwrap().map_err(Status::from_raw).unwrap();
    }

    async fn write(blob: &fio::FileProxy, bytes: &[u8]) {
        assert_eq!(
            blob.write(bytes).await.unwrap().map_err(Status::from_raw).unwrap(),
            bytes.len() as u64
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn write_delivery_blob() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let content = [3; 1024];
        let hash = fuchsia_merkle::from_slice(&content).root();
        let delivery_content =
            delivery_blob::Type1Blob::generate(&content, delivery_blob::CompressionMode::Always);

        let proxy = client.open_blob_proxy_from_dir_for_write(&hash).await.unwrap();

        let () = resize(&proxy, delivery_content.len()).await;
        let () = write(&proxy, &delivery_content).await;

        assert!(client.has_blob(&hash).await);

        blobfs.stop().await.unwrap();
    }

    /// Wrapper for a blob and its hash. This lets the tests retain ownership of the Blob,
    /// which is important because it ensures blobfs will not close partially written blobs for the
    /// duration of the test.
    struct TestBlob {
        _blob: fio::FileProxy,
        hash: Hash,
    }

    async fn open_blob_only(client: &Client, content: &[u8]) -> TestBlob {
        let hash = fuchsia_merkle::from_slice(content).root();
        let _blob = client.open_blob_proxy_from_dir_for_write(&hash).await.unwrap();
        TestBlob { _blob, hash }
    }

    async fn open_and_truncate_blob(client: &Client, content: &[u8]) -> TestBlob {
        let hash = fuchsia_merkle::from_slice(content).root();
        let _blob = client.open_blob_proxy_from_dir_for_write(&hash).await.unwrap();
        let () = resize(&_blob, content.len()).await;
        TestBlob { _blob, hash }
    }

    async fn partially_write_blob(client: &Client, content: &[u8]) -> TestBlob {
        let hash = fuchsia_merkle::from_slice(content).root();
        let _blob = client.open_blob_proxy_from_dir_for_write(&hash).await.unwrap();
        let content = delivery_blob::generate(delivery_blob::DeliveryBlobType::Type1, content);
        let () = resize(&_blob, content.len()).await;
        let () = write(&_blob, &content[..content.len() / 2]).await;
        TestBlob { _blob, hash }
    }

    async fn fully_write_blob(client: &Client, content: &[u8]) -> TestBlob {
        let hash = fuchsia_merkle::from_slice(content).root();
        let _blob = client.open_blob_proxy_from_dir_for_write(&hash).await.unwrap();
        let content = delivery_blob::generate(delivery_blob::DeliveryBlobType::Type1, content);
        let () = resize(&_blob, content.len()).await;
        let () = write(&_blob, &content).await;
        TestBlob { _blob, hash }
    }

    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_blobs() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let missing_hash0 = Hash::from([0; 32]);
        let missing_hash1 = Hash::from([1; 32]);

        let present_blob0 = fully_write_blob(&client, &[2; 1024]).await;
        let present_blob1 = fully_write_blob(&client, &[3; 1024]).await;

        assert_eq!(
            client
                .filter_to_missing_blobs(
                    // Pass in <= 20 candidates so the heuristic is not used.
                    &hashset! { missing_hash0, missing_hash1,
                        present_blob0.hash, present_blob1.hash
                    },
                )
                .await,
            hashset! { missing_hash0, missing_hash1 }
        );

        blobfs.stop().await.unwrap();
    }

    /// Similar to the above test, except also test that partially written blobs count as missing.
    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_blobs_with_partially_written_blobs() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        // Some blobs are created (but not yet truncated).
        let missing_blob0 = open_blob_only(&client, &[0; 1024]).await;

        // Some are truncated but not written.
        let missing_blob1 = open_and_truncate_blob(&client, &[1; 1024]).await;

        // Some are partially written.
        let missing_blob2 = partially_write_blob(&client, &[2; 1024]).await;

        // Some are fully written.
        let present_blob = fully_write_blob(&client, &[3; 1024]).await;

        assert_eq!(
            client
                .filter_to_missing_blobs(&hashset! {
                    missing_blob0.hash,
                    missing_blob1.hash,
                    missing_blob2.hash,
                    present_blob.hash
                },)
                .await,
            // All partially written blobs should count as missing.
            hashset! { missing_blob0.hash, missing_blob1.hash, missing_blob2.hash }
        );

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn sync() {
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let (client, mut stream) = Client::new_test();
        fasync::Task::spawn(async move {
            match stream.try_next().await.unwrap().unwrap() {
                fio::DirectoryRequest::Sync { responder } => {
                    counter_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    responder.send(Ok(())).unwrap();
                }
                other => panic!("unexpected request: {other:?}"),
            }
        })
        .detach();

        assert_matches!(client.sync().await, Ok(()));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_blob_for_write_uses_fxblob_if_configured() {
        let (blob_creator, mut blob_creator_stream) =
            fidl::endpoints::create_proxy_and_stream::<ffxfs::BlobCreatorMarker>();
        let (blob_reader, _) = fidl::endpoints::create_proxy::<ffxfs::BlobReaderMarker>();
        let client = Client::new(
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().0,
            Some(blob_creator),
            Some(blob_reader),
            None,
        )
        .unwrap();

        fuchsia_async::Task::spawn(async move {
            match blob_creator_stream.next().await.unwrap().unwrap() {
                ffxfs::BlobCreatorRequest::Create { hash, allow_existing, responder } => {
                    assert_eq!(hash, [0; 32]);
                    assert!(!allow_existing);
                    let () = responder.send(Ok(fidl::endpoints::create_endpoints().0)).unwrap();
                }
            }
        })
        .detach();

        assert_matches!(
            client.open_blob_for_write(&[0; 32].into()).await,
            Ok(fpkg::BlobWriter::Writer(_))
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_blob_for_write_fxblob_maps_already_exists() {
        let (blob_creator, mut blob_creator_stream) =
            fidl::endpoints::create_proxy_and_stream::<ffxfs::BlobCreatorMarker>();
        let (blob_reader, _) = fidl::endpoints::create_proxy::<ffxfs::BlobReaderMarker>();

        let client = Client::new(
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().0,
            Some(blob_creator),
            Some(blob_reader),
            None,
        )
        .unwrap();

        fuchsia_async::Task::spawn(async move {
            match blob_creator_stream.next().await.unwrap().unwrap() {
                ffxfs::BlobCreatorRequest::Create { hash, allow_existing, responder } => {
                    assert_eq!(hash, [0; 32]);
                    assert!(!allow_existing);
                    let () = responder.send(Err(ffxfs::CreateBlobError::AlreadyExists)).unwrap();
                }
            }
        })
        .detach();

        assert_matches!(
            client.open_blob_for_write(&[0; 32].into()).await,
            Err(CreateError::AlreadyExists)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn concurrent_list_known_blobs_all_return_full_contents() {
        use futures::StreamExt;
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        // ReadDirents returns an 8,192 byte buffer, and each entry is 74 bytes [0] (including 64
        // bytes of filename), so use more than 110 entries to guarantee that listing all contents
        // requires multiple ReadDirents calls. This isn't necessary to cause conflict, because
        // each successful listing requires a call to Rewind as well, but it does make conflict
        // more likely.
        // [0] https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.io/directory.fidl;l=261;drc=9e84e19d3f42240c46d2b0c3c132c2f0b5a3343f
        for i in 0..256u16 {
            let _: TestBlob = fully_write_blob(&client, i.to_le_bytes().as_slice()).await;
        }

        let () = futures::stream::iter(0..100)
            .for_each_concurrent(None, |_| async {
                assert_eq!(client.list_known_blobs().await.unwrap().len(), 256);
            })
            .await;
    }
}
