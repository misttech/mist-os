// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        testing::{open_file_checked, TestFixture, TestFixtureOptions},
        volume::FxVolume,
    },
    async_trait::async_trait,
    fidl_fuchsia_io::{self as fio, MAX_TRANSFER_SIZE},
    fuchsia_merkle::{Hash, MerkleTreeBuilder},
    fxfs::object_store::{directory::Directory, HandleOptions, ObjectStore, StoreObjectHandle},
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
    async fn get_blob_handle(&self, name: &str) -> StoreObjectHandle<FxVolume>;
}

#[async_trait]
impl BlobFixture for TestFixture {
    async fn write_blob(&self, data: &[u8]) -> Hash {
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();

        let blob = open_file_checked(
            self.root(),
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE,
            &format!("{}", hash),
        )
        .await;
        blob.resize(data.len() as u64).await.expect("FIDL call failed").expect("truncate failed");
        for chunk in data.chunks(MAX_TRANSFER_SIZE as usize) {
            assert_eq!(
                blob.write(&chunk).await.expect("FIDL call failed").expect("write failed"),
                chunk.len() as u64
            );
        }

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

    async fn get_blob_handle(&self, name: &str) -> StoreObjectHandle<FxVolume> {
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
