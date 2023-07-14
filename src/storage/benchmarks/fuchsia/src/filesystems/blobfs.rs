// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::{BlobFilesystem, DeliveryBlob, FsManagementFilesystemInstance},
    async_trait::async_trait,
    delivery_blob::delivery_blob_path,
    fidl_fuchsia_io as fio,
    std::path::Path,
    storage_benchmarks::{
        BlockDeviceConfig, BlockDeviceFactory, CacheClearableFilesystem, Filesystem,
        FilesystemConfig,
    },
};

/// Config object for starting Blobfs instances.
#[derive(Clone)]
pub struct Blobfs;

#[async_trait]
impl FilesystemConfig for Blobfs {
    type Filesystem = BlobfsInstance;

    async fn start_filesystem(
        &self,
        block_device_factory: &dyn BlockDeviceFactory,
    ) -> BlobfsInstance {
        let block_device = block_device_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;
        let blobfs = FsManagementFilesystemInstance::new(
            fs_management::Blobfs { allow_delivery_blobs: true, ..Default::default() },
            block_device,
            /*as_blob=*/ false,
        )
        .await;
        let root = fuchsia_fs::directory::open_in_namespace(
            blobfs.benchmark_dir().to_str().unwrap(),
            fuchsia_fs::OpenFlags::RIGHT_WRITABLE | fuchsia_fs::OpenFlags::RIGHT_READABLE,
        )
        .unwrap();
        BlobfsInstance { root, blobfs }
    }

    fn name(&self) -> String {
        "blobfs".to_owned()
    }
}

pub struct BlobfsInstance {
    root: fio::DirectoryProxy,
    blobfs: FsManagementFilesystemInstance,
}

#[async_trait]
impl Filesystem for BlobfsInstance {
    async fn shutdown(self) {
        self.blobfs.shutdown().await
    }

    fn benchmark_dir(&self) -> &Path {
        self.blobfs.benchmark_dir()
    }
}

#[async_trait]
impl CacheClearableFilesystem for BlobfsInstance {
    async fn clear_cache(&mut self) {
        self.blobfs.clear_cache().await
    }
}

#[async_trait]
impl BlobFilesystem for BlobfsInstance {
    async fn write_blob(&self, blob: &DeliveryBlob) {
        let blob_proxy = fuchsia_fs::directory::open_file(
            &self.root,
            &delivery_blob_path(blob.name),
            fuchsia_fs::OpenFlags::CREATE | fuchsia_fs::OpenFlags::RIGHT_WRITABLE,
        )
        .await
        .unwrap();
        let blob_size = blob.data.len();
        blob_proxy.resize(blob_size as u64).await.unwrap().unwrap();
        let mut written = 0;
        while written != blob_size {
            // Don't try to write more than MAX_TRANSFER_SIZE bytes at a time.
            let bytes_to_write =
                std::cmp::min(fio::MAX_TRANSFER_SIZE, (blob_size - written) as u64);
            let bytes_written: u64 = blob_proxy
                .write(&blob.data[written..written + bytes_to_write as usize])
                .await
                .unwrap()
                .unwrap();
            assert_eq!(bytes_written, bytes_to_write);
            written += bytes_written as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::Blobfs, crate::filesystems::testing::check_blob_filesystem};

    #[fuchsia::test]
    async fn start_blobfs() {
        check_blob_filesystem(Blobfs).await;
    }
}
