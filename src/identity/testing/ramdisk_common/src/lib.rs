// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::expect_fun_call)]

use {
    fidl::{endpoints::Proxy as _, HandleBased as _},
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy},
    fidl_fuchsia_hardware_block_encrypted::DeviceManagerMarker,
    fidl_fuchsia_hardware_block_partition::Guid,
    fidl_fuchsia_hardware_block_volume::VolumeManagerMarker,
    fidl_fuchsia_io as fio,
    fuchsia_component_test::RealmInstance,
    fuchsia_driver_test as _,
    fuchsia_zircon::{
        sys::{zx_handle_t, zx_status_t},
        AsHandleRef, Status,
    },
    ramdevice_client::{RamdiskClient, RamdiskClientBuilder},
    rand::{rngs::SmallRng, Rng, SeedableRng},
    std::fs,
    storage_isolated_driver_manager::bind_fvm,
};

const BLOCK_SIZE: u64 = 4096;
const BLOCK_COUNT: u64 = 1024; // 4MB RAM ought to be good enough

// 1 block for zxcrypt, and minfs needs at least 3 blocks.
const FVM_SLICE_SIZE: usize = BLOCK_SIZE as usize * 4;

#[link(name = "fvm")]
extern "C" {
    pub fn fvm_init(device: zx_handle_t, slice_size: usize) -> zx_status_t;
}

/// Given a realm instance, return a DirectoryProxy which is a client to
///
/// NB: This method calls .expect() and panics rather than returning a result, so
/// it is suitable only for use in tests.
/// root.open(..,"dev",..).
pub fn get_dev_root(realm_instance: &RealmInstance) -> fio::DirectoryProxy {
    fuchsia_fs::directory::open_directory_no_describe(
        realm_instance.root.get_exposed_dir(),
        "dev-topological",
        fio::OpenFlags::empty(),
    )
    .expect("get dev root")
}

/// Given a realm instance, return a File which is /dev, taken as a handle.
///
/// NB: This method calls .expect() and panics rather than returning a result, so
/// it is suitable only for use in tests.
pub fn get_dev_root_fd(realm_instance: &RealmInstance) -> fs::File {
    let dev_root_proxy = get_dev_root(realm_instance);
    fdio::create_fd(
        dev_root_proxy
            .into_channel()
            .expect("Could not convert dev root DirectoryProxy into channel")
            .into_zx_channel()
            .into_handle(),
    )
    .expect("create fd of dev root")
}

/// Given a realm instance, a GUID, and the name of some RAM partition, allocates
/// that volume in RAM and returns the RamDiskClient by value. NB: Callers should
/// retain this client in a test framework, since dropping RamdiskClient destroys
/// the disk.
///
/// NB: This method calls .expect() and panics rather than returning a result, so
/// it is suitable only for use in tests.
pub async fn setup_ramdisk(
    realm_instance: &RealmInstance,
    type_guid: Guid,
    name: &str,
) -> (RamdiskClient, ControllerProxy) {
    // Create ramdisk
    let ramdisk = RamdiskClientBuilder::new(BLOCK_SIZE, BLOCK_COUNT)
        .dev_root(get_dev_root(realm_instance))
        .build()
        .await
        .expect("Could not create ramdisk");

    // Open ramdisk device and initialize FVM
    {
        let ramdisk_handle = ramdisk.open().await.expect("Could not re-open ramdisk");
        let ramdisk_handle_raw = ramdisk_handle.raw_handle();
        let status = unsafe { fvm_init(ramdisk_handle_raw, FVM_SLICE_SIZE) };
        Status::ok(status).expect("could not initialize FVM structures in ramdisk");
    }

    // Open ramdisk device again as fidl_fuchsia_device::ControllerProxy
    let client_end = ramdisk.open_controller().await.expect("Could not re-open ramdisk");
    let controller =
        client_end.into_proxy().expect("Could not convert ramdisk channel to async channel");

    // Bind FVM to that ramdisk
    bind_fvm(&controller).await.expect("Could not bind FVM");

    let ramdisk_dir = ramdisk.as_dir().expect("invalid directory proxy");

    // wait for /fvm child device to appear and open it
    let volume_manager =
        device_watcher::recursive_wait_and_open::<VolumeManagerMarker>(ramdisk_dir, "/fvm")
            .await
            .expect("failed to open fvm child device");

    // create FVM child volume with desired GUID/label
    let mut rng = SmallRng::from_entropy();
    let instance_guid = Guid { value: rng.gen() };
    let status = volume_manager
        .allocate_partition(1, &type_guid, &instance_guid, name, 0)
        .await
        .expect("Could not request to create volume");
    Status::ok(status).expect("Could not create volume");

    let controller = device_watcher::recursive_wait_and_open::<ControllerMarker>(
        ramdisk_dir,
        &format!("/fvm/{name}-p-1/block"),
    )
    .await
    .expect("Could not wait for inner fvm block device");

    // Return handle to ramdisk since RamdiskClient's Drop impl destroys the ramdisk.
    (ramdisk, controller)
}

/// Given a realm instance and a ramdisk, formats that disk under
/// /<ramdisk>/fvm/<name>-p-1/block as a zxcrypt disk.
///
/// NB: This method calls .expect() and panics rather than returning a result, so
/// it is suitable only for use in tests.
pub async fn format_zxcrypt(ramdisk: &RamdiskClient, name: &str, controller: ControllerProxy) {
    // Bind the zxcrypt driver to the block device
    controller
        .bind("zxcrypt.cm")
        .await
        .expect("Could not send request to bind zxcrypt driver")
        .expect("Could not bind zxcrypt driver");

    // Wait for zxcrypt device manager node to appear
    let manager = device_watcher::recursive_wait_and_open::<DeviceManagerMarker>(
        ramdisk.as_dir().expect("invalid directory proxy"),
        &format!("/fvm/{name}-p-1/block/zxcrypt"),
    )
    .await
    .expect("wait for zxcrypt from isolated-devmgr");

    let key: [u8; 32] = [0; 32];
    manager.format(&key, 0).await.expect("Could not format zxcrypt");
}
