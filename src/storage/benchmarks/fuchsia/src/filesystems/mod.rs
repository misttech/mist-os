// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    delivery_blob::{CompressionMode, Type1Blob},
    either::Either,
    fidl_fuchsia_fxfs::MountOptions,
    fidl_fuchsia_io as fio,
    fs_management::{
        filesystem::{ServingMultiVolumeFilesystem, ServingSingleVolumeFilesystem},
        FSConfig,
    },
    fuchsia_merkle::{Hash, MerkleTree},
    std::path::Path,
    storage_benchmarks::{block_device::BlockDevice, CacheClearableFilesystem, Filesystem},
};

mod blobfs;
mod f2fs;
mod fxblob;
mod fxfs;
mod memfs;
mod minfs;
#[cfg(test)]
mod testing;

pub use {blobfs::Blobfs, f2fs::F2fs, fxblob::Fxblob, fxfs::Fxfs, memfs::Memfs, minfs::Minfs};

const MOUNT_PATH: &str = "/benchmark";

/// Struct for holding the name of a blob and its contents in the delivery blob format.
pub struct DeliveryBlob {
    pub data: Vec<u8>,
    pub name: Hash,
}

impl DeliveryBlob {
    pub fn new(data: Vec<u8>, mode: CompressionMode) -> Self {
        let name = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        Self { data: Type1Blob::generate(&data, mode), name }
    }
}

/// A trait for filesystems that support reading and writing blobs.
#[async_trait]
pub trait BlobFilesystem: CacheClearableFilesystem {
    /// Writes a blob to the filesystem.
    ///
    /// Blobfs and Fxblob write blobs using different protocols. How a blob is written is
    /// implemented in the filesystem so benchmarks don't have to know which protocol to use.
    async fn write_blob(&self, blob: &DeliveryBlob);
}

pub struct FsManagementFilesystemInstance {
    fs: fs_management::filesystem::Filesystem,
    serving_filesystem: Option<Either<ServingSingleVolumeFilesystem, ServingMultiVolumeFilesystem>>,
    as_blob: bool,
    // Keep the underlying block device alive for as long as we are using the filesystem.
    _block_device: Box<dyn BlockDevice>,
}

impl FsManagementFilesystemInstance {
    pub async fn new<FSC: FSConfig>(
        config: FSC,
        block_device: Box<dyn BlockDevice>,
        as_blob: bool,
    ) -> Self {
        let mut fs =
            fs_management::filesystem::Filesystem::new(block_device.controller().clone(), config);
        fs.format().await.expect("Failed to format the filesystem");
        let serving_filesystem = if fs.config().is_multi_volume() {
            let mut serving_filesystem =
                fs.serve_multi_volume().await.expect("Failed to start the filesystem");
            let vol = serving_filesystem
                .create_volume(
                    "default",
                    MountOptions { crypt: fs.config().crypt_client().map(|c| c.into()), as_blob },
                )
                .await
                .expect("Failed to create volume");
            vol.bind_to_path(MOUNT_PATH).expect("Failed to bind the volume");
            Either::Right(serving_filesystem)
        } else {
            let mut serving_filesystem = fs.serve().await.expect("Failed to start the filesystem");
            serving_filesystem.bind_to_path(MOUNT_PATH).expect("Failed to bind the filesystem");
            Either::Left(serving_filesystem)
        };
        Self {
            fs,
            serving_filesystem: Some(serving_filesystem),
            _block_device: block_device,
            as_blob,
        }
    }

    fn exposed_dir(&self) -> &fio::DirectoryProxy {
        let fs = self.serving_filesystem.as_ref().unwrap();
        match fs {
            Either::Left(serving_filesystem) => serving_filesystem.exposed_dir(),
            Either::Right(serving_filesystem) => {
                serving_filesystem.volume("default").unwrap().exposed_dir()
            }
        }
    }
}

#[async_trait]
impl Filesystem for FsManagementFilesystemInstance {
    async fn shutdown(self) {
        if let Some(fs) = self.serving_filesystem {
            match fs {
                Either::Left(fs) => fs.shutdown().await.expect("Failed to stop filesystem"),
                Either::Right(fs) => fs.shutdown().await.expect("Failed to stop filesystem"),
            }
        }
    }

    fn benchmark_dir(&self) -> &Path {
        Path::new(MOUNT_PATH)
    }
}

#[async_trait]
impl CacheClearableFilesystem for FsManagementFilesystemInstance {
    async fn clear_cache(&mut self) {
        // Remount the filesystem to guarantee that all cached data from reads and write is cleared.
        let serving_filesystem = self.serving_filesystem.take().unwrap();
        let serving_filesystem = match serving_filesystem {
            Either::Left(serving_filesystem) => {
                serving_filesystem.shutdown().await.expect("Failed to stop the filesystem");
                let mut serving_filesystem =
                    self.fs.serve().await.expect("Failed to start the filesystem");
                serving_filesystem.bind_to_path(MOUNT_PATH).expect("Failed to bind the filesystem");
                Either::Left(serving_filesystem)
            }
            Either::Right(serving_filesystem) => {
                serving_filesystem.shutdown().await.expect("Failed to stop the filesystem");
                let mut serving_filesystem =
                    self.fs.serve_multi_volume().await.expect("Failed to start the filesystem");
                let vol = serving_filesystem
                    .open_volume(
                        "default",
                        MountOptions {
                            crypt: self.fs.config().crypt_client().map(|c| c.into()),
                            as_blob: self.as_blob,
                        },
                    )
                    .await
                    .expect("Failed to create volume");
                vol.bind_to_path(MOUNT_PATH).expect("Failed to bind the volume");
                Either::Right(serving_filesystem)
            }
        };
        self.serving_filesystem = Some(serving_filesystem);
    }
}
