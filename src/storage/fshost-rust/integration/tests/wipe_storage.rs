// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test cases which simulate fshost running in the configuration used in recovery builds (which,
//! among other things, sets the fvm_ramdisk flag to prevent binding of the on-disk filesystems.)

use {
    crate::{data_fs_name, data_fs_spec, data_fs_zxcrypt, new_builder},
    device_watcher::recursive_wait,
    fidl::endpoints::{create_proxy, Proxy as _},
    fidl_fuchsia_fshost as fshost,
    fidl_fuchsia_hardware_block::BlockProxy,
    fidl_fuchsia_hardware_block_partition::PartitionMarker,
    fidl_fuchsia_io as fio,
    fs_management::{
        partition::{find_partition_in, PartitionMatcher},
        Blobfs,
    },
    fshost_test_fixture::TestFixture,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_zircon as zx,
    remote_block_device::{BlockClient, MutableBufferSlice, RemoteBlockClient},
};

// Blob containing 8192 bytes of 0xFF ("oneblock").
const TEST_BLOB_LEN: u64 = 8192;
const TEST_BLOB_DATA: [u8; TEST_BLOB_LEN as usize] = [0xFF; TEST_BLOB_LEN as usize];
const TEST_BLOB_NAME: &'static str =
    "68d131bc271f9c192d4f6dcd8fe61bef90004856da19d0f2f514a7f4098b0737";

async fn write_test_blob(directory: &fio::DirectoryProxy) {
    let test_blob = fuchsia_fs::directory::open_file(
        directory,
        TEST_BLOB_NAME,
        fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    )
    .await
    .unwrap();
    test_blob.resize(TEST_BLOB_LEN).await.unwrap().expect("Resize failed");
    let bytes_written = test_blob.write(&TEST_BLOB_DATA).await.unwrap().expect("Write failed");
    assert_eq!(bytes_written, TEST_BLOB_LEN);
}

// TODO(fxbug.dev/112142): Due to a race between the block watcher and some fshost functionality
// (e.g. WipeStorage), we have to wait for the block watcher to finish binding all expected drivers.
//
// Regardless of the `fvm_ramdisk` / `ramdisk_prefix` / `gpt_all` config options, fshost will match
// the first block device with a GPT or FVM partition and bind those drivers. zxcrypt volumes are
// also unsealed, unless `no_zxcrypt` is true.
async fn wait_for_block_watcher(fixture: &TestFixture, has_formatted_fvm: bool) {
    let ramdisk = fixture.ramdisks.first().unwrap();
    let gpt_path = "/part-000/block";
    let ramdisk_dir = ramdisk.as_dir().expect("invalid directory proxy");
    if has_formatted_fvm {
        // TODO(https://fxbug.dev/121274): Remove hardcoded paths
        let blobfs_path = format!("{}/fvm/blobfs-p-1/block", gpt_path);
        recursive_wait(ramdisk_dir, &blobfs_path).await.unwrap();
        let data_path = format!("{}/fvm/data-p-2/block", gpt_path);
        if data_fs_name() != "fxfs" && data_fs_zxcrypt() {
            let zxcrypt_path = format!("{}/zxcrypt/unsealed/block", data_path);
            recursive_wait(ramdisk_dir, &zxcrypt_path).await.unwrap();
        } else {
            recursive_wait(ramdisk_dir, &data_path).await.unwrap();
        }
    } else {
        recursive_wait(ramdisk_dir, &gpt_path).await.unwrap();
    }
}

// Ensure fuchsia.fshost.Admin/WipeStorage fails if we cannot identify a storage device to wipe.
#[fuchsia::test]
async fn no_fvm_device() {
    // TODO(fxbug.dev/113970): this test doesn't work on f2fs
    if data_fs_name() == "f2fs" {
        return;
    }

    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("fvm_ramdisk", true)
        .set_config_value("ramdisk_prefix", "/nada/zip/zilch");

    // Only the rust fshost can deal with multiple disks.
    if cfg!(feature = "fshost_rust") {
        builder.with_zbi_ramdisk();
    }

    let fixture = builder.build().await;
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (_, blobfs_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let result = admin
        .wipe_storage(blobfs_server)
        .await
        .expect("FIDL call to WipeStorage failed")
        .expect_err("WipeStorage unexpectedly succeeded");
    assert_eq!(zx::Status::from_raw(result), zx::Status::INTERNAL);
    fixture.tear_down().await;
}

// Demonstrate high level usage of the fuchsia.fshost.Admin/WipeStorage method.
#[fuchsia::test]
async fn write_blob() {
    // TODO(fxbug.dev/113970): this test doesn't work on f2fs
    if data_fs_name() == "f2fs" {
        return;
    }

    let mut builder = new_builder();
    // Ensure the ramdisk prefix will **not** match the ramdisks we create in the fixture, thus
    // treating them as "real" storage devices.
    builder
        .fshost()
        .set_config_value("fvm_ramdisk", true)
        .set_config_value("ramdisk_prefix", "/nada/zip/zilch");
    // We need to use a GPT as WipeStorage relies on the reported partition type GUID, rather than
    // content sniffing of the FVM magic.
    builder.with_disk().with_gpt();

    // Only the rust fshost can deal with multiple disks.
    if cfg!(feature = "fshost_rust") {
        builder.with_zbi_ramdisk();
    }

    let fixture = builder.build().await;
    wait_for_block_watcher(&fixture, /*has_formatted_fvm*/ true).await;

    // Invoke WipeStorage, which will unbind the FVM, reprovision it, and format/mount Blobfs.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    admin.wipe_storage(blobfs_server).await.unwrap().expect("WipeStorage unexpectedly failed");

    // Ensure that we can write a blob into the new Blobfs instance.
    write_test_blob(&blobfs_root).await;

    fixture.tear_down().await;
}

// Verify that all existing blobs are purged after running fuchsia.fshost.Admin/WipeStorage.
#[fuchsia::test]
async fn blobfs_formatted() {
    // TODO(fxbug.dev/113970): this test doesn't work on f2fs
    if data_fs_name() == "f2fs" {
        return;
    }

    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("fvm_ramdisk", true)
        .set_config_value("ramdisk_prefix", "/nada/zip/zilch");
    builder.with_disk().with_gpt();

    // Only the rust fshost can deal with multiple disks.
    if cfg!(feature = "fshost_rust") {
        builder.with_zbi_ramdisk();
    }

    let fixture = builder.build().await;
    wait_for_block_watcher(&fixture, /*has_formatted_fvm*/ true).await;

    // Mount Blobfs and write a blob.
    {
        let test_disk = fixture.ramdisks.first().unwrap();
        let test_disk_path = test_disk
            .as_controller()
            .expect("ramdisk didn't have controller proxy")
            .get_topological_path()
            .await
            .expect("get topo path fidl failed")
            .expect("get topo path returned error");
        let matcher = PartitionMatcher {
            parent_device: Some(test_disk_path),
            labels: Some(vec!["blobfs".to_string()]),
            ..Default::default()
        };
        let blobfs_path = find_partition_in(
            &fixture.dir("dev-topological/class/block"),
            matcher,
            zx::Duration::INFINITE,
        )
        .await
        .unwrap();
        let controller =
            connect_to_named_protocol_at_dir_root::<fidl_fuchsia_device::ControllerMarker>(
                &fixture.dir("dev-topological"),
                &blobfs_path.strip_prefix("/dev").unwrap(),
            )
            .unwrap();
        let blobfs = Blobfs::new(controller).serve().await.unwrap();

        let blobfs_root = blobfs.root();
        write_test_blob(blobfs_root).await;
        assert!(fuchsia_fs::directory::dir_contains(blobfs_root, TEST_BLOB_NAME).await.unwrap());
    }

    // Invoke the WipeStorage API.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    admin
        .wipe_storage(blobfs_server)
        .await
        .unwrap()
        .map_err(zx::Status::from_raw)
        .expect("WipeStorage unexpectedly failed");

    // Verify there are no more blobs.
    assert!(fuchsia_fs::directory::readdir(&blobfs_root).await.unwrap().is_empty());

    fixture.tear_down().await;
}

// Verify that the data partition is wiped and remains unformatted.
#[fuchsia::test]
async fn data_unformatted() {
    // TODO(fxbug.dev/113970): this test doesn't work on f2fs
    if data_fs_name() == "f2fs" {
        return;
    }

    const BUFF_LEN: usize = 512;
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("fvm_ramdisk", true)
        .set_config_value("ramdisk_prefix", "/nada/zip/zilch");
    builder.with_disk().format_data(data_fs_spec()).with_gpt();

    // Only the rust fshost can deal with multiple disks.
    if cfg!(feature = "fshost_rust") {
        builder.with_zbi_ramdisk();
    }

    let fixture = builder.build().await;
    wait_for_block_watcher(&fixture, /*has_formatted_fvm*/ true).await;
    let test_disk = fixture.ramdisks.first().unwrap();
    let test_disk_path = test_disk
        .as_controller()
        .expect("ramdisk didn't have controller proxy")
        .get_topological_path()
        .await
        .expect("get topo path fidl failed")
        .expect("get topo path returned error");
    let dev = fixture.dir("dev-topological");
    let dev_class = fixture.dir("dev-topological/class/block");
    let matcher = PartitionMatcher {
        parent_device: Some(test_disk_path),
        labels: Some(vec!["data".to_string()]),
        ..Default::default()
    };

    let orig_instance_guid;
    {
        let data_path =
            find_partition_in(&dev_class, matcher.clone(), zx::Duration::INFINITE).await.unwrap();
        let data_partition = connect_to_named_protocol_at_dir_root::<PartitionMarker>(
            &dev,
            &data_path.strip_prefix("/dev").unwrap(),
        )
        .unwrap();

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
    let (_, blobfs_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    admin.wipe_storage(blobfs_server).await.unwrap().expect("WipeStorage unexpectedly failed");

    // Ensure the data partition was assigned a new instance GUID.
    let data_path = find_partition_in(&dev_class, matcher, zx::Duration::INFINITE).await.unwrap();
    let data_partition = connect_to_named_protocol_at_dir_root::<PartitionMarker>(
        &dev,
        &data_path.strip_prefix("/dev").unwrap(),
    )
    .unwrap();
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
#[fuchsia::test]
async fn handles_corrupt_fvm() {
    // TODO(fxbug.dev/113970): this test doesn't work on f2fs
    if data_fs_name() == "f2fs" {
        return;
    }

    let mut builder = new_builder();
    // Ensure the ramdisk prefix will **not** match the ramdisks we create in the fixture, thus
    // treating them as "real" storage devices.
    builder
        .fshost()
        .set_config_value("fvm_ramdisk", true)
        .set_config_value("ramdisk_prefix", "/nada/zip/zilch");
    // Ensure that, while we allocate an FVM partition inside the GPT, we leave it empty.
    builder.with_disk().with_gpt().with_unformatted_fvm();

    // Only the rust fshost can deal with multiple disks.
    if cfg!(feature = "fshost_rust") {
        builder.with_zbi_ramdisk();
    }

    let fixture = builder.build().await;
    wait_for_block_watcher(&fixture, /*has_formatted_fvm*/ false).await;

    // Invoke WipeStorage, which will unbind the FVM, reprovision it, and format/mount Blobfs.
    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    let (blobfs_root, blobfs_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    admin.wipe_storage(blobfs_server).await.unwrap().expect("WipeStorage unexpectedly failed");

    // Ensure that we can write a blob into the new Blobfs instance.
    write_test_blob(&blobfs_root).await;

    fixture.tear_down().await;
}
