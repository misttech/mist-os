// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]
#![allow(clippy::let_unit_value)]

//! Test utilities for starting a blobfs server.

use anyhow::{anyhow, Context as _, Error};
use delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_fs_startup::{CreateOptions, MountOptions};
use fuchsia_merkle::Hash;
use std::borrow::Cow;
use std::collections::BTreeSet;
use {fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio};

const RAMDISK_BLOCK_SIZE: u64 = 512;
static FXFS_BLOB_VOLUME_NAME: &str = "blob";

#[cfg(test)]
mod test;

/// A blob's hash, length, and contents.
#[derive(Debug, Clone)]
pub struct BlobInfo {
    merkle: Hash,
    contents: Cow<'static, [u8]>,
}

impl<B> From<B> for BlobInfo
where
    B: Into<Cow<'static, [u8]>>,
{
    fn from(bytes: B) -> Self {
        let bytes = bytes.into();
        Self { merkle: fuchsia_merkle::from_slice(&bytes).root(), contents: bytes }
    }
}

/// A helper to construct [`BlobfsRamdisk`] instances.
pub struct BlobfsRamdiskBuilder {
    ramdisk: Option<SuppliedRamdisk>,
    blobs: Vec<BlobInfo>,
    implementation: Implementation,
}

enum SuppliedRamdisk {
    Formatted(FormattedRamdisk),
    Unformatted(Ramdisk),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The blob filesystem implementation to use.
pub enum Implementation {
    /// The older C++ implementation.
    CppBlobfs,
    /// The newer Rust implementation that uses FxFs.
    Fxblob,
}

impl Implementation {
    /// The production blobfs implementation (and downstream decisions like whether pkg-cache
    /// should use /blob or fuchsia.fxfs/BlobCreator to write blobs) is determined by a GN
    /// variable. This function returns the implementation determined by said GN variable, so that
    /// clients inheriting production configs can create a BlobfsRamdisk backed by the appropriate
    /// implementation.
    pub fn from_env() -> Self {
        match env!("FXFS_BLOB") {
            "true" => Self::Fxblob,
            "false" => Self::CppBlobfs,
            other => panic!("unexpected value for env var 'FXFS_BLOB': {other}"),
        }
    }
}

impl BlobfsRamdiskBuilder {
    fn new() -> Self {
        Self { ramdisk: None, blobs: vec![], implementation: Implementation::CppBlobfs }
    }

    /// Configures this blobfs to use the already formatted given backing ramdisk.
    pub fn formatted_ramdisk(self, ramdisk: FormattedRamdisk) -> Self {
        Self { ramdisk: Some(SuppliedRamdisk::Formatted(ramdisk)), ..self }
    }

    /// Configures this blobfs to use the supplied unformatted ramdisk.
    pub fn ramdisk(self, ramdisk: Ramdisk) -> Self {
        Self { ramdisk: Some(SuppliedRamdisk::Unformatted(ramdisk)), ..self }
    }

    /// Write the provided blob after mounting blobfs if the blob does not already exist.
    pub fn with_blob(mut self, blob: impl Into<BlobInfo>) -> Self {
        self.blobs.push(blob.into());
        self
    }

    /// Use the blobfs implementation of the blob file system (the older C++ implementation that
    /// provides a fuchsia.io interface).
    pub fn cpp_blobfs(self) -> Self {
        Self { implementation: Implementation::CppBlobfs, ..self }
    }

    /// Use the fxblob implementation of the blob file system (the newer Rust implementation built
    /// on fxfs that has a custom FIDL interface).
    pub fn fxblob(self) -> Self {
        Self { implementation: Implementation::Fxblob, ..self }
    }

    /// Use the provided blobfs implementation.
    pub fn implementation(self, implementation: Implementation) -> Self {
        Self { implementation, ..self }
    }

    /// Use the blobfs implementation that would be active in the production configuration.
    pub fn impl_from_env(self) -> Self {
        self.implementation(Implementation::from_env())
    }

    /// Starts a blobfs server with the current configuration options.
    pub async fn start(self) -> Result<BlobfsRamdisk, Error> {
        let Self { ramdisk, blobs, implementation } = self;
        let (ramdisk, needs_format) = match ramdisk {
            Some(SuppliedRamdisk::Formatted(FormattedRamdisk(ramdisk))) => (ramdisk, false),
            Some(SuppliedRamdisk::Unformatted(ramdisk)) => (ramdisk, true),
            None => (Ramdisk::start().await.context("creating backing ramdisk for blobfs")?, true),
        };

        let ramdisk_controller = ramdisk.client.open_controller()?.into_proxy();

        // Spawn blobfs on top of the ramdisk.
        let mut fs = match implementation {
            Implementation::CppBlobfs => fs_management::filesystem::Filesystem::new(
                ramdisk_controller,
                fs_management::Blobfs { ..fs_management::Blobfs::dynamic_child() },
            ),
            Implementation::Fxblob => fs_management::filesystem::Filesystem::new(
                ramdisk_controller,
                fs_management::Fxfs::default(),
            ),
        };
        if needs_format {
            let () = fs.format().await.context("formatting ramdisk")?;
        }

        let fs = match implementation {
            Implementation::CppBlobfs => ServingFilesystem::SingleVolume(
                fs.serve().await.context("serving single volume filesystem")?,
            ),
            Implementation::Fxblob => {
                let mut fs =
                    fs.serve_multi_volume().await.context("serving multi volume filesystem")?;
                if needs_format {
                    let _: &mut fs_management::filesystem::ServingVolume = fs
                        .create_volume(
                            FXFS_BLOB_VOLUME_NAME,
                            CreateOptions::default(),
                            MountOptions { as_blob: Some(true), ..MountOptions::default() },
                        )
                        .await
                        .context("creating blob volume")?;
                } else {
                    let _: &mut fs_management::filesystem::ServingVolume = fs
                        .open_volume(
                            FXFS_BLOB_VOLUME_NAME,
                            MountOptions { as_blob: Some(true), ..MountOptions::default() },
                        )
                        .await
                        .context("opening blob volume")?;
                }
                ServingFilesystem::MultiVolume(fs)
            }
        };

        let blobfs = BlobfsRamdisk { backing_ramdisk: FormattedRamdisk(ramdisk), fs };

        // Write all the requested missing blobs to the mounted filesystem.
        if !blobs.is_empty() {
            let mut present_blobs = blobfs.list_blobs()?;

            for blob in blobs {
                if present_blobs.contains(&blob.merkle) {
                    continue;
                }
                blobfs
                    .write_blob(blob.merkle, &blob.contents)
                    .await
                    .context(format!("writing {}", blob.merkle))?;
                present_blobs.insert(blob.merkle);
            }
        }

        Ok(blobfs)
    }
}

/// A ramdisk-backed blobfs instance
pub struct BlobfsRamdisk {
    backing_ramdisk: FormattedRamdisk,
    fs: ServingFilesystem,
}

/// The old blobfs can only be served out of a single volume filesystem, but the new fxblob can
/// only be served out of a multi volume filesystem (which we create with just a single volume
/// with the name coming from `FXFS_BLOB_VOLUME_NAME`). This enum allows `BlobfsRamdisk` to
/// wrap either blobfs or fxblob.
enum ServingFilesystem {
    SingleVolume(fs_management::filesystem::ServingSingleVolumeFilesystem),
    MultiVolume(fs_management::filesystem::ServingMultiVolumeFilesystem),
}

impl ServingFilesystem {
    async fn shutdown(self) -> Result<(), Error> {
        match self {
            Self::SingleVolume(fs) => fs.shutdown().await.context("shutting down single volume"),
            Self::MultiVolume(fs) => fs.shutdown().await.context("shutting down multi volume"),
        }
    }

    fn exposed_dir(&self) -> Result<&fio::DirectoryProxy, Error> {
        match self {
            Self::SingleVolume(fs) => Ok(fs.exposed_dir()),
            Self::MultiVolume(fs) => Ok(fs
                .volume(FXFS_BLOB_VOLUME_NAME)
                .ok_or(anyhow!("missing blob volume"))?
                .exposed_dir()),
        }
    }

    /// The name of the blob root directory in the exposed directory.
    fn blob_dir_name(&self) -> &'static str {
        match self {
            Self::SingleVolume(_) => "blob-exec",
            Self::MultiVolume(_) => "root",
        }
    }

    /// None if the filesystem does not expose any services.
    fn svc_dir(&self) -> Result<Option<fio::DirectoryProxy>, Error> {
        match self {
            Self::SingleVolume(_) => Ok(None),
            Self::MultiVolume(_) => Ok(Some(
                fuchsia_fs::directory::open_directory_async(
                    self.exposed_dir()?,
                    "svc",
                    fio::PERM_READABLE,
                )
                .context("opening svc dir")?,
            )),
        }
    }

    /// None if the filesystem does not support the API.
    fn blob_creator_proxy(&self) -> Result<Option<ffxfs::BlobCreatorProxy>, Error> {
        Ok(match self.svc_dir()? {
            Some(d) => Some(
                fuchsia_component::client::connect_to_protocol_at_dir_root::<
                    ffxfs::BlobCreatorMarker,
                >(&d)
                .context("connecting to fuchsia.fxfs.BlobCreator")?,
            ),
            None => None,
        })
    }

    /// None if the filesystem does not support the API.
    fn blob_reader_proxy(&self) -> Result<Option<ffxfs::BlobReaderProxy>, Error> {
        Ok(match self.svc_dir()? {
            Some(d) => {
                Some(
                    fuchsia_component::client::connect_to_protocol_at_dir_root::<
                        ffxfs::BlobReaderMarker,
                    >(&d)
                    .context("connecting to fuchsia.fxfs.BlobReader")?,
                )
            }
            None => None,
        })
    }

    fn implementation(&self) -> Implementation {
        match self {
            Self::SingleVolume(_) => Implementation::CppBlobfs,
            Self::MultiVolume(_) => Implementation::Fxblob,
        }
    }
}

impl BlobfsRamdisk {
    /// Creates a new [`BlobfsRamdiskBuilder`] with no pre-configured ramdisk.
    pub fn builder() -> BlobfsRamdiskBuilder {
        BlobfsRamdiskBuilder::new()
    }

    /// Starts a blobfs server backed by a freshly formatted ramdisk.
    pub async fn start() -> Result<Self, Error> {
        Self::builder().start().await
    }

    /// Returns a new connection to blobfs using the blobfs::Client wrapper type.
    ///
    /// # Panics
    ///
    /// Panics on error
    pub fn client(&self) -> blobfs::Client {
        blobfs::Client::new(
            self.root_dir_proxy().unwrap(),
            self.blob_creator_proxy().unwrap(),
            self.blob_reader_proxy().unwrap(),
            None,
        )
        .unwrap()
    }

    /// Returns a new connection to blobfs's root directory as a raw zircon channel.
    pub fn root_dir_handle(&self) -> Result<ClientEnd<fio::DirectoryMarker>, Error> {
        let (root_clone, server_end) = zx::Channel::create();
        self.fs.exposed_dir()?.open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            fio::ModeType::empty(),
            self.fs.blob_dir_name(),
            server_end.into(),
        )?;
        Ok(root_clone.into())
    }

    /// Returns a new connection to blobfs's root directory as a DirectoryProxy.
    pub fn root_dir_proxy(&self) -> Result<fio::DirectoryProxy, Error> {
        Ok(self.root_dir_handle()?.into_proxy())
    }

    /// Returns a new connection to blobfs's root directory as a openat::Dir.
    pub fn root_dir(&self) -> Result<openat::Dir, Error> {
        use std::os::fd::{FromRawFd as _, IntoRawFd as _, OwnedFd};

        let fd: OwnedFd =
            fdio::create_fd(self.root_dir_handle()?.into()).context("failed to create fd")?;

        // SAFETY: `openat::Dir` requires that the file descriptor is a directory, which we are
        // guaranteed because `root_dir_handle()` implements the directory FIDL interface. There is
        // not a direct way to transfer ownership from an `OwnedFd` to `openat::Dir`, so we need to
        // convert the fd into a `RawFd` before handing it off to `Dir`.
        unsafe { Ok(openat::Dir::from_raw_fd(fd.into_raw_fd())) }
    }

    /// Signals blobfs to unmount and waits for it to exit cleanly, returning a new
    /// [`BlobfsRamdiskBuilder`] initialized with the ramdisk.
    pub async fn into_builder(self) -> Result<BlobfsRamdiskBuilder, Error> {
        let implementation = self.fs.implementation();
        let ramdisk = self.unmount().await?;
        Ok(Self::builder().formatted_ramdisk(ramdisk).implementation(implementation))
    }

    /// Signals blobfs to unmount and waits for it to exit cleanly, returning the backing Ramdisk.
    pub async fn unmount(self) -> Result<FormattedRamdisk, Error> {
        self.fs.shutdown().await?;
        Ok(self.backing_ramdisk)
    }

    /// Signals blobfs to unmount and waits for it to exit cleanly, stopping the inner ramdisk.
    pub async fn stop(self) -> Result<(), Error> {
        self.unmount().await?.stop().await
    }

    /// Returns a sorted list of all blobs present in this blobfs instance.
    pub fn list_blobs(&self) -> Result<BTreeSet<Hash>, Error> {
        self.root_dir()?
            .list_dir(".")?
            .map(|entry| {
                Ok(entry?
                    .file_name()
                    .to_str()
                    .ok_or_else(|| anyhow!("expected valid utf-8"))?
                    .parse()?)
            })
            .collect()
    }

    /// Writes the blob to blobfs.
    pub async fn add_blob_from(
        &self,
        merkle: Hash,
        mut source: impl std::io::Read,
    ) -> Result<(), Error> {
        let mut bytes = vec![];
        source.read_to_end(&mut bytes)?;
        self.write_blob(merkle, &bytes).await
    }

    /// Writes a blob with hash `merkle` and blob contents `bytes` to blobfs. `bytes` should be
    /// uncompressed. Ignores AlreadyExists errors.
    pub async fn write_blob(&self, merkle: Hash, bytes: &[u8]) -> Result<(), Error> {
        let compressed_data = Type1Blob::generate(bytes, CompressionMode::Attempt);
        match self.fs {
            ServingFilesystem::SingleVolume(_) => {
                use std::io::Write as _;
                let mut file =
                    match self.root_dir().unwrap().new_file(delivery_blob_path(merkle), 0o600) {
                        Ok(file) => file,
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            // blob is being written or already written
                            return Ok(());
                        }
                        Err(e) => {
                            return Err(e.into());
                        }
                    };
                file.set_len(compressed_data.len().try_into().unwrap())?;
                file.write_all(&compressed_data)?;
            }
            ServingFilesystem::MultiVolume(_) => {
                let blob_creator = self.blob_creator_proxy()?.ok_or_else(|| {
                    anyhow!("The filesystem does not expose the BlobCreator service")
                })?;
                let writer_client_end = match blob_creator.create(&merkle.into(), false).await? {
                    Ok(writer_client_end) => writer_client_end,
                    Err(ffxfs::CreateBlobError::AlreadyExists) => {
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(anyhow!("create blob error {:?}", e));
                    }
                };
                let writer = writer_client_end.into_proxy();
                let mut blob_writer =
                    blob_writer::BlobWriter::create(writer, compressed_data.len() as u64)
                        .await
                        .context("failed to create BlobWriter")?;
                blob_writer.write(&compressed_data).await?;
            }
        }
        Ok(())
    }

    /// Returns a connection to blobfs's exposed "svc" directory, or None if the
    /// implementation does not expose any services.
    /// More convenient than using `blob_creator_proxy` directly when forwarding the service
    /// to RealmBuilder components.
    pub fn svc_dir(&self) -> Result<Option<fio::DirectoryProxy>, Error> {
        self.fs.svc_dir()
    }

    /// Returns a new connection to blobfs's fuchsia.fxfs/BlobCreator API, or None if the
    /// implementation does not support it.
    pub fn blob_creator_proxy(&self) -> Result<Option<ffxfs::BlobCreatorProxy>, Error> {
        self.fs.blob_creator_proxy()
    }

    /// Returns a new connection to blobfs's fuchsia.fxfs/BlobReader API, or None if the
    /// implementation does not support it.
    pub fn blob_reader_proxy(&self) -> Result<Option<ffxfs::BlobReaderProxy>, Error> {
        self.fs.blob_reader_proxy()
    }
}

/// A helper to construct [`Ramdisk`] instances.
pub struct RamdiskBuilder {
    block_count: u64,
}

impl RamdiskBuilder {
    fn new() -> Self {
        Self { block_count: 1 << 20 }
    }

    /// Set the block count of the [`Ramdisk`].
    pub fn block_count(mut self, block_count: u64) -> Self {
        self.block_count = block_count;
        self
    }

    /// Starts a new ramdisk.
    pub async fn start(self) -> Result<Ramdisk, Error> {
        let client = ramdevice_client::RamdiskClient::builder(RAMDISK_BLOCK_SIZE, self.block_count);
        let client = client.build().await?;
        Ok(Ramdisk { client })
    }

    /// Create a [`BlobfsRamdiskBuilder`] that uses this as its backing ramdisk.
    pub async fn into_blobfs_builder(self) -> Result<BlobfsRamdiskBuilder, Error> {
        Ok(BlobfsRamdiskBuilder::new().ramdisk(self.start().await?))
    }
}

/// A virtual memory-backed block device.
pub struct Ramdisk {
    client: ramdevice_client::RamdiskClient,
}

// FormattedRamdisk Derefs to Ramdisk, which is only safe if all of the &self Ramdisk methods
// preserve the blobfs formatting.
impl Ramdisk {
    /// Create a RamdiskBuilder that defaults to 1024 * 1024 blocks of size 512 bytes.
    pub fn builder() -> RamdiskBuilder {
        RamdiskBuilder::new()
    }

    /// Starts a new ramdisk with 1024 * 1024 blocks and a block size of 512 bytes, resulting in a
    /// drive with 512MiB capacity.
    pub async fn start() -> Result<Self, Error> {
        Self::builder().start().await
    }

    /// Shuts down this ramdisk.
    pub async fn stop(self) -> Result<(), Error> {
        self.client.destroy().await
    }
}

/// A [`Ramdisk`] formatted for use by blobfs.
pub struct FormattedRamdisk(Ramdisk);

// This is safe as long as all of the &self methods of Ramdisk maintain the blobfs formatting.
impl std::ops::Deref for FormattedRamdisk {
    type Target = Ramdisk;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FormattedRamdisk {
    /// Shuts down this ramdisk.
    pub async fn stop(self) -> Result<(), Error> {
        self.0.stop().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn clean_start_and_stop() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();

        let proxy = blobfs.root_dir_proxy().unwrap();
        drop(proxy);

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn clean_start_contains_no_blobs() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();

        assert_eq!(blobfs.list_blobs().unwrap(), BTreeSet::new());

        blobfs.stop().await.unwrap();
    }

    #[test]
    fn blob_info_conversions() {
        let a = BlobInfo::from(&b"static slice"[..]);
        let b = BlobInfo::from(b"owned vec".to_vec());
        let c = BlobInfo::from(Cow::from(&b"cow"[..]));
        assert_ne!(a.merkle, b.merkle);
        assert_ne!(b.merkle, c.merkle);
        assert_eq!(a.merkle, fuchsia_merkle::from_slice(&b"static slice"[..]).root());

        // Verify the following calling patterns build, but don't bother building the ramdisk.
        let _ = BlobfsRamdisk::builder()
            .with_blob(&b"static slice"[..])
            .with_blob(b"owned vec".to_vec())
            .with_blob(Cow::from(&b"cow"[..]));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn with_blob_ignores_duplicates() {
        let blob = BlobInfo::from(&b"duplicate"[..]);

        let blobfs = BlobfsRamdisk::builder()
            .with_blob(blob.clone())
            .with_blob(blob.clone())
            .start()
            .await
            .unwrap();
        assert_eq!(blobfs.list_blobs().unwrap(), BTreeSet::from([blob.merkle]));

        let blobfs =
            blobfs.into_builder().await.unwrap().with_blob(blob.clone()).start().await.unwrap();
        assert_eq!(blobfs.list_blobs().unwrap(), BTreeSet::from([blob.merkle]));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn build_with_two_blobs() {
        let blobfs = BlobfsRamdisk::builder()
            .with_blob(&b"blob 1"[..])
            .with_blob(&b"blob 2"[..])
            .start()
            .await
            .unwrap();

        let expected = BTreeSet::from([
            fuchsia_merkle::from_slice(&b"blob 1"[..]).root(),
            fuchsia_merkle::from_slice(&b"blob 2"[..]).root(),
        ]);
        assert_eq!(expected.len(), 2);
        assert_eq!(blobfs.list_blobs().unwrap(), expected);

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn blobfs_remount() {
        let blobfs =
            BlobfsRamdisk::builder().cpp_blobfs().with_blob(&b"test"[..]).start().await.unwrap();
        let blobs = blobfs.list_blobs().unwrap();

        let blobfs = blobfs.into_builder().await.unwrap().start().await.unwrap();

        assert_eq!(blobs, blobfs.list_blobs().unwrap());

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_remount() {
        let blobfs =
            BlobfsRamdisk::builder().fxblob().with_blob(&b"test"[..]).start().await.unwrap();
        let blobs = blobfs.list_blobs().unwrap();

        let blobfs = blobfs.into_builder().await.unwrap().start().await.unwrap();

        assert_eq!(blobs, blobfs.list_blobs().unwrap());

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn blob_appears_in_readdir() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let root = blobfs.root_dir().unwrap();

        let hello_merkle = write_blob(&root, "Hello blobfs!".as_bytes());
        assert_eq!(list_blobs(&root), vec![hello_merkle]);

        drop(root);
        blobfs.stop().await.unwrap();
    }

    /// Writes a blob to blobfs, returning the computed merkle root of the blob.
    #[allow(clippy::zero_prefixed_literal)]
    fn write_blob(dir: &openat::Dir, payload: &[u8]) -> String {
        let merkle = fuchsia_merkle::from_slice(payload).root().to_string();
        let compressed_data = Type1Blob::generate(payload, CompressionMode::Always);
        let mut f = dir.new_file(delivery_blob_path(&merkle), 0600).unwrap();
        f.set_len(compressed_data.len() as u64).unwrap();
        f.write_all(&compressed_data).unwrap();

        merkle
    }

    /// Returns an unsorted list of blobs in the given blobfs dir.
    fn list_blobs(dir: &openat::Dir) -> Vec<String> {
        dir.list_dir(".")
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_owned().into_string().unwrap())
            .collect()
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn ramdisk_builder_sets_block_count() {
        for block_count in [1, 2, 3, 16] {
            let ramdisk = Ramdisk::builder().block_count(block_count).start().await.unwrap();
            let client_end = ramdisk.client.open().unwrap();
            let proxy = client_end.into_proxy();
            let info = proxy.get_info().await.unwrap().unwrap();
            assert_eq!(info.block_count, block_count);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn ramdisk_into_blobfs_formats_ramdisk() {
        let _: BlobfsRamdisk =
            Ramdisk::builder().into_blobfs_builder().await.unwrap().start().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn blobfs_does_not_support_blob_creator_api() {
        let blobfs = BlobfsRamdisk::builder().cpp_blobfs().start().await.unwrap();

        assert!(blobfs.blob_creator_proxy().unwrap().is_none());

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn blobfs_does_not_support_blob_reader_api() {
        let blobfs = BlobfsRamdisk::builder().cpp_blobfs().start().await.unwrap();

        assert!(blobfs.blob_reader_proxy().unwrap().is_none());

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_read_and_write() {
        let blobfs = BlobfsRamdisk::builder().fxblob().start().await.unwrap();
        let root = blobfs.root_dir().unwrap();

        assert_eq!(list_blobs(&root), Vec::<String>::new());
        let data = "Hello blobfs!".as_bytes();
        let merkle = fuchsia_merkle::from_slice(data).root();
        blobfs.write_blob(merkle, data).await.unwrap();

        assert_eq!(list_blobs(&root), vec![merkle.to_string()]);

        drop(root);
        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_blob_creator_api() {
        let blobfs = BlobfsRamdisk::builder().fxblob().start().await.unwrap();
        let root = blobfs.root_dir().unwrap();
        assert_eq!(list_blobs(&root), Vec::<String>::new());

        let bytes = [1u8; 40];
        let hash = fuchsia_merkle::from_slice(&bytes).root();
        let compressed_data = Type1Blob::generate(&bytes, CompressionMode::Always);

        let blob_creator = blobfs.blob_creator_proxy().unwrap().unwrap();
        let blob_writer = blob_creator.create(&hash, false).await.unwrap().unwrap();
        let mut blob_writer =
            blob_writer::BlobWriter::create(blob_writer.into_proxy(), compressed_data.len() as u64)
                .await
                .unwrap();
        let () = blob_writer.write(&compressed_data).await.unwrap();

        assert_eq!(list_blobs(&root), vec![hash.to_string()]);

        drop(root);
        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_blob_reader_api() {
        let data = "Hello blobfs!".as_bytes();
        let hash = fuchsia_merkle::from_slice(data).root();
        let blobfs = BlobfsRamdisk::builder().fxblob().with_blob(data).start().await.unwrap();

        let root = blobfs.root_dir().unwrap();
        assert_eq!(list_blobs(&root), vec![hash.to_string()]);

        let blob_reader = blobfs.blob_reader_proxy().unwrap().unwrap();
        let vmo = blob_reader.get_vmo(&hash.into()).await.unwrap().unwrap();
        let mut buf = vec![0; vmo.get_content_size().unwrap() as usize];
        let () = vmo.read(&mut buf, 0).unwrap();
        assert_eq!(buf, data);

        drop(root);
        blobfs.stop().await.unwrap();
    }
}
