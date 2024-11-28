// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fuchsia::testing::{TestFixture, TestFixtureOptions};
use crate::fuchsia::volume::FxVolume;
use async_trait::async_trait;
use blob_writer::BlobWriter;
use delivery_blob::{CompressionMode, Type1Blob};
use fidl_fuchsia_fxfs::{BlobCreatorMarker, BlobReaderMarker, BlobWriterProxy, CreateBlobError};
use fuchsia_component::client::connect_to_protocol_at_dir_svc;
use fuchsia_merkle::Hash;

use fxfs::object_store::directory::Directory;
use fxfs::object_store::{DataObjectHandle, HandleOptions, ObjectStore};
use storage_device::fake_device::FakeDevice;
use storage_device::DeviceHolder;

pub async fn new_blob_fixture() -> TestFixture {
    TestFixture::open(
        DeviceHolder::new(FakeDevice::new(16384, 512)),
        TestFixtureOptions { encrypted: false, as_blob: true, format: true, serve_volume: true },
    )
    .await
}

pub async fn open_blob_fixture(device_holder: DeviceHolder) -> TestFixture {
    TestFixture::open(
        device_holder,
        TestFixtureOptions { encrypted: false, as_blob: true, format: false, serve_volume: true },
    )
    .await
}

#[async_trait]
pub trait BlobFixture {
    async fn write_blob(&self, data: &[u8], mode: CompressionMode) -> Hash;
    async fn read_blob(&self, hash: Hash) -> Vec<u8>;
    async fn get_blob_handle(&self, name: &str) -> DataObjectHandle<FxVolume>;
    async fn get_blob_vmo(&self, hash: Hash) -> zx::Vmo;
    async fn create_blob(
        &self,
        hash: &[u8; 32],
        allow_existing: bool,
    ) -> Result<BlobWriterProxy, CreateBlobError>;
}

#[async_trait]
impl BlobFixture for TestFixture {
    async fn write_blob(&self, data: &[u8], mode: CompressionMode) -> Hash {
        let hash = fuchsia_merkle::from_slice(data).root();
        let delivery_data: Vec<u8> = Type1Blob::generate(&data, mode);
        let writer = self.create_blob(&hash.into(), false).await.expect("failed to create blob");
        let mut blob_writer = BlobWriter::create(writer, delivery_data.len() as u64)
            .await
            .expect("failed to create BlobWriter");
        blob_writer.write(&delivery_data).await.unwrap();
        hash
    }

    async fn read_blob(&self, hash: Hash) -> Vec<u8> {
        let vmo = self.get_blob_vmo(hash).await;
        vmo.read_to_vec(0, vmo.get_stream_size().unwrap()).expect("vmo read failed")
    }

    async fn get_blob_handle(&self, name: &str) -> DataObjectHandle<FxVolume> {
        let root_object_id = self.volume().volume().store().root_directory_object_id();
        let root_dir =
            Directory::open(self.volume().volume(), root_object_id).await.expect("open failed");
        let (object_id, _) =
            root_dir.lookup(name).await.expect("lookup failed").expect("file doesn't exist yet");

        ObjectStore::open_object(self.volume().volume(), object_id, HandleOptions::default(), None)
            .await
            .expect("open_object failed")
    }

    async fn get_blob_vmo(&self, hash: Hash) -> zx::Vmo {
        let blob_reader = connect_to_protocol_at_dir_svc::<BlobReaderMarker>(self.volume_out_dir())
            .expect("failed to connect to the BlobReader service");
        blob_reader
            .get_vmo(&hash.into())
            .await
            .expect("transport error on BlobReader.GetVmo")
            .expect("get_vmo failed")
    }

    async fn create_blob(
        &self,
        hash: &[u8; 32],
        allow_existing: bool,
    ) -> Result<BlobWriterProxy, CreateBlobError> {
        let blob_proxy = connect_to_protocol_at_dir_svc::<BlobCreatorMarker>(self.volume_out_dir())
            .expect("failed to connect to the BlobCreator service");
        let blob_writer =
            blob_proxy.create(hash, allow_existing).await.expect("transport error on create")?;
        Ok(blob_writer.into_proxy())
    }
}
