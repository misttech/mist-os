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
//! If the original state doesn't match the goal state, we format the device with the goal state,
//! disregarding any contents of the original filesystem.
//!
//! A set of fshost integration tests are generated for each of the three filesystem types. For
//! filesystems which use zxcrypt, there is a set for both with and without zxcrypt. This covers
//! all the goal states, so we just need one test for each original state.

use crate::{data_fs_name, data_fs_spec, data_fs_type, data_fs_zxcrypt, new_builder, volumes_spec};

// Original state - unknown/blank

#[fuchsia::test]
async fn none_to_format() {
    let mut builder = new_builder();
    builder.with_disk(data_fs_spec());
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Original state - fxfs

#[fuchsia::test]
async fn fxfs_to_format() {
    let mut builder = new_builder();
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("fxfs"), ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.tear_down().await;
}

// Original state - minfs with no zxcrypt

#[fuchsia::test]
async fn minfs_no_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("minfs"), ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.tear_down().await;
}

// Original state - f2fs with no zxcrypt

#[fuchsia::test]
async fn f2fs_no_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder
        .with_disk()
        .format_volumes(volumes_spec())
        .format_data(DataSpec { format: Some("f2fs"), ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.tear_down().await;
}

// Original state - zxcrypt with unknown/blank

#[fuchsia::test]
async fn zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(DataSpec {
        format: None,
        zxcrypt: true,
        ..Default::default()
    });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Original state - zxcrypt with minfs

#[fuchsia::test]
async fn minfs_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(DataSpec {
        format: Some("minfs"),
        zxcrypt: true,
        ..Default::default()
    });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.tear_down().await;
}

// Original state - zxcrypt with f2fs

#[fuchsia::test]
async fn f2fs_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_volumes(volumes_spec()).format_data(DataSpec {
        format: Some("f2fs"),
        zxcrypt: true,
        ..Default::default()
    });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;
    fixture.tear_down().await;
}
