// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        fuse_attr::{create_dir_attr, create_file_attr, create_symlink_attr},
        fuse_errors::FuseErrorParser,
    },
    event_listener::Event,
    fuse3::{raw::prelude::*, Result},
    fxfs::{
        filesystem::{FxFilesystem, OpenFxFilesystem},
        log::info,
        object_handle::{GetProperties, ObjectProperties},
        object_store::{
            volume::root_volume, Directory, HandleOptions, ObjectDescriptor, ObjectKey, ObjectKind,
            ObjectStore, ObjectValue, StoreObjectHandle, Timestamp,
        },
    },
    fxfs_crypto::Crypt,
    libc,
    once_cell::sync::OnceCell,
    std::{
        ffi::OsStr,
        fs::{File, OpenOptions},
        path::PathBuf,
        process,
        sync::Arc,
    },
    storage_device::{fake_device::FakeDevice, file_backed_device::FileBackedDevice, DeviceHolder},
    tokio,
};

const DEFAULT_DEVICE_BLOCK_SIZE: u32 = 512;
const DEFAULT_DEVICE_SIZE: u64 = 1024u64.pow(4);

/// FUSE assumes that the root directory has an object id of 1,
/// which needs to be casted to be correct root directory id in Fxfs.
const DEFAULT_FUSE_ROOT: u64 = 1;
const DEFAULT_VOLUME_NAME: &str = "fxfs";

/// In-memory fake devices are used for unit tests.
const IN_MEMORY_DEVICE_BLOCK_COUNT: u64 = 8192;

/// CLOSE_EVENT listens to the signals from user to gracefully close the filesystem.
static CLOSE_EVENT: OnceCell<Event> = OnceCell::new();

fn register_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, handle_sigterm as usize);
    }
}

/// Handler for the SIGTERM signal.
extern "C" fn handle_sigterm(_signal: i32) {
    register_signal_handlers();
    info!("SIGTERM Received");
    CLOSE_EVENT.get().unwrap().notify(usize::MAX);
}

pub struct FuseFs {
    pub fs: OpenFxFilesystem,
    pub default_store: Arc<ObjectStore>,
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

    /// Listen to the signals to gracefully close the filesystem.
    pub async fn notify_destroy(&self) -> tokio::task::JoinHandle<()> {
        register_signal_handlers();
        CLOSE_EVENT.set(Event::new()).unwrap();
        let fs_clone = self.fs.clone();

        let handle = tokio::spawn(async move {
            let listener = CLOSE_EVENT.get().unwrap().listen();
            listener.wait();
            fs_clone.close().await.expect("Close failed");
            info!("Filesystem is gracefully closed");
            process::exit(0);
        });
        handle
    }

    /// FUSE assumes that inode 1 is the root of a filesystem, which needs
    /// to be changed to the correct root directory object id of Fxfs.
    pub fn fuse_inode_to_object_id(&self, inode: u64) -> u64 {
        match inode {
            DEFAULT_FUSE_ROOT => self.default_store.root_directory_object_id(),
            _ => inode,
        }
    }

    /// Get the object store of Fxfs's root directory.
    pub async fn root_dir(&self) -> Result<Directory<ObjectStore>> {
        Directory::open(&self.default_store, self.default_store.root_directory_object_id())
            .await
            .parse_error()
    }

    /// Open the directory with specified object id.
    pub async fn open_dir(&self, object_id: u64) -> Result<Directory<ObjectStore>> {
        Directory::open(&self.default_store, object_id).await.parse_error()
    }

    /// Get the handle of an object with specified id.
    pub async fn get_object_handle(
        &self,
        object_id: u64,
    ) -> Result<Arc<StoreObjectHandle<ObjectStore>>> {
        let handle = Arc::new(
            ObjectStore::open_object(
                &self.default_store,
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .parse_error()?,
        );

        Ok(handle)
    }

    /// Get the properties of an object with specified id.
    pub async fn get_object_properties(
        &self,
        object_id: u64,
        object_type: ObjectDescriptor,
    ) -> Result<ObjectProperties> {
        if object_type == ObjectDescriptor::File {
            let handle = self.get_object_handle(object_id).await?;
            handle.get_properties().await.parse_error()
        } else if object_type == ObjectDescriptor::Directory {
            let dir = self.open_dir(object_id).await?;
            dir.get_properties().await.parse_error()
        } else {
            panic!("Invalid input object type");
        }
    }

    /// Get the type of an object with specified id.
    pub async fn get_object_type(&self, object_id: u64) -> Result<Option<ObjectDescriptor>> {
        let object_result =
            self.default_store.tree().find(&ObjectKey::object(object_id)).await.parse_error()?;
        if let Some(object) = object_result {
            match object.value {
                ObjectValue::Object { kind: ObjectKind::Directory { .. }, .. } => {
                    Ok(Some(ObjectDescriptor::Directory))
                }
                ObjectValue::Object { kind: ObjectKind::File { .. }, .. } => {
                    Ok(Some(ObjectDescriptor::File))
                }
                _ => Ok(None),
            }
        } else {
            if let Some(symlink) = self
                .default_store
                .tree()
                .find(&ObjectKey::symlink(object_id))
                .await
                .parse_error()?
            {
                if symlink.value != ObjectValue::None {
                    return Ok(Some(ObjectDescriptor::Symlink));
                }
            }
            Ok(None)
        }
    }

    /// Create the FUSE-style attribute for an object based on its type.
    pub async fn create_object_attr(
        &self,
        object_id: u64,
        object_type: ObjectDescriptor,
    ) -> Result<FileAttr> {
        match object_type {
            ObjectDescriptor::Directory => {
                let properties = self.get_object_properties(object_id, object_type.clone()).await?;
                Ok(create_dir_attr(
                    object_id,
                    properties.data_attribute_size,
                    properties.creation_time,
                    properties.modification_time,
                ))
            }
            ObjectDescriptor::File => {
                let properties = self.get_object_properties(object_id, object_type.clone()).await?;
                Ok(create_file_attr(
                    object_id,
                    properties.data_attribute_size,
                    properties.creation_time,
                    properties.modification_time,
                    properties.refs as u32,
                ))
            }
            ObjectDescriptor::Volume => Err(libc::ENOSYS.into()),
            ObjectDescriptor::Symlink => {
                Ok(create_symlink_attr(object_id, Timestamp::now(), Timestamp::now()))
            }
        }
    }
}

pub trait FuseStrParser {
    fn parse_str(&self) -> Result<&str>;
}

impl FuseStrParser for OsStr {
    /// Convert from OsStr to str.
    fn parse_str(&self) -> Result<&str> {
        if let Some(s) = self.to_str() {
            Ok(s)
        } else {
            Err(libc::EINVAL.into())
        }
    }
}
