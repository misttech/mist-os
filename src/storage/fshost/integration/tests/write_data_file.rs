// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test cases which simulate fshost running in the configuration used in recovery builds (which,
//! among other things, sets the ramdisk_image flag to prevent binding of the on-disk filesystems.)

use {
    super::{
        blob_fs_type, data_fs_name, data_fs_spec, data_fs_type, new_builder, volumes_spec,
        VolumesSpec,
    },
    fidl_fuchsia_fshost as fshost, fidl_fuchsia_io as fio,
    fshost::{AdminProxy, AdminWriteDataFileResult},
    fuchsia_zircon::{self as zx, HandleBased as _},
};

const PAYLOAD: &[u8] = b"top secret stuff";
const SECRET_FILE_NAME: &'static str = "inconspicuous/secret.txt";
const SMALL_DISK_SIZE: u64 = 25165824;

async fn call_write_data_file(admin: &AdminProxy) -> AdminWriteDataFileResult {
    let vmo = zx::Vmo::create(1024).unwrap();
    vmo.write(PAYLOAD, 0).unwrap();
    vmo.set_content_size(&(PAYLOAD.len() as u64)).unwrap();
    admin
        .write_data_file(SECRET_FILE_NAME, vmo)
        .await
        .expect("write_data_file failed: transport error")
}

#[fuchsia::test]
async fn unformatted() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_disk().format_volumes(volumes_spec());
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    call_write_data_file(&admin).await.expect("write_data_file failed");
    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        SECRET_FILE_NAME,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}

// TODO(https://fxbug.dev/122943) write_data_file not supported for fxblob.
#[fuchsia::test]
#[cfg_attr(feature = "fxblob", ignore)]
async fn no_existing_data_partition() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_disk().format_volumes(volumes_spec());
    builder
        .with_zbi_ramdisk()
        .format_volumes(VolumesSpec { create_data_partition: false, ..volumes_spec() });

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    call_write_data_file(&admin).await.expect("write_data_file failed");
    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // Ensure the blob volume is present and unmodified.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        SECRET_FILE_NAME,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn unformatted_netboot() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("netboot", true);
    builder.with_disk().format_volumes(volumes_spec());
    let fixture = builder.build().await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    call_write_data_file(&admin).await.expect("write_data_file failed");
    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // Ensure the blob volume is present and unmodified.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        SECRET_FILE_NAME,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn unformatted_small_disk() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .size(SMALL_DISK_SIZE)
        .data_volume_size(SMALL_DISK_SIZE / 2);

    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    if data_fs_name() == "f2fs" {
        call_write_data_file(&admin).await.expect_err("write_data_file succeeded");
        return;
    }
    call_write_data_file(&admin).await.expect("write_data_file failed");
    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // Ensure the blob volume is present and unmodified.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        SECRET_FILE_NAME,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn formatted() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    call_write_data_file(&admin).await.expect("write_data_file failed");
    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // Ensure the blob volume is present and unmodified.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    // Make sure the original contents in the data partition still exist.
    fixture.check_test_data_file().await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        SECRET_FILE_NAME,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn formatted_file_in_root() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("ramdisk_image", true);
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    builder.with_zbi_ramdisk().format_volumes(volumes_spec());

    let fixture = builder.build().await;
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_fs_type("data", data_fs_type()).await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();

    let vmo = zx::Vmo::create(1024).unwrap();
    vmo.write(PAYLOAD, 0).unwrap();
    vmo.set_content_size(&(PAYLOAD.len() as u64)).unwrap();
    admin
        .write_data_file("test.txt", vmo)
        .await
        .expect("write_data_file failed: transport error")
        .unwrap();

    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // Ensure the blob volume is present and unmodified.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    // Make sure the original contents in the data partition still exist.
    fixture.check_test_data_file().await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        "test.txt",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn formatted_netboot() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("netboot", true);
    builder.with_disk().format_volumes(volumes_spec()).format_data(data_fs_spec());
    let fixture = builder.build().await;

    let admin =
        fixture.realm.root.connect_to_protocol_at_exposed_dir::<fshost::AdminMarker>().unwrap();
    call_write_data_file(&admin).await.expect("write_data_file failed");
    let vmo = fixture.ramdisk_vmo().unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
    fixture.tear_down().await;

    let fixture = new_builder().with_disk_from_vmo(vmo).build().await;

    // Ensure the blob volume is present and unmodified.
    fixture.check_fs_type("blob", blob_fs_type()).await;
    fixture.check_test_blob().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    // Make sure the original contents in the data partition still exist.
    fixture.check_test_data_file().await;

    let secret = fuchsia_fs::directory::open_file(
        &fixture.dir("data", fio::OpenFlags::RIGHT_READABLE),
        SECRET_FILE_NAME,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    assert_eq!(&fuchsia_fs::file::read(&secret).await.unwrap(), PAYLOAD);

    fixture.tear_down().await;
}
