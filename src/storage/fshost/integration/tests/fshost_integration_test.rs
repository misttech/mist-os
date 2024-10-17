// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob};
use fidl::endpoints::create_proxy;
use fidl_fuchsia_fs_startup::VolumeMarker as FsStartupVolumeMarker;
use fidl_fuchsia_fshost::AdminMarker;
use fidl_fuchsia_hardware_block_volume::{VolumeManagerMarker, VolumeMarker};
use fs_management::format::constants::DATA_PARTITION_LABEL;
use fs_management::partition::{find_partition_in, PartitionMatcher};
use fs_management::DATA_TYPE_GUID;
use fshost_test_fixture::disk_builder::{
    DataSpec, VolumesSpec, DEFAULT_DATA_VOLUME_SIZE, FVM_SLICE_SIZE,
};
use fshost_test_fixture::{
    BLOBFS_MAX_BYTES, DATA_MAX_BYTES, VFS_TYPE_FXFS, VFS_TYPE_MEMFS, VFS_TYPE_MINFS,
};
use fuchsia_component::client::connect_to_named_protocol_at_dir_root;
use futures::FutureExt;
use regex::Regex;
use zx::{self as zx, HandleBased};
use {fidl_fuchsia_fshost as fshost, fidl_fuchsia_io as fio, fuchsia_async as fasync};

#[cfg(feature = "fxfs")]
use fidl_fuchsia_fxfs::CryptManagementMarker;

#[cfg(feature = "fxblob")]
use {
    blob_writer::BlobWriter,
    fidl_fuchsia_fxfs::{BlobCreatorMarker, BlobReaderMarker},
    fidl_fuchsia_update_verify::{BlobfsVerifierMarker, VerifyOptions},
};

pub mod config;

use config::{
    blob_fs_type, data_fs_name, data_fs_spec, data_fs_type, data_fs_zxcrypt, new_builder,
    volumes_spec, DATA_FILESYSTEM_FORMAT, DATA_FILESYSTEM_VARIANT,
};

#[fuchsia::test]
async fn blobfs_and_data_mounted() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;
    fixture.tear_down().await;
}

#[fuchsia::test]
async fn blobfs_and_data_mounted_legacy_label() {
    let mut builder = new_builder();
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .format_data(data_fs_spec())
        .with_legacy_data_label();
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn data_formatted() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn data_partition_nonexistent() {
    let mut builder = new_builder();
    builder
        .with_disk()
        .format_volumes(VolumesSpec { create_data_partition: false, ..volumes_spec() });
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn data_formatted_legacy_label() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).with_legacy_data_label();
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

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

#[fuchsia::test]
async fn data_formatted_no_fuchsia_boot() {
    let mut builder = new_builder().no_fuchsia_boot();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn data_formatted_with_small_initial_volume() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).data_volume_size(1);

    #[allow(unused_mut)]
    let mut fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    // With the storage-host configuration, the FVM driver is responsible for mounting the data
    // volume, and we use the error code returned to decide whether to generate a crash report.
    // Specifically, we look for the `WRONG_TYPE` error code and if we get that, we assume it's most
    // likely due to FDR or flashing.  In this test, the error is something other than `WRONG_TYPE`
    // so it generates a crash report, which is arguably the right thing to do, but you won't get a
    // crash report for the non storage-host configuration or for fxfs.
    #[cfg(all(feature = "storage-host", not(feature = "fxfs")))]
    fixture.ignore_crash_reports();

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn data_formatted_with_small_initial_volume_big_target() {
    let mut builder = new_builder();
    // The formatting uses the max bytes argument as the initial target to resize to. If this
    // target is larger than the disk, the resize should still succeed.
    builder.fshost().set_config_value(
        "data_max_bytes",
        fshost_test_fixture::disk_builder::DEFAULT_DISK_SIZE * 2,
    );
    builder.with_disk().format_volumes(volumes_spec()).data_volume_size(1);

    #[allow(unused_mut)]
    let mut fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    // With the storage-host configuration, the FVM driver is responsible for mounting the data
    // volume, and we use the error code returned to decide whether to generate a crash report.
    // Specifically, we look for the `WRONG_TYPE` error code and if we get that, we assume it's most
    // likely due to FDR or flashing.  In this test, the error is something other than `WRONG_TYPE`
    // so it generates a crash report, which is arguably the right thing to do, but you won't get a
    // crash report for the non storage-host configuration or for fxfs.
    #[cfg(all(feature = "storage-host", not(feature = "fxfs")))]
    fixture.ignore_crash_reports();

    fixture.tear_down().await;
}

// Ensure WipeStorage is not supported in the normal mode of operation (i.e. when the
// `ramdisk_image` option is false). WipeStorage should only function within a recovery context.
#[fuchsia::test]
async fn wipe_storage_not_supported() {
    let builder = new_builder();
    let fixture = builder.build().await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();

    let (_, blobfs_server) = create_proxy::<fio::DirectoryMarker>().unwrap();

    let result = admin
        .wipe_storage(Some(blobfs_server), None)
        .await
        .unwrap()
        .expect_err("WipeStorage unexpectedly succeeded");
    assert_eq!(zx::Status::from_raw(result), zx::Status::NOT_SUPPORTED);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn ramdisk_blob_and_data_mounted() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder
        .with_zbi_ramdisk()
        .format_volumes(volumes_spec())
        .format_data(DataSpec { zxcrypt: false, ..data_fs_spec() });
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn ramdisk_blob_and_data_mounted_no_existing_data_partition() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder
        .with_zbi_ramdisk()
        .format_volumes(VolumesSpec { create_data_partition: false, ..volumes_spec() })
        .format_data(DataSpec { zxcrypt: false, ..data_fs_spec() });
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture.tear_down().await;
}

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): Make this work with storage-host.
#[cfg_attr(feature = "storage-host", ignore)]
async fn ramdisk_data_ignores_non_ramdisk() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .format_data(DataSpec { zxcrypt: false, ..data_fs_spec() });
    let fixture = builder.build().await;

    if DATA_FILESYSTEM_VARIANT != "fxblob" {
        let dev = fixture.dir("dev-topological/class/block", fio::Flags::empty());

        // The filesystems won't be mounted, but make sure fvm and potentially zxcrypt are bound.
        device_watcher::wait_for_device_with(&dev, |info| {
            info.topological_path.ends_with("fvm/data-p-2/block").then_some(())
        })
        .await
        .unwrap();
    }

    // There isn't really a good way to tell that something is not mounted, but at this point we
    // would be pretty close to it, so a timeout of a couple seconds should safeguard against
    // potential issues.
    futures::select! {
        _ = fixture.check_fs_type("data", data_fs_type()).fuse() => {
            panic!("check_fs_type returned unexpectedly - data was mounted");
        },
        _ = fixture.check_fs_type("blob", blob_fs_type()).fuse() => {
            panic!("check_fs_type returned unexpectedly - blob was mounted");
        },
        _ = fasync::Timer::new(std::time::Duration::from_secs(2)).fuse() => (),
    }

    fixture.tear_down().await;
}

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): Make this work with storage-host.
#[cfg_attr(any(feature = "fxblob", feature = "storage-host"), ignore)]
async fn partition_max_size_set() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", DATA_MAX_BYTES)
        .set_config_value("blobfs_max_bytes", BLOBFS_MAX_BYTES);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    // Get the blobfs instance guid.
    // TODO(https://fxbug.dev/42072287): Remove hardcoded paths
    let volume_proxy_data = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
        &fixture.dir("dev-topological", fio::Flags::empty()),
        "sys/platform/ram-disk/ramctl/ramdisk-0/block/fvm/blobfs-p-1/block",
    )
    .unwrap();
    let (status, data_instance_guid) = volume_proxy_data.get_instance_guid().await.unwrap();
    zx::Status::ok(status).unwrap();
    let mut blobfs_instance_guid = data_instance_guid.unwrap();

    let data_matcher = PartitionMatcher {
        type_guids: Some(vec![DATA_TYPE_GUID]),
        labels: Some(vec![DATA_PARTITION_LABEL.to_string()]),
        ignore_if_path_contains: Some("zxcrypt/unsealed".to_string()),
        ..Default::default()
    };

    let dev = fixture.dir("dev-topological/class/block", fio::Flags::empty());
    let data_partition_controller =
        find_partition_in(&dev, data_matcher, zx::MonotonicDuration::from_seconds(10))
            .await
            .expect("failed to find data partition");

    // Get the data instance guid.
    let (volume_proxy, volume_server_end) =
        fidl::endpoints::create_proxy::<VolumeMarker>().unwrap();
    data_partition_controller.connect_to_device_fidl(volume_server_end.into_channel()).unwrap();

    let (status, data_instance_guid) = volume_proxy.get_instance_guid().await.unwrap();
    zx::Status::ok(status).unwrap();
    let mut data_instance_guid = data_instance_guid.unwrap();

    // TODO(https://fxbug.dev/42072287): Remove hardcoded paths
    let fvm_proxy = connect_to_named_protocol_at_dir_root::<VolumeManagerMarker>(
        &fixture.dir("dev-topological", fio::Flags::empty()),
        "sys/platform/ram-disk/ramctl/ramdisk-0/block/fvm",
    )
    .unwrap();

    // blobfs max size check
    let (status, blobfs_slice_count) =
        fvm_proxy.get_partition_limit(blobfs_instance_guid.as_mut()).await.unwrap();
    zx::Status::ok(status).unwrap();
    assert_eq!(blobfs_slice_count, (BLOBFS_MAX_BYTES + FVM_SLICE_SIZE - 1) / FVM_SLICE_SIZE);

    // data max size check
    let (status, data_slice_count) =
        fvm_proxy.get_partition_limit(data_instance_guid.as_mut()).await.unwrap();
    zx::Status::ok(status).unwrap();
    // The expected size depends on whether we are using zxcrypt or not.
    // When wrapping in zxcrypt the data partition size is the same, but the physical disk
    // commitment is one slice bigger.
    let mut expected_slices = (DATA_MAX_BYTES + FVM_SLICE_SIZE - 1) / FVM_SLICE_SIZE;
    if data_fs_zxcrypt() && data_fs_type() != VFS_TYPE_FXFS {
        tracing::info!("Adding an extra expected data slice for zxcrypt");
        expected_slices += 1;
    }
    assert_eq!(data_slice_count, expected_slices);

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(not(feature = "fxblob"), ignore)]
async fn set_volume_bytes_limit() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", DATA_MAX_BYTES)
        .set_config_value("blobfs_max_bytes", BLOBFS_MAX_BYTES);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let volumes_dir = fixture.dir("volumes", fio::Flags::empty());

    let blob_volume_proxy =
        connect_to_named_protocol_at_dir_root::<FsStartupVolumeMarker>(&volumes_dir, "blob")
            .unwrap();
    let blob_volume_bytes_limit = blob_volume_proxy.get_limit().await.unwrap().unwrap();

    let data_volume_proxy =
        connect_to_named_protocol_at_dir_root::<FsStartupVolumeMarker>(&volumes_dir, "data")
            .unwrap();
    let data_volume_bytes_limit = data_volume_proxy.get_limit().await.unwrap().unwrap();
    assert_eq!(blob_volume_bytes_limit, BLOBFS_MAX_BYTES);
    assert_eq!(data_volume_bytes_limit, DATA_MAX_BYTES);
    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(feature = "fxblob", ignore)]
async fn set_data_and_blob_max_bytes_zero() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("data_max_bytes", 0).set_config_value("blobfs_max_bytes", 0);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    let flags = fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE;

    let data_root = fixture.dir("data", flags);
    let file = fuchsia_fs::directory::open_file(&data_root, "file", flags).await.unwrap();
    fuchsia_fs::file::write(&file, "file contents!").await.unwrap();

    let blob_contents = vec![0; 8192];
    let hash = fuchsia_merkle::from_slice(&blob_contents).root();

    let blob_root = fixture.dir("blob", flags);
    let blob =
        fuchsia_fs::directory::open_file(&blob_root, &format!("{}", hash), flags).await.unwrap();
    blob.resize(blob_contents.len() as u64)
        .await
        .expect("FIDL call failed")
        .expect("truncate failed");
    fuchsia_fs::file::write(&blob, &blob_contents).await.unwrap();

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg(feature = "fxblob")]
async fn set_data_and_blob_max_bytes_zero_new_write_api() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("data_max_bytes", 0).set_config_value("blobfs_max_bytes", 0);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    let flags = fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE;

    let data_root = fixture.dir("data", flags);
    let file = fuchsia_fs::directory::open_file(&data_root, "file", flags).await.unwrap();
    fuchsia_fs::file::write(&file, "file contents!").await.unwrap();

    let blob_contents = vec![0; 8192];
    let hash = fuchsia_merkle::from_slice(&blob_contents).root();
    let compressed_data: Vec<u8> = Type1Blob::generate(&blob_contents, CompressionMode::Always);

    let blob_proxy = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<BlobCreatorMarker>()
        .expect("connect_to_protocol_at_exposed_dir failed");

    let writer_client_end = blob_proxy
        .create(&hash.into(), false)
        .await
        .expect("transport error on BlobCreator.Create")
        .expect("failed to create blob");
    let writer = writer_client_end.into_proxy().unwrap();
    let mut blob_writer = BlobWriter::create(writer, compressed_data.len() as u64)
        .await
        .expect("failed to create BlobWriter");
    blob_writer.write(&compressed_data).await.unwrap();

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn tmp_is_available() {
    let builder = new_builder();
    let fixture = builder.build().await;

    fixture.check_fs_type("tmp", VFS_TYPE_MEMFS).await;

    fixture.tear_down().await;
}

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): Make this work with storage-host.
#[cfg_attr(feature = "storage-host", ignore)]
async fn netboot_set() {
    // Set the netboot flag
    let mut builder = new_builder().netboot();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    if DATA_FILESYSTEM_VARIANT != "fxblob" {
        let dev = fixture.dir("dev-topological/class/block", fio::Flags::empty());

        // Filesystems will not be mounted but make sure that fvm is bound
        device_watcher::wait_for_device_with(&dev, |info| {
            info.topological_path.ends_with("fvm/data-p-2/block").then_some(())
        })
        .await
        .unwrap();
    }

    // Use the same approach as ramdisk_data_ignores_non_ramdisk() to ensure that
    // neither blobfs nor data were mounted using a timeout
    futures::select! {
        _ = fixture.check_fs_type("data", data_fs_type()).fuse() => {
            panic!("check_fs_type returned unexpectedly - data was mounted");
        },
        _ = fixture.check_fs_type("blob", blob_fs_type()).fuse() => {
            panic!("check_fs_type returned unexpectedly - blob was mounted");
        },
        _ = fasync::Timer::new(std::time::Duration::from_secs(2)).fuse() => {
        },
    }

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn ramdisk_image_serves_zbi_ramdisk_contents_with_unformatted_data() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Test that fshost handles the case where the FVM is within a GPT partition.
#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): port to storage-host
#[cfg_attr(any(feature = "fxblob", feature = "storage-host"), ignore)]
async fn fvm_within_gpt() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).with_gpt().format_data(data_fs_spec());
    let fixture = builder.build().await;
    let dev = fixture.dir("dev-topological/class/block", fio::Flags::empty());

    // Ensure we bound the GPT by checking the relevant partitions exist under the ramdisk path.
    let fvm_partition_path = device_watcher::wait_for_device_with(&dev, |info| {
        info.topological_path.ends_with("/part-000/block").then(|| info.topological_path)
    })
    .await
    .unwrap();
    let blobfs_path = format!("{}/fvm/blobfs-p-1/block", fvm_partition_path);
    device_watcher::wait_for_device_with(&dev, |info| {
        (info.topological_path == blobfs_path).then_some(())
    })
    .await
    .unwrap();
    let data_path = format!("{}/fvm/data-p-2/block", fvm_partition_path);
    device_watcher::wait_for_device_with(&dev, |info| {
        (info.topological_path == data_path).then_some(())
    })
    .await
    .unwrap();

    // Make sure we can access the blob/data partitions within the FVM.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture.tear_down().await;
}

// Test that fshost handles the case where Fxblob is within a GPT partition.
#[fuchsia::test]
#[cfg(feature = "fxblob")]
async fn fxblob_within_gpt() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).with_gpt().format_data(data_fs_spec());
    let fixture = builder.build().await;

    // Make sure we can access the blob/data partitions within the FVM.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.check_test_blob(DATA_FILESYSTEM_VARIANT == "fxblob").await;

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(feature = "fxfs", ignore)]
async fn shred_data_volume_not_supported() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let admin = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<AdminMarker>()
        .expect("connect_to_protcol_at_exposed_dir failed");

    admin
        .shred_data_volume()
        .await
        .expect("shred_data_volume FIDL failed")
        .expect_err("shred_data_volume should fail");

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(not(feature = "fxfs"), ignore)]
async fn shred_data_volume_when_mounted() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();

    fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::PERM_READABLE | fio::PERM_WRITABLE),
        "test-file",
        fio::Flags::FLAG_MAYBE_CREATE,
    )
    .await
    .expect("open_file failed");

    let admin = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<AdminMarker>()
        .expect("connect_to_protcol_at_exposed_dir failed");

    admin
        .shred_data_volume()
        .await
        .expect("shred_data_volume FIDL failed")
        .expect("shred_data_volume failed");

    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // If we try and open the same test file, it shouldn't exist because the data volume should have
    // been shredded.
    assert_matches!(
        fuchsia_fs::directory::open_file(
            &fixture.dir("data", fio::PERM_READABLE),
            "test-file",
            fio::PERM_READABLE,
        )
        .await
        .expect_err("open_file failed"),
        fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND)
    );

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(not(feature = "fxfs"), ignore)]
async fn shred_data_volume_from_recovery() {
    let mut builder = new_builder();
    builder.with_disk().with_gpt().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();

    fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::PERM_READABLE | fio::PERM_WRITABLE),
        "test-file",
        fio::Flags::FLAG_MAYBE_CREATE,
    )
    .await
    .expect("open_file failed");

    fixture.tear_down().await;

    // Launch a version of fshost that will behave like recovery: it will mount data and blob from
    // a ramdisk it launches, binding the fvm on the "regular" disk but otherwise leaving it alone.
    let mut builder =
        new_builder().with_disk_from_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap());
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let admin = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<AdminMarker>()
        .expect("connect_to_protcol_at_exposed_dir failed");

    admin
        .shred_data_volume()
        .await
        .expect("shred_data_volume FIDL failed")
        .expect("shred_data_volume failed");

    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // If we try and open the same test file, it shouldn't exist because the data volume should have
    // been shredded.
    assert_matches!(
        fuchsia_fs::directory::open_file(
            &fixture.dir("data", fio::PERM_READABLE),
            "test-file",
            fio::PERM_READABLE
        )
        .await
        .expect_err("open_file failed"),
        fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND)
    );

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn disable_block_watcher() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("disable_block_watcher", true);
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    // The filesystems are not mounted when the block watcher is disabled.
    futures::select! {
        _ = fixture.check_fs_type("data", data_fs_type()).fuse() => {
            panic!("check_fs_type returned unexpectedly - data was mounted");
        },
        _ = fixture.check_fs_type("blob", blob_fs_type()).fuse() => {
            panic!("check_fs_type returned unexpectedly - blob was mounted");
        },
        _ = fasync::Timer::new(std::time::Duration::from_secs(2)).fuse() => (),
    }

    fixture.tear_down().await;
}

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): Make this work with storage-host.
#[cfg_attr(any(feature = "fxblob", feature = "storage-host"), ignore)]
async fn reset_fvm_partitions() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("data_max_bytes", DEFAULT_DATA_VOLUME_SIZE);
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .with_account_and_virtualization()
        .data_volume_size(DEFAULT_DATA_VOLUME_SIZE / 2);
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let fvm_proxy = fuchsia_fs::directory::open_directory(
        &fixture.dir("dev-topological", fio::Flags::empty()),
        "sys/platform/ram-disk/ramctl/ramdisk-0/block/fvm",
        fio::Flags::empty(),
    )
    .await
    .expect("Failed to open the fvm");

    // Ensure that the account and virtualization partitions were successfully destroyed. The
    // partitions are removed from devfs asynchronously, so use a timeout.
    let start_time = std::time::Instant::now();
    let mut dir_entries = fuchsia_fs::directory::readdir(&fvm_proxy)
        .await
        .expect("Failed to readdir the fvm DirectoryProxy");
    while dir_entries
        .iter()
        .find(|x| x.name.contains("account") || x.name.contains("virtualization"))
        .is_some()
    {
        let elapsed = start_time.elapsed().as_secs() as u64;
        if elapsed >= 30 {
            panic!("The account or virtualization partition still exists in devfs after 30 secs");
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        dir_entries = fuchsia_fs::directory::readdir(&fvm_proxy)
            .await
            .expect("Failed to readdir the fvm DirectoryProxy");
    }

    let mut count = 0;
    let mut data_name = "".to_string();
    for entry in dir_entries {
        let name = entry.name;
        if name.contains("blobfs") || name.contains("device") {
            count += 1;
        } else if name.contains("data") {
            data_name = name;
            count += 1;
        } else {
            panic!("Unexpected entry name: {name}");
        }
    }
    assert_eq!(count, 4);
    assert_ne!(&data_name, "");

    // Ensure that the data partition got larger. We exclude minfs because it automatically handles
    // resizing and does not call resize_volume() inside format_data.
    if DATA_FILESYSTEM_FORMAT != "minfs" {
        let data_volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.dir("dev-topological", fio::Flags::empty()),
            &format!("sys/platform/ram-disk/ramctl/ramdisk-0/block/fvm/{}/block", data_name),
        )
        .expect("Failed to connect to data VolumeProxy");
        let (status, manager_info, volume_info) =
            data_volume_proxy.get_volume_info().await.expect("transport error on get_volume_info");
        zx::Status::ok(status).expect("get_volume_info failed");
        let manager_info = manager_info.expect("get_volume_info returned no volume manager info");
        let volume_info = volume_info.expect("get_volume_info returned no volume info");
        let slice_size = manager_info.slice_size;
        let slice_count = volume_info.partition_slice_count;
        // We expect the partition to be at least as big as we requested...
        assert!(
            slice_size * slice_count >= DEFAULT_DATA_VOLUME_SIZE,
            "{} >= {}",
            slice_size * slice_count,
            DEFAULT_DATA_VOLUME_SIZE
        );
        // ..but also no bigger than one extra slice and some rounding up.
        assert!(
            slice_size * slice_count <= DEFAULT_DATA_VOLUME_SIZE + slice_size,
            "{} <= {}",
            slice_size * slice_count,
            DEFAULT_DATA_VOLUME_SIZE + slice_size
        );
    }

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(feature = "fxblob", ignore)]
async fn reset_fvm_partitions_no_existing_data_partition() {
    // We do not bother with setting the "data_max_bytes" config value because we do not have an
    // existing data partition whose size we can compare with the newly allocated data partition.
    let mut builder = new_builder();
    builder
        .with_disk()
        .format_volumes(VolumesSpec { create_data_partition: false, ..volumes_spec() })
        .with_account_and_virtualization();
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let volumes_dir;
    let expected;
    #[cfg(feature = "storage-host")]
    {
        volumes_dir = fixture.dir("volumes", fio::Flags::empty());
        expected = [r"^blobfs$", r"^data$"];
    }
    #[cfg(not(feature = "storage-host"))]
    {
        volumes_dir = fuchsia_fs::directory::open_directory(
            &fixture.dir("dev-topological", fio::Flags::empty()),
            "sys/platform/ram-disk/ramctl/ramdisk-0/block/fvm",
            fio::Flags::empty(),
        )
        .await
        .expect("Failed to open the fvm");
        expected = [r"^blobfs", r"data", r"^device_controller$", r"^device_protocol$"];
    }

    let mut expected: Vec<_> = expected.into_iter().map(|r| Regex::new(r).unwrap()).collect();

    // Ensure that the account and virtualization partitions were successfully destroyed. The
    // partitions are removed from devfs asynchronously, so use a timeout.
    let start_time = std::time::Instant::now();
    let mut dir_entries =
        fuchsia_fs::directory::readdir(&volumes_dir).await.expect("Failed to readdir the volumes");
    while dir_entries
        .iter()
        .find(|x| x.name.contains("account") || x.name.contains("virtualization"))
        .is_some()
    {
        let elapsed = start_time.elapsed().as_secs() as u64;
        if elapsed >= 30 {
            panic!("The account or virtualization partition still exists in devfs after 30 secs");
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        dir_entries = fuchsia_fs::directory::readdir(&volumes_dir)
            .await
            .expect("Failed to readdir the fvm DirectoryProxy");
    }
    for entry in dir_entries {
        let name = entry.name;
        let position = expected
            .iter()
            .position(|r| r.is_match(&name))
            .unwrap_or_else(|| panic!("Unexpected entry name: {name}"));
        expected.swap_remove(position);
    }
    assert!(expected.is_empty(), "Missing {expected:?}");

    fixture.tear_down().await;
}

// Toggle migration mode

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): port to storage-host
#[cfg_attr(any(feature = "fxblob", feature = "storage-host"), ignore)]
async fn migration_toggle() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", DATA_MAX_BYTES)
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .data_volume_size(DATA_MAX_BYTES / 2)
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("minfs"), zxcrypt: true, ..Default::default() })
        .set_fs_switch("toggle");
    let fixture = builder.build().await;

    fixture.check_fs_type("data", VFS_TYPE_FXFS).await;
    fixture.check_test_data_file().await;

    let vmo = fixture.into_vmo().await.unwrap();

    let mut builder = new_builder();
    builder.fshost().set_config_value("data_max_bytes", DATA_MAX_BYTES / 2);
    let mut fixture = builder.build().await;
    fixture.set_ramdisk_vmo(vmo).await;

    fixture.check_fs_type("data", VFS_TYPE_MINFS).await;
    fixture.check_test_data_file().await;
    fixture.tear_down().await;
}

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): port to storage-host
#[cfg_attr(any(feature = "fxblob", feature = "storage-host"), ignore)]
async fn migration_to_fxfs() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", DATA_MAX_BYTES / 2)
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .data_volume_size(DATA_MAX_BYTES / 2)
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("minfs"), zxcrypt: true, ..Default::default() })
        .set_fs_switch("fxfs");
    let fixture = builder.build().await;

    fixture.check_fs_type("data", VFS_TYPE_FXFS).await;
    fixture.check_test_data_file().await;
    fixture.tear_down().await;
}

#[fuchsia::test]
// TODO(https://fxbug.dev/339491886): port to storage-host
#[cfg_attr(any(feature = "fxblob", feature = "storage-host"), ignore)]
async fn migration_to_minfs() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", DATA_MAX_BYTES / 2)
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .data_volume_size(DATA_MAX_BYTES / 2)
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("fxfs"), zxcrypt: false, ..Default::default() })
        .set_fs_switch("minfs");
    let fixture = builder.build().await;

    fixture.check_fs_type("data", VFS_TYPE_MINFS).await;
    fixture.check_test_data_file().await;
    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg(feature = "fxblob")]
async fn verify_blobs() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    let blobfs_verifier = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<BlobfsVerifierMarker>()
        .expect("connect_to_protcol_at_exposed_dir failed");
    blobfs_verifier
        .verify(&VerifyOptions::default())
        .await
        .expect("FIDL failure")
        .expect("verify failure");

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg(feature = "fxfs")]
async fn add_wrapping_key() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    let crypt_management = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<CryptManagementMarker>()
        .expect("connect_to_protcol_at_exposed_dir failed");
    let mut wrapping_key_id = [0; 16];
    wrapping_key_id[0] = 2;
    crypt_management
        .add_wrapping_key(&wrapping_key_id, &[1; 32])
        .await
        .expect("FIDL failure")
        .expect("add wrapping key failed");

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg_attr(feature = "fxblob", ignore)]
async fn delivery_blob_support() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("blobfs_max_bytes", BLOBFS_MAX_BYTES);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;
    // 65536 bytes of 0xff "small" f75f59a944d2433bc6830ec243bfefa457704d2aed12f30539cd4f18bf1d62cf
    const HASH: &'static str = "f75f59a944d2433bc6830ec243bfefa457704d2aed12f30539cd4f18bf1d62cf";
    let data: Vec<u8> = vec![0xff; 65536];
    let payload = Type1Blob::generate(&data, CompressionMode::Always);
    // `data` is highly compressible, so we should be able to transfer it in one write call.
    assert!((payload.len() as u64) < fio::MAX_TRANSFER_SIZE, "Payload exceeds max transfer size!");
    // Now attempt to write `payload` as we would any other blob, but using the delivery path.
    let blob = fuchsia_fs::directory::open_file(
        &fixture.dir("blob", fio::PERM_READABLE | fio::PERM_WRITABLE),
        &delivery_blob_path(HASH),
        fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_WRITABLE,
    )
    .await
    .expect("Failed to open delivery blob for writing.");
    // Resize to required length.
    blob.resize(payload.len() as u64).await.unwrap().map_err(zx::Status::from_raw).unwrap();
    // Write payload.
    let bytes_written = blob.write(&payload).await.unwrap().map_err(zx::Status::from_raw).unwrap();
    assert_eq!(bytes_written, payload.len() as u64);
    blob.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();

    // We should now be able to open the blob by its hash and read the contents back.
    let blob = fuchsia_fs::directory::open_file(
        &fixture.dir("blob", fio::PERM_READABLE),
        HASH,
        fio::PERM_READABLE,
    )
    .await
    .expect("Failed to open delivery blob for reading.");
    // Read the last 1024 bytes of the file and ensure the bytes match the original `data`.
    let len: u64 = 1024;
    let offset: u64 = data.len().checked_sub(1024).unwrap() as u64;
    let contents = blob.read_at(len, offset).await.unwrap().map_err(zx::Status::from_raw).unwrap();
    assert_eq!(contents.as_slice(), &data[offset as usize..]);

    fixture.tear_down().await;
}

#[fuchsia::test]
#[cfg(feature = "fxblob")]
async fn delivery_blob_support_fxblob() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("blobfs_max_bytes", BLOBFS_MAX_BYTES);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let data: Vec<u8> = vec![0xff; 65536];
    let hash = fuchsia_merkle::from_slice(&data).root();
    let payload = Type1Blob::generate(&data, CompressionMode::Always);

    let blob_creator = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<BlobCreatorMarker>()
        .expect("connect_to_protocol_at_exposed_dir failed");
    let blob_writer_client_end = blob_creator
        .create(&hash.into(), false)
        .await
        .expect("transport error on create")
        .expect("failed to create blob");

    let writer = blob_writer_client_end.into_proxy().unwrap();
    let mut blob_writer = BlobWriter::create(writer, payload.len() as u64)
        .await
        .expect("failed to create BlobWriter");
    blob_writer.write(&payload).await.unwrap();

    // We should now be able to open the blob by its hash and read the contents back.
    let blob_reader = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<BlobReaderMarker>()
        .expect("connect_to_protocol_at_exposed_dir failed");
    let vmo = blob_reader.get_vmo(&hash.into()).await.unwrap().unwrap();

    // Read the last 1024 bytes of the file and ensure the bytes match the original `data`.
    let mut buf = vec![0; 1024];
    let offset: u64 = data.len().checked_sub(1024).unwrap() as u64;
    let () = vmo.read(&mut buf, offset).unwrap();
    assert_eq!(&buf, &data[offset as usize..]);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn data_persists() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let data_root = fixture.dir("data", fio::PERM_READABLE | fio::PERM_WRITABLE);
    let file = fuchsia_fs::directory::open_file(
        &data_root,
        "file",
        fio::Flags::FLAG_MUST_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .await
    .unwrap();
    fuchsia_fs::file::write(&file, "file contents!").await.unwrap();

    // Shut down fshost, which should propagate to the data filesystem too.
    let vmo = fixture.into_vmo().await.unwrap();
    let builder = new_builder().with_disk_from_vmo(vmo);
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    let data_root = fixture.dir("data", fio::PERM_READABLE);
    let file =
        fuchsia_fs::directory::open_file(&data_root, "file", fio::PERM_READABLE).await.unwrap();
    assert_eq!(&fuchsia_fs::file::read(&file).await.unwrap()[..], b"file contents!");

    fixture.tear_down().await;
}
