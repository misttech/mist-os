// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_fshost as fshost,
    fidl_fuchsia_fshost::AdminMarker,
    fidl_fuchsia_hardware_block_volume::{VolumeManagerMarker, VolumeMarker},
    fidl_fuchsia_io as fio,
    fs_management::{
        format::constants::DATA_PARTITION_LABEL,
        partition::{find_partition_in, PartitionMatcher},
        DATA_TYPE_GUID,
    },
    fshost_test_fixture::disk_builder::DEFAULT_DATA_VOLUME_SIZE,
    fshost_test_fixture::{
        disk_builder::{DataSpec, VolumesSpec, FVM_SLICE_SIZE},
        TestFixtureBuilder,
    },
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_fs::node::OpenError,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::FutureExt,
};

#[cfg(feature = "fxblob")]
use fidl_fuchsia_update_verify::{BlobfsVerifierMarker, VerifyOptions};

mod migration;
mod wipe_storage;
mod write_data_file;

const FSHOST_COMPONENT_NAME: &'static str = std::env!("FSHOST_COMPONENT_NAME");
const DATA_FILESYSTEM_FORMAT: &'static str = std::env!("DATA_FILESYSTEM_FORMAT");
const DATA_FILESYSTEM_VARIANT: &'static str = std::env!("DATA_FILESYSTEM_VARIANT");

fn new_builder() -> TestFixtureBuilder {
    TestFixtureBuilder::new(FSHOST_COMPONENT_NAME)
}

// const VFS_TYPE_FATFS: u32 = 0xce694d21;
const VFS_TYPE_BLOBFS: u32 = 0x9e694d21;
const VFS_TYPE_MINFS: u32 = 0x6e694d21;
const VFS_TYPE_MEMFS: u32 = 0x3e694d21;
// const VFS_TYPE_FACTORYFS: u32 = 0x1e694d21;
const VFS_TYPE_FXFS: u32 = 0x73667866;
const VFS_TYPE_F2FS: u32 = 0xfe694d21;
const BLOBFS_MAX_BYTES: u64 = 8765432;
// DATA_MAX_BYTES must be greater than DEFAULT_F2FS_MIN_BYTES
// (defined in device/constants.rs) to ensure that when f2fs is
// the data filesystem format, we don't run out of space
const DATA_MAX_BYTES: u64 = 109876543;

fn blob_fs_type() -> u32 {
    if DATA_FILESYSTEM_VARIANT == "fxblob" {
        VFS_TYPE_FXFS
    } else {
        VFS_TYPE_BLOBFS
    }
}

fn data_fs_type() -> u32 {
    match DATA_FILESYSTEM_FORMAT {
        "f2fs" => VFS_TYPE_F2FS,
        "fxfs" => VFS_TYPE_FXFS,
        "minfs" => VFS_TYPE_MINFS,
        _ => panic!("invalid data filesystem format"),
    }
}

fn data_fs_name() -> &'static str {
    match DATA_FILESYSTEM_FORMAT {
        "f2fs" => "f2fs",
        "fxfs" => "fxfs",
        "minfs" => "minfs",
        _ => panic!("invalid data filesystem format"),
    }
}

fn data_fs_zxcrypt() -> bool {
    !DATA_FILESYSTEM_VARIANT.ends_with("no-zxcrypt")
}

fn volumes_spec() -> VolumesSpec {
    VolumesSpec { fxfs_blob: DATA_FILESYSTEM_VARIANT == "fxblob" }
}

fn data_fs_spec() -> DataSpec {
    DataSpec { format: Some(data_fs_name()), zxcrypt: data_fs_zxcrypt() }
}

async fn check_delivery_blob_support(
    blobfs: fio::DirectoryProxy,
    should_allow_delivery_blobs: bool,
) {
    // We just use the null blob's hash as a test.
    let hash = "15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b";
    let delivery_path = format!("v1-{}", hash);
    let blob = fuchsia_fs::directory::open_file(
        &blobfs,
        &delivery_path,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
    )
    .await;
    if should_allow_delivery_blobs {
        assert!(blob.is_ok(), "Failed to open delivery blob for writing: {:?}", &blob);
    } else {
        // If delivery blob support is disabled, we should get NOT_SUPPORTED.
        assert!(blob.is_err(), "Succeeded opening delivery blob when feature disabled.");
        match blob.unwrap_err() {
            OpenError::OpenError(status) => assert_eq!(status, zx::Status::NOT_SUPPORTED),
            other => panic!(
                "Unexpected error when opening delivery blob (expected NOT_SUPPORTED): {:?}",
                other
            ),
        }
    }
}

#[fuchsia::test]
async fn blobfs_and_data_mounted() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.check_test_blob().await;

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
    fixture.check_test_blob().await;

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
    let (file, server) = create_proxy::<fio::NodeMarker>().unwrap();
    fixture
        .dir("data", fio::OpenFlags::RIGHT_READABLE)
        .open(fio::OpenFlags::RIGHT_READABLE, fio::ModeType::empty(), "foo", server)
        .expect("open failed");
    file.get_attr().await.expect_err("foo shouldn't exist");

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
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

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
    let fixture = builder.build().await;

    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

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
        .wipe_storage(blobfs_server)
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
    fixture.check_test_blob().await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn ramdisk_data_ignores_non_ramdisk() {
    let mut builder = new_builder();
    // Fake out the cpp fshost ramdisk checking by providing a nonsense ramdisk prefix. The rust
    // fshost has tighter behavior - it only matches on the ramdisk path it created itself (which
    // is also not this one).
    builder.fshost().set_config_value("ramdisk_image", true);
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .format_data(DataSpec { zxcrypt: false, ..data_fs_spec() });
    let fixture = builder.build().await;

    if DATA_FILESYSTEM_VARIANT != "fxblob" {
        let dev = fixture.dir("dev-topological/class/block", fio::OpenFlags::empty());

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
#[cfg_attr(feature = "fxblob", ignore)]
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
    // TODO(https://fxbug.dev/121274): Remove hardcoded paths
    let volume_proxy_data = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
        &fixture.dir("dev-topological", fio::OpenFlags::empty()),
        "sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm/blobfs-p-1/block",
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

    let dev = fixture.dir("dev-topological/class/block", fio::OpenFlags::empty());
    let data_partition_controller =
        find_partition_in(&dev, data_matcher, zx::Duration::from_seconds(10))
            .await
            .expect("failed to find data partition");

    // Get the data instance guid.
    let (volume_proxy, volume_server_end) =
        fidl::endpoints::create_proxy::<VolumeMarker>().unwrap();
    data_partition_controller.connect_to_device_fidl(volume_server_end.into_channel()).unwrap();

    let (status, data_instance_guid) = volume_proxy.get_instance_guid().await.unwrap();
    zx::Status::ok(status).unwrap();
    let mut data_instance_guid = data_instance_guid.unwrap();

    // TODO(https://fxbug.dev/121274): Remove hardcoded paths
    let fvm_proxy = connect_to_named_protocol_at_dir_root::<VolumeManagerMarker>(
        &fixture.dir("dev-topological", fio::OpenFlags::empty()),
        "sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm",
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
async fn tmp_is_available() {
    let builder = new_builder();
    let fixture = builder.build().await;

    fixture.check_fs_type("tmp", VFS_TYPE_MEMFS).await;
}

#[fuchsia::test]
async fn netboot_set() {
    // Set the netboot flag
    let mut builder = new_builder().netboot();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    if DATA_FILESYSTEM_VARIANT != "fxblob" {
        let dev = fixture.dir("dev-topological/class/block", fio::OpenFlags::empty());

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
#[cfg_attr(feature = "fxblob", ignore)]
async fn fvm_within_gpt() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).with_gpt().format_data(data_fs_spec());
    let fixture = builder.build().await;
    let dev = fixture.dir("dev-topological/class/block", fio::OpenFlags::empty());

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
    fixture.check_test_blob().await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn pausing_block_watcher_ignores_devices() {
    // The first disk has a blank data filesystem
    let mut disk_builder1 = fshost_test_fixture::disk_builder::DiskBuilder::new();
    disk_builder1.format_volumes(volumes_spec());
    let disk_vmo = disk_builder1.build().await;

    // The second disk has a formatted data filesystem with a test file inside it.
    let mut disk_builder2 = fshost_test_fixture::disk_builder::DiskBuilder::new();
    disk_builder2.format_volumes(volumes_spec()).format_data(data_fs_spec());
    let disk_vmo2 = disk_builder2.build().await;

    let mut fixture = new_builder().build().await;
    let pauser = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<fshost::BlockWatcherMarker>()
        .unwrap();
    let dev = fixture.dir("dev-topological/class/block", fio::OpenFlags::empty());

    // A block device added when the block watcher is paused doesn't do anything.
    assert_eq!(pauser.pause().await.unwrap(), zx::Status::OK.into_raw());
    fixture.add_ramdisk(disk_vmo).await;
    // Wait for the block device entry to appear in devfs before resuming.
    device_watcher::wait_for_device_with(&dev, |info| {
        info.topological_path.ends_with("ramdisk-0/block").then_some(())
    })
    .await
    .unwrap();

    // A block device added after the block watcher is resumed is processed.
    assert_eq!(pauser.resume().await.unwrap(), zx::Status::OK.into_raw());
    fixture.add_ramdisk(disk_vmo2).await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;
    // Only the second disk we attached has a file inside. We use it as a proxy for testing that
    // only the second one was processed.
    fixture.check_test_data_file().await;
    fixture.check_test_blob().await;

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn block_watcher_second_pause_fails() {
    let fixture = new_builder().build().await;
    let pauser = fixture
        .realm
        .root
        .connect_to_protocol_at_exposed_dir::<fshost::BlockWatcherMarker>()
        .unwrap();
    assert_eq!(pauser.pause().await.unwrap(), zx::Status::OK.into_raw());
    // A paused block watcher should fail if paused a second time
    assert_eq!(pauser.pause().await.unwrap(), zx::Status::BAD_STATE.into_raw());
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
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE),
        "test-file",
        fio::OpenFlags::CREATE,
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
            &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
            "test-file",
            fio::OpenFlags::RIGHT_READABLE
        )
        .await
        .expect_err("open_file failed"),
        fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND)
    );
}

// TODO(https://fxbug.dev/122940) shred_data_volume in recovery is not supported for fxblob.
#[fuchsia::test]
#[cfg_attr(any(not(feature = "fxfs"), feature = "fxblob"), ignore)]
async fn shred_data_volume_from_recovery() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();

    fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE),
        "test-file",
        fio::OpenFlags::CREATE,
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
            &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
            "test-file",
            fio::OpenFlags::RIGHT_READABLE
        )
        .await
        .expect_err("open_file failed"),
        fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND)
    );
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
#[cfg_attr(feature = "fxblob", ignore)]
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
        &fixture.dir("dev-topological", fio::OpenFlags::empty()),
        "sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm",
        fio::OpenFlags::empty(),
    )
    .await
    .expect("Failed to open the fvm");

    // Ensure that the account and virtualization partitions
    // were successfully destroyed
    let dir_entries = fuchsia_fs::directory::readdir(&fvm_proxy)
        .await
        .expect("Failed to readdir the fvm DirectoryProxy");
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
            &fixture.dir("dev-topological", fio::OpenFlags::empty()),
            &format!("sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm/{}/block", data_name),
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
            DEFAULT_DATA_VOLUME_SIZE + 2 * slice_size
        );
    }
}

// Toggle migration mode

#[fuchsia::test]
#[cfg_attr(feature = "fxblob", ignore)]
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
#[cfg_attr(feature = "fxblob", ignore)]
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
#[cfg_attr(feature = "fxblob", ignore)]
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

// TODO(https://fxbug.dev/122125): fxblob delivery blob support.
#[fuchsia::test]
#[cfg_attr(feature = "fxblob", ignore)]
async fn delivery_blob_config() {
    for allow_delivery_blobs in [false, true] {
        let mut builder = new_builder();
        builder.fshost().set_config_value("blobfs_allow_delivery_blobs", allow_delivery_blobs);
        builder.with_disk().format_volumes(volumes_spec());
        let fixture = builder.build().await;
        // Ensure we succeed/fail to create a delivery blob if the feature is enabled/disabled.
        check_delivery_blob_support(
            fixture.dir("blob", fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE),
            allow_delivery_blobs,
        )
        .await;
        fixture.tear_down().await;
    }
}
