// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::expect_fun_call)]

use {
    fidl::{endpoints::Proxy as _, HandleBased as _},
    fidl_fuchsia_device::ControllerMarker,
    fidl_fuchsia_hardware_block_encrypted::{DeviceManagerMarker, DeviceManagerProxy},
    fidl_fuchsia_hardware_block_partition::Guid,
    fidl_fuchsia_hardware_block_volume::VolumeManagerProxy,
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
        "/dev",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .expect("Get /dev from isolated_devmgr")
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
    mut type_guid: Guid,
    name: &str,
) -> RamdiskClient {
    let dev_root_proxy = get_dev_root(realm_instance);

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
    //
    // TODO(https://fxbug.dev/112484): this relies on multiplexing.
    let client_end = ramdisk.open().await.expect("Could not re-open ramdisk");
    let client_end = fidl::endpoints::ClientEnd::<ControllerMarker>::new(client_end.into_channel());
    let controller =
        client_end.into_proxy().expect("Could not convert ramdisk channel to async channel");

    // Bind FVM to that ramdisk
    bind_fvm(&controller).await.expect("Could not bind FVM");

    // wait for /fvm child device to appear and open it
    let fvm_path = ramdisk.get_path().to_string() + "/fvm";
    let volume_manager_client =
        device_watcher::recursive_wait_and_open_node(&dev_root_proxy, &fvm_path)
            .await
            .expect("Could not wait for fvm from isolated-devmgr")
            .into_channel()
            .map(VolumeManagerProxy::from_channel)
            .expect("Could not get fvm channel");

    // create FVM child volume with desired GUID/label
    let mut rng = SmallRng::from_entropy();
    let mut instance_guid = Guid { value: rng.gen() };
    let status = volume_manager_client
        .allocate_partition(1, &mut type_guid, &mut instance_guid, name, 0)
        .await
        .expect("Could not request to create volume");
    Status::ok(status).expect("Could not create volume");

    let fvm_inner_block_path = fvm_path + "/" + name + "-p-1/block";
    let _: fio::NodeProxy =
        device_watcher::recursive_wait_and_open_node(&dev_root_proxy, &fvm_inner_block_path)
            .await
            .expect("Could not wait for inner fvm block device");

    // Return handle to ramdisk since RamdiskClient's Drop impl destroys the ramdisk.
    ramdisk
}

/// Given a realm instance, opens /<ramdisk>/fvm/<name>-p-1/block/zxcrypt and
/// returns a proxy to it.
///
/// NB: This method calls .expect() and panics rather than returning a result, so
/// it is suitable only for use in tests.
pub fn open_zxcrypt_manager(
    realm_instance: &RealmInstance,
    ramdisk: &RamdiskClient,
    name: &str,
) -> DeviceManagerProxy {
    let mgr_path = format!("{}/fvm/{}-p-1/block/zxcrypt", ramdisk.get_path(), name);
    fuchsia_component::client::connect_to_named_protocol_at_dir_root::<DeviceManagerMarker>(
        &get_dev_root(realm_instance),
        &mgr_path,
    )
    .expect("Could not connect to zxcrypt manager")
}

/// Given a realm instance and a ramdisk, formats that disk under
/// /<ramdisk>/fvm/<name>-p-1/block as a zxcrypt disk.
///
/// NB: This method calls .expect() and panics rather than returning a result, so
/// it is suitable only for use in tests.
pub async fn format_zxcrypt(realm_instance: &RealmInstance, ramdisk: &RamdiskClient, name: &str) {
    let block_path = format!("{}/fvm/{}-p-1/block", ramdisk.get_path(), name);
    let controller_client = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
        ControllerMarker,
    >(&get_dev_root(realm_instance), &block_path)
    .expect("Could not connect to fvm block device");

    // Bind the zxcrypt driver to the block device
    controller_client
        .bind("zxcrypt.so")
        .await
        .expect("Could not send request to bind zxcrypt driver")
        .expect("Could not bind zxcrypt driver");

    // Wait for zxcrypt device manager node to appear
    let zxcrypt_path = block_path + "/zxcrypt";
    let dev_root_proxy = get_dev_root(realm_instance);
    let _: fio::NodeProxy =
        device_watcher::recursive_wait_and_open_node(&dev_root_proxy, &zxcrypt_path)
            .await
            .expect("wait for zxcrypt from isolated-devmgr");

    // Open zxcrypt device manager node
    let manager = open_zxcrypt_manager(realm_instance, ramdisk, name);
    let key: [u8; 32] = [0; 32];
    manager.format(&key, 0).await.expect("Could not format zxcrypt");
}
