// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for some of the filesystem corruption handling paths. These naturally have a lot of error
//! messages since corruption is Bad, so they are in their own test package that allows error logs.

use device_watcher::recursive_wait;
use fidl::endpoints::{create_proxy, ServiceMarker as _};
use fshost_test_fixture::{write_test_blob, write_test_blob_fxblob};
use {fidl_fuchsia_fshost as fshost, fidl_fuchsia_io as fio};

pub mod config;
use config::{
    blob_fs_type, data_fs_name, data_fs_spec, data_fs_type, new_builder, volumes_spec,
    DATA_FILESYSTEM_VARIANT,
};

const TEST_BLOB_DATA: [u8; 8192] = [0xFF; 8192];
// TODO(https://fxbug.dev/42072287): Remove hardcoded paths
const GPT_PATH: &'static str = "/part-000/block";

#[fuchsia::test]
async fn data_reformatted_when_corrupt() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec()).corrupt_data();
    let mut fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file_absent().await;

    // Ensure blobs are not reformatted.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture
        .wait_for_crash_reports(
            1,
            data_fs_name(),
            &format!("fuchsia-{}-corruption", data_fs_name()),
        )
        .await;

    fixture.tear_down().await;
}

// Verify that WipeStorage can handle a completely corrupted FVM.
#[fuchsia::test]
// TODO(https://fxbug.dev/42065222): this test doesn't work on f2fs.
#[cfg_attr(feature = "f2fs", ignore)]
// TODO(https://fxbug.dev/388533231): this test doesn't work on storage-host+minfs
#[cfg_attr(all(feature = "storage-host", feature = "minfs"), ignore)]
async fn wipe_storage_handles_corrupt_fvm() {
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
        .map_err(zx::Status::from_raw)
        .expect("WipeStorage unexpectedly failed");

    // Ensure that we can write a blob into the new Blobfs instance.
    if cfg!(feature = "fxblob") {
        write_test_blob_fxblob(blob_creator_proxy.unwrap(), &TEST_BLOB_DATA).await;
    } else {
        write_test_blob(&blobfs_root, &TEST_BLOB_DATA, false).await;
    }

    fixture.tear_down().await;
}
