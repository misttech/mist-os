// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        testing::{open_file_checked, TestFixture, TestFixtureOptions},
        volume::FxVolume,
    },
    async_trait::async_trait,
    blob_writer::BlobWriter,
    delivery_blob::{CompressionMode, Type1Blob},
    fidl_fuchsia_io::{self as fio, MAX_TRANSFER_SIZE},
    fuchsia_component::client::connect_to_protocol_at_dir_svc,
    fuchsia_merkle::{Hash, MerkleTreeBuilder},
    fxfs::object_store::{directory::Directory, DataObjectHandle, HandleOptions, ObjectStore},
    storage_device::{fake_device::FakeDevice, DeviceHolder},
};

pub async fn new_blob_fixture() -> TestFixture {
    TestFixture::open(
        DeviceHolder::new(FakeDevice::new(16384, 512)),
        TestFixtureOptions { encrypted: false, as_blob: true, format: true },
    )
    .await
}

// TODO(fxbug.dev/125334): Support using the new write API in this fixture.
#[async_trait]
pub trait BlobFixture {
    async fn write_blob(&self, data: &[u8]) -> Hash;
    async fn read_blob(&self, name: &str) -> Vec<u8>;
    async fn get_blob_handle(&self, name: &str) -> DataObjectHandle<FxVolume>;
}

#[async_trait]
impl BlobFixture for TestFixture {
    async fn write_blob(&self, data: &[u8]) -> Hash {
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let delivery_data: Vec<u8> = Type1Blob::generate(&data, CompressionMode::Never);

        let (blob_volume_outgoing_dir, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                .expect("Create dir proxy to succeed");

        self.volumes_directory()
            .serve_volume(self.volume(), server_end, true)
            .await
            .expect("failed to serve the blob volume");
        let blob_proxy = connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
            &blob_volume_outgoing_dir,
        )
        .expect("failed to connect to the Blob service");

        let blob_writer_client_end = blob_proxy
            .create(&hash.into(), false)
            .await
            .expect("transport error on create")
            .expect("failed to create blob");

        let writer = blob_writer_client_end.into_proxy().unwrap();
        let mut blob_writer = BlobWriter::create(writer, delivery_data.len() as u64)
            .await
            .expect("failed to create BlobWriter");
        blob_writer.write(&delivery_data).await.unwrap();
        hash
    }

    async fn read_blob(&self, name: &str) -> Vec<u8> {
        let blob = open_file_checked(self.root(), fio::OpenFlags::RIGHT_READABLE, name).await;
        let mut data = Vec::new();
        loop {
            let chunk =
                blob.read(MAX_TRANSFER_SIZE).await.expect("FIDL call failed").expect("read failed");
            let done = chunk.len() < MAX_TRANSFER_SIZE as usize;
            data.extend(chunk);
            if done {
                break;
            }
        }
        data
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
}
