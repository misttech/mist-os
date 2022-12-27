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
//! In 4 of these scenarios, we migrate the data on the partition to the new format.
//!   - minfs         -> f2fs
//!   - minfs         -> fxfs
//!   - zxcrypt+minfs -> fxfs
//!   - zxcrypt+minfs -> zxcrypt+f2fs
//!
//! In the rest of the combinations, if the original state doesn't match the goal state, we format
//! the device with the goal state, disregarding any contents of the original filesystem.
//!
//! A set of fshost integration tests are generated for each of the three filesystem types. For
//! filesystems which use zxcrypt, there is a set for both with and without zxcrypt. This covers
//! all the goal states, so we just need one test for each original state.

use {
    crate::{data_fs_name, data_fs_type, data_fs_zxcrypt, new_builder},
    fshost_test_fixture::disk_builder::DataSpec,
};

// Original state - unknown/blank

#[fuchsia::test]
async fn none_to_format() {
    let mut builder = new_builder();
    builder.with_disk();
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Original state - fxfs

#[fuchsia::test]
async fn fxfs_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_data(DataSpec { format: Some("fxfs"), ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    if data_fs_name() == "fxfs" {
        // Original state matches goal state
        fixture.check_test_data_file().await;
    }

    fixture.tear_down().await;
}

// Original state - minfs with no zxcrypt

#[fuchsia::test]
async fn minfs_no_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_data(DataSpec { format: Some("minfs"), ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    // This covers the minfs->fxfs and minfs->f2fs migrations outlined above.
    // TODO(fxbug.dev/109293): support migrating data in the rust fshost
    #[cfg(not(feature = "fshost_rust"))]
    if data_fs_name() == "fxfs" || (data_fs_name() == "f2fs" && !data_fs_zxcrypt()) {
        fixture.check_test_data_file().await;
    }

    if data_fs_name() == "minfs" && !data_fs_zxcrypt() {
        // Original state matches goal state
        fixture.check_test_data_file().await;
    }

    fixture.tear_down().await;
}

// Original state - f2fs with no zxcrypt

#[fuchsia::test]
async fn f2fs_no_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_data(DataSpec { format: Some("f2fs"), ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    if data_fs_name() == "f2fs" && !data_fs_zxcrypt() {
        // Original state matches goal state
        fixture.check_test_data_file().await;
    }

    fixture.tear_down().await;
}

// Original state - zxcrypt with unknown/blank

#[fuchsia::test]
async fn zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_data(DataSpec { format: None, zxcrypt: true, ..Default::default() });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    fixture.tear_down().await;
}

// Original state - zxcrypt with minfs

#[fuchsia::test]
async fn minfs_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_data(DataSpec {
        format: Some("minfs"),
        zxcrypt: true,
        ..Default::default()
    });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    // This covers the zxcrypt+minfs->fxfs and zxcrypt+minfs->f2fs migrations outlined above.

    // TODO(fxbug.dev/109293): support migrating data in the rust fshost
    #[cfg(not(feature = "fshost_rust"))]
    if data_fs_name() == "f2fs" && data_fs_zxcrypt() {
        fixture.check_test_data_file().await;
    }

    if (data_fs_name() == "minfs" && data_fs_zxcrypt()) || data_fs_name() == "fxfs" {
        fixture.check_test_data_file().await;
    }

    fixture.tear_down().await;
}

// Original state - zxcrypt with f2fs

#[fuchsia::test]
async fn f2fs_zxcrypt_to_format() {
    let mut builder = new_builder();
    builder.with_disk().format_data(DataSpec {
        format: Some("f2fs"),
        zxcrypt: true,
        ..Default::default()
    });
    let fixture = builder.build().await;

    fixture.check_fs_type("data", data_fs_type()).await;

    if data_fs_name() == "f2fs" && data_fs_zxcrypt() {
        // Original state matches goal state
        fixture.check_test_data_file().await;
    }

    fixture.tear_down().await;
}
