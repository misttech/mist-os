// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    io_conformance_util::{test_harness::TestHarness, *},
};

#[fuchsia::test]
async fn create_directory_with_create_if_absent_flag() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_create {
        return;
    }

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    let mnt_dir = open_dir_with_flags(
        &root_dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE_IF_ABSENT | fio::OpenFlags::CREATE,
        "mnt",
    )
    .await;
    let _tmp_dir = open_dir_with_flags(
        &mnt_dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE_IF_ABSENT | fio::OpenFlags::CREATE,
        "tmp",
    )
    .await;

    let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

    root_dir
        .open(
            fio::OpenFlags::CREATE_IF_ABSENT
                | fio::OpenFlags::CREATE
                | fio::OpenFlags::DESCRIBE
                | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            "mnt/tmp/foo",
            server,
        )
        .expect("Cannot open file");

    assert_eq!(get_open_status(&client).await, zx::Status::OK);
}

#[fuchsia::test]
async fn create_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_create {
        return;
    }

    for dir_flags in harness.file_rights.valid_combos_with(fio::OpenFlags::RIGHT_WRITABLE) {
        let root = root_directory(vec![]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        // Re-open directory with the flags being tested.
        let dir = open_dir_with_flags(&test_dir, dir_flags, ".").await;
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

        dir.open(
            dir_flags
                | fio::OpenFlags::CREATE
                | fio::OpenFlags::DESCRIBE
                | fio::OpenFlags::NOT_DIRECTORY,
            fio::ModeType::empty(),
            TEST_FILE,
            server,
        )
        .expect("Cannot open file");

        assert_eq!(get_open_status(&client).await, zx::Status::OK);
        assert_eq!(read_file(&test_dir, TEST_FILE).await, &[]);
    }
}

#[fuchsia::test]
async fn create_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_create {
        return;
    }

    for dir_flags in harness.file_rights.valid_combos_without(fio::OpenFlags::RIGHT_WRITABLE) {
        let root = root_directory(vec![]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        // Re-open directory with the flags being tested.
        let dir = open_dir_with_flags(&test_dir, dir_flags, ".").await;
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

        dir.open(
            dir_flags
                | fio::OpenFlags::CREATE
                | fio::OpenFlags::DESCRIBE
                | fio::OpenFlags::NOT_DIRECTORY,
            fio::ModeType::empty(),
            TEST_FILE,
            server,
        )
        .expect("Cannot open file");

        assert_eq!(get_open_status(&client).await, zx::Status::ACCESS_DENIED);
        assert_file_not_found(&test_dir, TEST_FILE).await;
    }
}
