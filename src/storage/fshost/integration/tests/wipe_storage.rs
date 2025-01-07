// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test cases which simulate fshost running in the configuration used in recovery builds (which,
//! among other things, sets the ramdisk_image flag to prevent binding of the on-disk filesystems.)

use block_client::{BlockClient, MutableBufferSlice, RemoteBlockClient};
use device_watcher::recursive_wait;
use fidl::endpoints::{create_proxy, Proxy as _, ServiceMarker as _};
use fidl_fuchsia_hardware_block::BlockProxy;
use fidl_fuchsia_hardware_block_partition::PartitionMarker;
use fs_management::partition::{find_partition_in, PartitionMatcher};
use fshost_test_fixture::disk_builder::VolumesSpec;
use fshost_test_fixture::{write_test_blob, write_test_blob_fxblob};
use {fidl_fuchsia_fshost as fshost, fidl_fuchsia_io as fio};

pub mod config;
use config::{blob_fs_type, data_fs_spec, data_fs_type, new_builder, volumes_spec};

const TEST_BLOB_DATA: [u8; 8192] = [0xFF; 8192];
// TODO(https://fxbug.dev/42072287): Remove hardcoded paths
const GPT_PATH: &'static str = "/part-000/block";
const BLOBFS_FVM_PATH: &'static str = "/part-000/block/fvm/blobfs-p-1/block";
const DATA_FVM_PATH: &'static str = "/part-000/block/fvm/data-p-2/block";

// Ensure fuchsia.fshost.Admin/WipeStorage fails if we cannot identify a storage device to wipe.
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
#[fuchsia::test]
#[cfg_attr(feature = "f2fs", ignore)]
async fn no_fvm_device() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (_, blobfs_server) = create_proxy::<fio::DirectoryMarker>();
    let result = admin
        .wipe_storage(Some(blobfs_server), None)
        .await
        .expect("FIDL call to WipeStorage failed")
        .expect_err("WipeStorage unexpectedly succeeded");
    assert_eq!(zx::Status::from_raw(result), zx::Status::INTERNAL);
    fixture.tear_down().await;
}

// Demonstrate high level usage of the fuchsia.fshost.Admin/WipeStorage method.
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
#[fuchsia::test]
#[cfg_attr(feature = "f2fs", ignore)]
async fn write_blob() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    // We need to use a GPT as WipeStorage relies on the reported partition type GUID, rather than
    // content sniffing of the FVM magic.
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec()).with_gpt();
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    // Wait for the zbi ramdisk filesystems
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    // Also wait for any driver binding on the "on-disk" devices
    if cfg!(feature = "storage-host") {
        recursive_wait(
            &fixture.dir(
                fidl_fuchsia_storage_partitions::PartitionServiceMarker::SERVICE_NAME,
                fio::PERM_READABLE,
            ),
            "part-000",
        )
        .await
        .unwrap();
    } else {
        let ramdisk_dir =
            fixture.ramdisks.first().expect("no ramdisks?").as_dir().expect("invalid dir proxy");
        recursive_wait(ramdisk_dir, GPT_PATH).await.unwrap();
        if !cfg!(feature = "fxblob") {
            recursive_wait(ramdisk_dir, BLOBFS_FVM_PATH).await.unwrap();
            recursive_wait(ramdisk_dir, DATA_FVM_PATH).await.unwrap();
        }
    }

    let (blob_creator_proxy, blob_creator) = if cfg!(feature = "fxblob") {
        let (proxy, server_end) = fidl::endpoints::create_proxy();
        (Some(proxy), Some(server_end))
    } else {
        (None, None)
    };

    // Invoke WipeStorage, which will unbind the FVM, reprovision it, and format/mount Blobfs.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>();
    admin
        .wipe_storage(Some(blobfs_server), blob_creator)
        .await
        .unwrap()
        .expect("WipeStorage unexpectedly failed");

    // Ensure that we can write a blob into the new Blobfs instance.
    if cfg!(feature = "fxblob") {
        write_test_blob_fxblob(blob_creator_proxy.unwrap(), &TEST_BLOB_DATA).await;
    } else {
        write_test_blob(&blobfs_root, &TEST_BLOB_DATA, false).await;
    }

    fixture.tear_down().await;
}

// Demonstrate high level usage of the fuchsia.fshost.Admin/WipeStorage method when a data
// data partition does not already exist.
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
#[fuchsia::test]
#[cfg_attr(feature = "f2fs", ignore)]
async fn write_blob_no_existing_data_partition() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    // We need to use a GPT as WipeStorage relies on the reported partition type GUID, rather than
    // content sniffing of the FVM magic.
    builder
        .with_disk()
        .format_volumes(VolumesSpec { create_data_partition: false, ..volumes_spec() })
        .with_gpt();
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    // Wait for the zbi ramdisk filesystems
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    // Also wait for any driver binding on the "on-disk" devices
    if cfg!(feature = "storage-host") {
        recursive_wait(
            &fixture.dir(
                fidl_fuchsia_storage_partitions::PartitionServiceMarker::SERVICE_NAME,
                fio::PERM_READABLE,
            ),
            "part-000",
        )
        .await
        .unwrap();
    } else {
        let ramdisk_dir =
            fixture.ramdisks.first().expect("no ramdisks?").as_dir().expect("invalid dir proxy");
        recursive_wait(ramdisk_dir, GPT_PATH).await.unwrap();
        if !cfg!(feature = "fxblob") {
            recursive_wait(ramdisk_dir, BLOBFS_FVM_PATH).await.unwrap();
        }
    }

    let (blob_creator_proxy, blob_creator) = if cfg!(feature = "fxblob") {
        let (proxy, server_end) = fidl::endpoints::create_proxy();
        (Some(proxy), Some(server_end))
    } else {
        (None, None)
    };

    // Invoke WipeStorage, which will unbind the FVM, reprovision it, and format/mount Blobfs.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>();
    admin
        .wipe_storage(Some(blobfs_server), blob_creator)
        .await
        .unwrap()
        .expect("WipeStorage unexpectedly failed");

    // Ensure that we can write a blob into the new Blobfs instance.
    if cfg!(feature = "fxblob") {
        write_test_blob_fxblob(blob_creator_proxy.unwrap(), &TEST_BLOB_DATA).await;
    } else {
        write_test_blob(&blobfs_root, &TEST_BLOB_DATA, false).await;
    }

    fixture.tear_down().await;
}

// Verify that all existing blobs are purged after running fuchsia.fshost.Admin/WipeStorage.
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
#[fuchsia::test]
#[cfg_attr(feature = "f2fs", ignore)]
async fn blobfs_formatted() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec()).with_gpt();

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    // The test fixture writes tests blobs to blobfs or fxblob when it is formatted.
    fixture.check_test_blob(cfg!(feature = "fxblob")).await;

    let vmo = fixture.into_vmo().await.unwrap();

    let mut builder = new_builder().with_disk_from_vmo(vmo);
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    // Wait for the zbi ramdisk filesystems
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    // Also wait for any driver binding on the "on-disk" devices
    if cfg!(feature = "storage-host") {
        recursive_wait(
            &fixture.dir(
                fidl_fuchsia_storage_partitions::PartitionServiceMarker::SERVICE_NAME,
                fio::PERM_READABLE,
            ),
            "part-000",
        )
        .await
        .unwrap();
    } else {
        let ramdisk_dir =
            fixture.ramdisks.first().expect("no ramdisks?").as_dir().expect("invalid dir proxy");
        recursive_wait(ramdisk_dir, GPT_PATH).await.unwrap();
        if !cfg!(feature = "fxblob") {
            recursive_wait(ramdisk_dir, BLOBFS_FVM_PATH).await.unwrap();
            recursive_wait(ramdisk_dir, DATA_FVM_PATH).await.unwrap();
        }
    }

    let blob_creator = if cfg!(feature = "fxblob") {
        let (_, server_end) = fidl::endpoints::create_proxy();
        Some(server_end)
    } else {
        None
    };

    // Invoke the WipeStorage API.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>();
    admin
        .wipe_storage(Some(blobfs_server), blob_creator)
        .await
        .unwrap()
        .map_err(zx::Status::from_raw)
        .expect("WipeStorage unexpectedly failed");

    // Verify there are no blobs.
    assert!(fuchsia_fs::directory::readdir(&blobfs_root).await.unwrap().is_empty());

    fixture.tear_down().await;
}

// Verify that the data partition is wiped and remains unformatted.
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
// This test is very specific to fvm, so we don't run it against fxblob. Since both volumes are in
// fxfs anyway with fxblob, this test is somewhat redundant with the basic tests.
#[fuchsia::test]
#[cfg_attr(any(feature = "f2fs", feature = "fxblob"), ignore)]
async fn data_unformatted() {
    const BUFF_LEN: usize = 512;
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec()).with_gpt();
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    // Wait for the zbi ramdisk filesystems
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    // Also wait for any driver binding on the "on-disk" devices
    if cfg!(feature = "storage-host") {
        recursive_wait(
            &fixture.dir(
                fidl_fuchsia_storage_partitions::PartitionServiceMarker::SERVICE_NAME,
                fio::PERM_READABLE,
            ),
            "part-000",
        )
        .await
        .unwrap();
    } else {
        let ramdisk_dir =
            fixture.ramdisks.first().expect("no ramdisks?").as_dir().expect("invalid dir proxy");
        recursive_wait(ramdisk_dir, GPT_PATH).await.unwrap();
        if !cfg!(feature = "fxblob") {
            recursive_wait(ramdisk_dir, BLOBFS_FVM_PATH).await.unwrap();
            recursive_wait(ramdisk_dir, DATA_FVM_PATH).await.unwrap();
        }
    }

    let test_disk = fixture.ramdisks.first().unwrap();
    let test_disk_path = test_disk
        .as_controller()
        .expect("ramdisk didn't have controller proxy")
        .get_topological_path()
        .await
        .expect("get topo path fidl failed")
        .expect("get topo path returned error");
    let dev_class = fixture.dir("dev-topological/class/block", fio::Flags::empty());
    let matcher = PartitionMatcher {
        parent_device: Some(test_disk_path),
        labels: Some(vec!["data".to_string()]),
        ..Default::default()
    };

    let orig_instance_guid;
    {
        let data_controller =
            find_partition_in(&dev_class, matcher.clone(), zx::MonotonicDuration::INFINITE)
                .await
                .unwrap();
        let (data_partition, partition_server_end) =
            fidl::endpoints::create_proxy::<PartitionMarker>();
        data_controller.connect_to_device_fidl(partition_server_end.into_channel()).unwrap();

        let (status, guid) = data_partition.get_instance_guid().await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        orig_instance_guid = guid.unwrap();

        let block_client = RemoteBlockClient::new(BlockProxy::from_channel(
            data_partition.into_channel().unwrap(),
        ))
        .await
        .unwrap();
        let mut buff: [u8; BUFF_LEN] = [0; BUFF_LEN];
        block_client.read_at(MutableBufferSlice::Memory(&mut buff), 0).await.unwrap();
        // The data partition should have been formatted so there should be some non-zero bytes.
        assert_ne!(buff, [0; BUFF_LEN]);
    }

    // Invoke WipeStorage.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (_, blobfs_server) = create_proxy::<fio::DirectoryMarker>();
    admin
        .wipe_storage(Some(blobfs_server), None)
        .await
        .unwrap()
        .expect("WipeStorage unexpectedly failed");

    // Ensure the data partition was assigned a new instance GUID.
    let data_controller =
        find_partition_in(&dev_class, matcher, zx::MonotonicDuration::INFINITE).await.unwrap();
    let (data_partition, partition_server_end) = fidl::endpoints::create_proxy::<PartitionMarker>();
    data_controller.connect_to_device_fidl(partition_server_end.into_channel()).unwrap();
    let (status, guid) = data_partition.get_instance_guid().await.unwrap();
    assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
    assert_ne!(guid.unwrap(), orig_instance_guid);

    // The data partition should remain unformatted, so the first few bytes should be all zero now.
    let block_client =
        RemoteBlockClient::new(BlockProxy::from_channel(data_partition.into_channel().unwrap()))
            .await
            .unwrap();
    let mut buff: [u8; BUFF_LEN] = [0; BUFF_LEN];
    block_client.read_at(MutableBufferSlice::Memory(&mut buff), 0).await.unwrap();
    assert_eq!(buff, [0; BUFF_LEN]);

    fixture.tear_down().await;
}

// Verify that WipeStorage can handle a completely corrupted FVM.
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
#[fuchsia::test]
#[cfg_attr(feature = "f2fs", ignore)]
async fn handles_corrupt_fvm() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    // Ensure that, while we allocate an FVM or Fxfs partition inside the GPT, we leave it empty.
    builder.with_disk().format_volumes(volumes_spec()).with_gpt().with_unformatted_volume_manager();
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    // Wait for the zbi ramdisk filesystems
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    // Also wait for any driver binding on the "on-disk" devices
    if cfg!(feature = "storage-host") {
        recursive_wait(
            &fixture.dir(
                fidl_fuchsia_storage_partitions::PartitionServiceMarker::SERVICE_NAME,
                fio::PERM_READABLE,
            ),
            "part-000",
        )
        .await
        .unwrap();
    } else {
        let ramdisk_dir =
            fixture.ramdisks.first().expect("no ramdisks?").as_dir().expect("invalid dir proxy");
        recursive_wait(ramdisk_dir, GPT_PATH).await.unwrap();
    }

    let (blob_creator_proxy, blob_creator) = if cfg!(feature = "fxblob") {
        let (proxy, server_end) = fidl::endpoints::create_proxy();
        (Some(proxy), Some(server_end))
    } else {
        (None, None)
    };

    // Invoke WipeStorage, which will unbind the FVM, reprovision it, and format/mount Blobfs.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>();
    admin
        .wipe_storage(Some(blobfs_server), blob_creator)
        .await
        .unwrap()
        .expect("WipeStorage unexpectedly failed");

    // Ensure that we can write a blob into the new Blobfs instance.
    if cfg!(feature = "fxblob") {
        write_test_blob_fxblob(blob_creator_proxy.unwrap(), &TEST_BLOB_DATA).await;
    } else {
        write_test_blob(&blobfs_root, &TEST_BLOB_DATA, false).await;
    }

    fixture.tear_down().await;
}
