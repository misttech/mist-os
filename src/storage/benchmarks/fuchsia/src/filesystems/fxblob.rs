// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::{BlobFilesystem, DeliveryBlob, FsManagementFilesystemInstance},
    async_trait::async_trait,
    blob_writer::BlobWriter,
    fidl_fuchsia_fxfs::{BlobCreatorMarker, BlobCreatorProxy},
    fuchsia_component::client::connect_to_protocol_at_dir_svc,
    std::path::Path,
    storage_benchmarks::{
        BlockDeviceConfig, BlockDeviceFactory, CacheClearableFilesystem, Filesystem,
        FilesystemConfig,
    },
};

/// Config object for starting Fxblob instances.
#[derive(Clone)]
pub struct Fxblob;

#[async_trait]
impl FilesystemConfig for Fxblob {
    type Filesystem = FxblobInstance;

    async fn start_filesystem(
        &self,
        block_device_factory: &dyn BlockDeviceFactory,
    ) -> FxblobInstance {
        let block_device = block_device_factory
            .create_block_device(&BlockDeviceConfig {
                use_zxcrypt: false,
                fvm_volume_size: Some(125 * 1024 * 1024),
            })
            .await;
        let fxblob = FsManagementFilesystemInstance::new(
            fs_management::Fxfs::default(),
            block_device,
            /*as_blob=*/ true,
        )
        .await;
        let blob_creator =
            connect_to_protocol_at_dir_svc::<BlobCreatorMarker>(fxblob.exposed_dir())
                .expect("failed to connect to the BlobCreator service");
        FxblobInstance { blob_creator, fxblob }
    }

    fn name(&self) -> String {
        "fxblob".to_owned()
    }
}

pub struct FxblobInstance {
    blob_creator: BlobCreatorProxy,
    fxblob: FsManagementFilesystemInstance,
}

#[async_trait]
impl Filesystem for FxblobInstance {
    async fn shutdown(self) {
        self.fxblob.shutdown().await
    }

    fn benchmark_dir(&self) -> &Path {
        self.fxblob.benchmark_dir()
    }
}

#[async_trait]
impl CacheClearableFilesystem for FxblobInstance {
    async fn clear_cache(&mut self) {
        self.fxblob.clear_cache().await
    }
}

#[async_trait]
impl BlobFilesystem for FxblobInstance {
    async fn write_blob(&self, blob: &DeliveryBlob) {
        let writer_client_end = self
            .blob_creator
            .create(&blob.name.into(), false)
            .await
            .expect("transport error on BlobCreator.Create")
            .expect("failed to create blob");
        let writer = writer_client_end.into_proxy().unwrap();
        let mut blob_writer = BlobWriter::create(writer, blob.data.len() as u64)
            .await
            .expect("failed to create BlobWriter");
        blob_writer.write(&blob.data).await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use {super::Fxblob, crate::filesystems::testing::check_blob_filesystem};

    #[fuchsia::test]
    async fn start_fxblob_new() {
        check_blob_filesystem(Fxblob).await;
    }
}
