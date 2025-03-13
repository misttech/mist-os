// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This set of tests covers the matrix of data format handling for each possible original and goal
//! state of a device.
//!
//! For original device state, here are 8 theoretical categories we care about with respect to the
//! data filesystem, from the perspective of data migration -
//!   - unknown/blank
//!   - formatted with fxfs
//!   - formatted with f2fs
//!   - formatted with minfs
//!   - formatted with zxcrypt with unknown/blank inside
//!   - formatted with zxcrypt+fxfs (unlikely, so not covered by these tests)
//!   - formatted with zxcrypt+f2fs
//!   - formatted with zxcrypt+minfs
//!
//! For goal state, there are 5 possibilities, based on fshost configuration.
//!   - minfs
//!   - f2fs
//!   - fxfs
//!   - zxcrypt+minfs
//!   - zxcrypt+f2fs
//!
//! If the original state doesn't match the goal state, we format the device with the goal state.
//! If the use_disk_migration build flag is in use, contents will be migrated. Otherwise
//! any existing content will be erased.
//!
//! A set of fshost integration tests are generated for each of the three filesystem types. For
//! filesystems which use zxcrypt, there is a set for both with and without zxcrypt. This covers
//! all the goal states, so we just need one test for each original state.

pub mod config;
use config::{data_fs_name, data_fs_type, new_builder, volumes_spec};
use fshost_test_fixture::disk_builder::{DataSpec, FVM_SLICE_SIZE};
use fshost_test_fixture::{round_down, DATA_MAX_BYTES};

/// f2fs requires more space than other filesystems so we use different values for it..
fn data_max_bytes() -> u64 {
    if data_fs_name() == "f2fs" {
        100 * 1024 * 1024
    } else {
        round_down(DATA_MAX_BYTES / 2, FVM_SLICE_SIZE)
    }
}
fn disk_size_bytes() -> u64 {
    data_max_bytes() * 2 + 4 * 1024 * 1024
}

// Original state - unknown/blank

#[fuchsia::test]
async fn none_to_format() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", data_max_bytes())
        .set_config_value("use_disk_migration", true);
    builder.with_disk().size(disk_size_bytes()).data_volume_size(data_max_bytes());
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Original state - fxfs

#[fuchsia::test]
async fn fxfs_to_format() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", data_max_bytes())
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .size(disk_size_bytes())
        .data_volume_size(data_max_bytes())
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("fxfs"), ..Default::default() })
        .set_fs_switch(data_fs_name());
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.tear_down().await;
}

// Original state - zxcrypt with unknown/blank

#[fuchsia::test]
async fn zxcrypt_to_format() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", data_max_bytes())
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .size(disk_size_bytes())
        .data_volume_size(data_max_bytes())
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: None, zxcrypt: true, ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Original state - zxcrypt with minfs

#[fuchsia::test]
async fn minfs_to_format() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", data_max_bytes())
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .size(disk_size_bytes())
        .data_volume_size(data_max_bytes())
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("minfs"), zxcrypt: true, ..Default::default() })
        .set_fs_switch(data_fs_name());
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.tear_down().await;
}

// Original state - zxcrypt with f2fs

#[fuchsia::test]
async fn f2fs_to_format() {
    let mut builder = new_builder();
    builder
        .fshost()
        .set_config_value("data_max_bytes", data_max_bytes())
        .set_config_value("use_disk_migration", true);
    builder
        .with_disk()
        .size(disk_size_bytes())
        .data_volume_size(data_max_bytes())
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("f2fs"), zxcrypt: true, ..Default::default() })
        .set_fs_switch(data_fs_name());
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.check_test_data_file().await;
    fixture.tear_down().await;
}
