// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]
#![warn(clippy::all)]
#![allow(clippy::let_unit_value)]

//! Test utilities for starting a blobfs server.

use {
    anyhow::{anyhow, Context as _, Error},
    delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob},
    fdio::{SpawnAction, SpawnOptions},
    fidl::endpoints::{ClientEnd, Proxy, ServerEnd},
    fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_merkle::{Hash, MerkleTreeBuilder},
    fuchsia_zircon::{self as zx, prelude::*},
    futures::prelude::*,
    std::{borrow::Cow, collections::BTreeSet, ffi::CString},
};

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
        let mut tree = MerkleTreeBuilder::new();
        tree.write(&bytes);
        Self { merkle: tree.finish().root(), contents: bytes }
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

#[derive(Debug)]
/// The blob filesystem implementation to use.
enum Implementation {
    /// The older C++ implementation.
    Blobfs,
    /// The newer Rust implementation that uses FxFs.
    FxBlob,
}

impl BlobfsRamdiskBuilder {
    fn new() -> Self {
        Self { ramdisk: None, blobs: vec![], implementation: Implementation::Blobfs }
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
    pub fn blobfs(self) -> Self {
        Self { implementation: Implementation::Blobfs, ..self }
    }

    /// Use the fxblob implementation of the blob file system (the newer Rust implementation built
    /// on fxfs that has a custom FIDL interface).
    pub fn fxblob(self) -> Self {
        Self { implementation: Implementation::FxBlob, ..self }
    }

    fn implementation(self, implementation: Implementation) -> Self {
        Self { implementation, ..self }
    }

    /// Starts a blobfs server with the current configuration options.
    pub async fn start(self) -> Result<BlobfsRamdisk, Error> {
        let Self { ramdisk, blobs, implementation } = self;
        let (ramdisk, needs_format) = match ramdisk {
            Some(SuppliedRamdisk::Formatted(FormattedRamdisk(ramdisk))) => (ramdisk, false),
            Some(SuppliedRamdisk::Unformatted(ramdisk)) => (ramdisk, true),
            None => (Ramdisk::start().await.context("creating backing ramdisk for blobfs")?, true),
        };

        let ramdisk_controller = Proxy::from_channel(fasync::Channel::from_channel(
            ramdisk.clone_channel().context("cloning ramdisk channel")?,
        )?);

        // Spawn blobfs on top of the ramdisk.
        let mut fs = match implementation {
            Implementation::Blobfs => fs_management::filesystem::Filesystem::new(
                ramdisk_controller,
                fs_management::Blobfs {
                    allow_delivery_blobs: true,
                    ..fs_management::Blobfs::dynamic_child()
                },
            ),
            Implementation::FxBlob => fs_management::filesystem::Filesystem::new(
                ramdisk_controller,
                fs_management::Fxfs::default(),
            ),
        };
        if needs_format {
            let () = fs.format().await.context("formatting ramdisk")?;
        }

        let fs = match implementation {
            Implementation::Blobfs => ServingFilesystem::SingleVolume(
                fs.serve().await.context("serving single volume filesystem")?,
            ),
            Implementation::FxBlob => {
                let mut fs =
                    fs.serve_multi_volume().await.context("serving multi volume filesystem")?;
                if needs_format {
                    let _: &mut fs_management::filesystem::ServingVolume = fs
                        .create_volume(
                            FXFS_BLOB_VOLUME_NAME,
                            ffxfs::MountOptions { crypt: None, as_blob: true },
                        )
                        .await
                        .context("creating blob volume")?;
                } else {
                    let _: &mut fs_management::filesystem::ServingVolume = fs
                        .open_volume(
                            FXFS_BLOB_VOLUME_NAME,
                            ffxfs::MountOptions { crypt: None, as_blob: true },
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
                    .write_blob_sync(&blob.merkle, &blob.contents)
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

    /// None if the filesystem does not support the API.
    fn blob_creator_proxy(&self) -> Result<Option<ffxfs::BlobCreatorProxy>, Error> {
        match self {
            Self::SingleVolume(_) => Ok(None),
            Self::MultiVolume(_) => fuchsia_component::client::connect_to_protocol_at_dir_svc::<
                ffxfs::BlobCreatorMarker,
            >(self.exposed_dir()?)
            .context("connecting to fuchsia.fxfs.BlobCreator")
            .map(Some),
        }
    }

    fn implementation(&self) -> Implementation {
        match self {
            Self::SingleVolume(_) => Implementation::Blobfs,
            Self::MultiVolume(_) => Implementation::FxBlob,
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
        blobfs::Client::new(self.root_dir_proxy().unwrap())
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
        Ok(self.root_dir_handle()?.into_proxy()?)
    }

    /// Returns a new connetion to blobfs's root directory as a openat::Dir.
    pub fn root_dir(&self) -> Result<openat::Dir, Error> {
        fdio::create_fd(self.root_dir_handle()?.into()).context("failed to create fd")
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
    pub fn add_blob_from(
        &self,
        merkle: &Hash,
        mut source: impl std::io::Read,
    ) -> Result<(), Error> {
        let mut bytes = vec![];
        source.read_to_end(&mut bytes)?;
        self.write_blob_sync(merkle, &bytes)
    }

    fn write_blob_sync(&self, merkle: &Hash, bytes: &[u8]) -> Result<(), Error> {
        use std::io::Write as _;
        let mut file = self.root_dir().unwrap().new_file(delivery_blob_path(merkle), 0o600)?;
        let compressed_data = Type1Blob::generate(bytes, CompressionMode::Always);
        file.set_len(compressed_data.len().try_into().unwrap())?;
        file.write_all(&compressed_data)?;
        Ok(())
    }

    /// Returns a new connection to blobfs's fuchsia.fxfs/BlobCreator API, or None if the
    /// implementation does not support it.
    pub fn blob_creator_proxy(&self) -> Result<Option<ffxfs::BlobCreatorProxy>, Error> {
        self.fs.blob_creator_proxy()
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
        let block = client.open().await?;
        let client_end = ClientEnd::<fio::NodeMarker>::new(block.into_channel());
        let proxy = client_end.into_proxy()?;
        Ok(Ramdisk { proxy, client })
    }

    /// Create a [`BlobfsRamdiskBuilder`] that uses this as its backing ramdisk.
    pub async fn into_blobfs_builder(self) -> Result<BlobfsRamdiskBuilder, Error> {
        Ok(BlobfsRamdiskBuilder::new().ramdisk(self.start().await?))
    }
}

/// A virtual memory-backed block device.
pub struct Ramdisk {
    proxy: fio::NodeProxy,
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

    fn clone_channel(&self) -> Result<zx::Channel, Error> {
        let (result, server_end) = zx::Channel::create();
        self.proxy.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, ServerEnd::new(server_end))?;
        Ok(result)
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
    /// Corrupt the blob given by merkle.
    pub async fn corrupt_blob(&self, merkle: &Hash) {
        let ramdisk = Clone::clone(&self.0.proxy);
        blobfs_corrupt_blob(ramdisk, merkle).await.unwrap();
    }

    /// Shuts down this ramdisk.
    pub async fn stop(self) -> Result<(), Error> {
        self.0.stop().await
    }
}

async fn blobfs_corrupt_blob(ramdisk: fio::NodeProxy, merkle: &Hash) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.root_dir().add_service_at("block", move |channel: zx::Channel| {
        let _: Result<(), fidl::Error> =
            ramdisk.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, channel.into());
        None
    });

    let (devfs_client, devfs_server) = fidl::endpoints::create_endpoints();
    fs.serve_connection(devfs_server)?;
    let serve_fs = fs.collect::<()>();

    let spawn_and_wait = async move {
        let p = fdio::spawn_etc(
            &fuchsia_runtime::job_default(),
            SpawnOptions::CLONE_ALL - SpawnOptions::CLONE_NAMESPACE,
            &CString::new("/pkg/bin/blobfs-corrupt").unwrap(),
            &[
                &CString::new("blobfs-corrupt").unwrap(),
                &CString::new("--device").unwrap(),
                &CString::new("/dev/block").unwrap(),
                &CString::new("--merkle").unwrap(),
                &CString::new(merkle.to_string()).unwrap(),
            ],
            None,
            &mut [SpawnAction::add_namespace_entry(
                &CString::new("/dev").unwrap(),
                devfs_client.into(),
            )],
        )
        .map_err(|(status, _)| status)
        .context("spawning 'blobfs-corrupt'")?;

        wait_for_process_async(p).await.context("'blobfs-corrupt'")?;
        Ok(())
    };

    let ((), res) = futures::join!(serve_fs, spawn_and_wait);

    res
}

async fn wait_for_process_async(proc: fuchsia_zircon::Process) -> Result<(), Error> {
    let signals =
        fuchsia_async::OnSignals::new(&proc.as_handle_ref(), zx::Signals::PROCESS_TERMINATED)
            .await
            .context("waiting for tool to terminate")?;
    assert_eq!(signals, zx::Signals::PROCESS_TERMINATED);

    let ret = proc.info().context("getting tool process info")?.return_code;
    if ret != 0 {
        return Err(anyhow!("tool returned nonzero exit code {}", ret));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, maplit::btreeset, std::io::Write};

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

        assert_eq!(blobfs.list_blobs().unwrap(), btreeset![]);

        blobfs.stop().await.unwrap();
    }

    #[test]
    fn blob_info_conversions() {
        let a = BlobInfo::from(&b"static slice"[..]);
        let b = BlobInfo::from(b"owned vec".to_vec());
        let c = BlobInfo::from(Cow::from(&b"cow"[..]));
        assert_ne!(a.merkle, b.merkle);
        assert_ne!(b.merkle, c.merkle);
        assert_eq!(
            a.merkle,
            fuchsia_merkle::MerkleTree::from_reader(&b"static slice"[..]).unwrap().root()
        );

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
        assert_eq!(blobfs.list_blobs().unwrap(), btreeset![blob.merkle]);

        let blobfs =
            blobfs.into_builder().await.unwrap().with_blob(blob.clone()).start().await.unwrap();
        assert_eq!(blobfs.list_blobs().unwrap(), btreeset![blob.merkle]);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn build_with_two_blobs() {
        let blobfs = BlobfsRamdisk::builder()
            .with_blob(&b"blob 1"[..])
            .with_blob(&b"blob 2"[..])
            .start()
            .await
            .unwrap();

        let expected = btreeset![
            fuchsia_merkle::MerkleTree::from_reader(&b"blob 1"[..]).unwrap().root(),
            fuchsia_merkle::MerkleTree::from_reader(&b"blob 2"[..]).unwrap().root(),
        ];
        assert_eq!(expected.len(), 2);
        assert_eq!(blobfs.list_blobs().unwrap(), expected);

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn blobfs_remount() {
        let blobfs =
            BlobfsRamdisk::builder().blobfs().with_blob(&b"test"[..]).start().await.unwrap();
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
        let merkle = fuchsia_merkle::MerkleTree::from_reader(payload).unwrap().root().to_string();
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
            let client_end = ramdisk.client.open().await.unwrap();
            let proxy = client_end.into_proxy().unwrap();
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
        let blobfs = BlobfsRamdisk::builder().blobfs().start().await.unwrap();

        assert!(blobfs.blob_creator_proxy().unwrap().is_none());

        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_read_and_write() {
        let blobfs = BlobfsRamdisk::builder().fxblob().start().await.unwrap();
        let root = blobfs.root_dir().unwrap();

        assert_eq!(list_blobs(&root), Vec::<String>::new());

        let hello_merkle = write_blob(&root, "Hello blobfs!".as_bytes());
        assert_eq!(list_blobs(&root), vec![hello_merkle]);

        drop(root);
        blobfs.stop().await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_blob_creator_api() {
        let blobfs = BlobfsRamdisk::builder().fxblob().start().await.unwrap();
        let root = blobfs.root_dir().unwrap();
        assert_eq!(list_blobs(&root), Vec::<String>::new());

        let bytes = [1u8; 40];
        let hash = fuchsia_merkle::MerkleTree::from_reader(&bytes[..]).unwrap().root();
        let compressed_data = Type1Blob::generate(&bytes, CompressionMode::Always);

        let blob_creator = blobfs.blob_creator_proxy().unwrap().unwrap();
        let blob_writer = blob_creator.create(&hash, false).await.unwrap().unwrap();
        let mut blob_writer = blob_writer::BlobWriter::create(
            blob_writer.into_proxy().unwrap(),
            compressed_data.len() as u64,
        )
        .await
        .unwrap();
        let () = blob_writer.write(&compressed_data).await.unwrap();

        assert_eq!(list_blobs(&root), vec![hash.to_string()]);

        drop(root);
        blobfs.stop().await.unwrap();
    }
}
