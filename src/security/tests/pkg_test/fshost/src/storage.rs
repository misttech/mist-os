// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy},
    fidl_fuchsia_io as fio,
    fs_management::{
        filesystem::{Filesystem, ServingSingleVolumeFilesystem},
        Blobfs, ComponentType,
    },
    fuchsia_zircon::{AsHandleRef, Rights, Vmo},
    ramdevice_client::{RamdiskClient, RamdiskClientBuilder},
    std::convert::TryInto as _,
    storage_isolated_driver_manager::bind_fvm,
};

// The block size to use for the ramdisk backing FVM+blobfs.
const RAMDISK_BLOCK_SIZE: u64 = 512;

// The subdirectory from the FVM topological path to the blobfs block device
// topological path. Full path may be, for example,
// `/dev/sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm/{FVM_BLOBFS_BLOCK_SUBDIR}`.
//
// TODO(fxbug.dev/88614): FVM+blobfs client code such as this would be less
// brittle if the there was a library for waiting on this block device.
const FVM_BLOBFS_BLOCK_SUBDIR: &str = "blobfs-p-1/block";

/// Wrapper around `fs_management::Filesystem` that retains objects for
/// instantiating blobfs loaded from an in-memory copy of an FVM image.
pub struct BlobfsInstance {
    _fvm_vmo: Vmo,
    _fvm: FvmInstance,
    blobfs: Filesystem,
    serving_blobfs: Option<ServingSingleVolumeFilesystem>,
}

impl BlobfsInstance {
    /// Instantiate blobfs using fvm block file store at `fvm_resource_path`.
    pub async fn new_from_resource(fvm_resource_path: &str) -> Self {
        // Create a VMO filled with the FVM image stored at `fvm_resource_path`.
        let fvm_buf = fuchsia_fs::file::read_in_namespace(fvm_resource_path).await.unwrap();
        let fvm_size = fvm_buf.len();
        let fvm_vmo = Vmo::create(fvm_size.try_into().unwrap()).unwrap();
        fvm_vmo.write(&fvm_buf, 0).unwrap();

        // Create a ramdisk; do not init FVM (init=false) as we are loading an
        // existing image.
        let fvm = FvmInstance::new(&fvm_vmo, RAMDISK_BLOCK_SIZE).await;

        // Wait for device at blobfs block path before interacting with it.
        let blobfs_controller = device_watcher::recursive_wait_and_open::<ControllerMarker>(
            &fvm.fvm_dir,
            FVM_BLOBFS_BLOCK_SUBDIR,
        )
        .await
        .expect("blobfs block did not appear");

        // Instantiate blobfs.
        let config = Blobfs {
            component_type: ComponentType::StaticChild,
            allow_delivery_blobs: true,
            ..Default::default()
        };
        let mut blobfs = Filesystem::new(blobfs_controller, config);

        // Check blobfs consistency.
        blobfs.fsck().await.unwrap();

        Self { _fvm_vmo: fvm_vmo, _fvm: fvm, blobfs, serving_blobfs: None }
    }

    /// Mount blobfs to directory location `mountpoint`.
    pub async fn mount(&mut self, mountpoint: &str) {
        if let Some(blobfs_dir) =
            self.serving_blobfs.as_ref().and_then(ServingSingleVolumeFilesystem::bound_path)
        {
            panic!(
                "Attempt to re-mount blobfs at {} when it is already mounted at {}",
                mountpoint, blobfs_dir
            );
        }

        let mut blobfs = self.blobfs.serve().await.unwrap();
        blobfs.bind_to_path(mountpoint).unwrap();
        self.serving_blobfs = Some(blobfs);
    }

    /// Umount blobfs.
    pub fn unmount(&mut self) {
        assert!(
            self.serving_blobfs.take().is_some(),
            "Attempt to unmount blobfs when it is not mounted"
        );
    }

    /// Open the blobfs root directory.
    pub fn open_root_dir(&self) -> fio::DirectoryProxy {
        fuchsia_fs::directory::open_in_namespace(
            self.serving_blobfs
                .as_ref()
                .and_then(ServingSingleVolumeFilesystem::bound_path)
                .expect("Attempt to open blobfs root when it is not mounted"),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .unwrap()
    }
}

//
// Forked from
// https://fuchsia.googlesource.com/fuchsia/+/30e9e1cc3b019ef1d7aabc620b470f8a6518db55/src/storage/stress-tests/utils/fvm.rs
//

async fn create_ramdisk(vmo: &Vmo, ramdisk_block_size: u64) -> RamdiskClient {
    let duplicated_handle = vmo.as_handle_ref().duplicate(Rights::SAME_RIGHTS).unwrap();
    let duplicated_vmo = Vmo::from(duplicated_handle);

    // Create the ramdisks
    RamdiskClientBuilder::new_with_vmo(duplicated_vmo, Some(ramdisk_block_size))
        .build()
        .await
        .unwrap()
}

async fn start_fvm_driver(
    ramdisk_controller: &ControllerProxy,
    ramdisk_dir: &fio::DirectoryProxy,
) -> fio::DirectoryProxy {
    let () = bind_fvm(ramdisk_controller).await.unwrap();
    // Wait until the FVM driver is available
    device_watcher::recursive_wait_and_open_directory(ramdisk_dir, "/fvm")
        .await
        .expect("Could not wait for fvm path")
}

/// This structs holds processes of component manager, isolated-devmgr
/// and the fvm driver.
///
/// NOTE: The order of fields in this struct is important.
/// Destruction happens top-down. Test must be destroyed last.
pub struct FvmInstance {
    /// Manages the ramdisk device that is backed by a VMO
    _ramdisk: RamdiskClient,

    /// Directory proxy associated with the started fvm instance
    fvm_dir: fio::DirectoryProxy,
}

impl FvmInstance {
    /// Start an isolated FVM driver against the given VMO.
    /// If `init` is true, initialize the VMO with FVM layout first.
    pub async fn new(vmo: &Vmo, ramdisk_block_size: u64) -> Self {
        let ramdisk = create_ramdisk(&vmo, ramdisk_block_size).await;
        let fvm_dir = start_fvm_driver(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
        )
        .await;
        Self { _ramdisk: ramdisk, fvm_dir }
    }
}
