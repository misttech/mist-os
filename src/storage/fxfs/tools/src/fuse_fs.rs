// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    fxfs::{
        filesystem::{FxFilesystem, OpenFxFilesystem},
        object_store::{volume::root_volume, ObjectStore},
    },
    fxfs_crypto::Crypt,
    std::{
        fs::{File, OpenOptions},
        path::PathBuf,
        sync::Arc,
    },
    storage_device::{fake_device::FakeDevice, file_backed_device::FileBackedDevice, DeviceHolder},
};

const DEFAULT_DEVICE_BLOCK_SIZE: u32 = 512;
const DEFAULT_DEVICE_SIZE: u64 = 1024u64.pow(4);

/// FUSE assumes that the root directory has an object id of 1,
/// which needs to be casted to be correct root directory id in Fxfs.
const DEFAULT_FUSE_ROOT: u64 = 1;
const DEFAULT_VOLUME_NAME: &str = "fxfs";

/// In-memory fake devices are used for unit tests.
const IN_MEMORY_DEVICE_BLOCK_COUNT: u64 = 8192;

pub struct FuseFs {
    pub fs: OpenFxFilesystem,
    default_store: Arc<ObjectStore>,
}

impl FuseFs {
    /// Initialize the filesystem with a fake in-memory device.
    pub async fn new_in_memory() -> Self {
        let device = DeviceHolder::new(FakeDevice::new(
            IN_MEMORY_DEVICE_BLOCK_COUNT,
            DEFAULT_DEVICE_BLOCK_SIZE,
        ));
        let crypt = None;
        FuseFs::new(device, crypt).await
    }

    /// Initialize the filesystem by creating a new file-backed device.
    pub async fn new_file_backed(path: &str) -> Self {
        let file = FuseFs::create_file(path);
        let device = DeviceHolder::new(FileBackedDevice::new(file, DEFAULT_DEVICE_BLOCK_SIZE));
        let crypt = None;
        FuseFs::new(device, crypt).await
    }

    /// Initialize the filesystem by opening an existing file-backed device.
    pub async fn open_file_backed(path: &str) -> Self {
        let file = FuseFs::create_file(path);
        let device = DeviceHolder::new(FileBackedDevice::new(file, DEFAULT_DEVICE_BLOCK_SIZE));
        let crypt = None;
        FuseFs::open(device, crypt).await
    }

    /// Create a file on disk that is used for file-backed device.
    pub fn create_file(path: &str) -> File {
        let path_buf = PathBuf::from(path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path_buf.as_path())
            .unwrap_or_else(|e| panic!("failed to create {:?}: {:?}", path_buf.as_path(), e));
        file.set_len(DEFAULT_DEVICE_SIZE).expect("failed to truncate file");
        file
    }

    /// Create a new FxFilesystem using given device and cryption, with a root volume initialized.
    pub async fn new(device: DeviceHolder, crypt: Option<Arc<dyn Crypt>>) -> Self {
        let fs = FxFilesystem::new_empty(device).await.expect("fxfs new_empty failed");
        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        root_volume
            .new_volume(DEFAULT_VOLUME_NAME, crypt.clone())
            .await
            .expect("new_volume failed");
        let default_store = root_volume
            .volume(DEFAULT_VOLUME_NAME, crypt.clone())
            .await
            .expect("default_store failed");

        Self { fs, default_store }
    }

    /// Open an existing FxFilesystem using given device and cryption
    pub async fn open(device: DeviceHolder, crypt: Option<Arc<dyn Crypt>>) -> Self {
        let fs = FxFilesystem::open(device).await.expect("new_empty failed");
        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        let default_store = root_volume
            .volume(DEFAULT_VOLUME_NAME, crypt.clone())
            .await
            .expect("default_store failed");

        Self { fs, default_store }
    }

    /// FUSE assumes that inode 1 is the root of a filesystem, which needs
    /// to be changed to the correct root directory object id of Fxfs.
    pub fn fuse_inode_to_object_id(&self, inode: u64) -> u64 {
        match inode {
            DEFAULT_FUSE_ROOT => self.default_store.root_directory_object_id(),
            _ => inode,
        }
    }
}
