// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;
use {fidl_fuchsia_io as fio, zx};

#[fuchsia::test]
async fn file_write_with_sufficient_rights() {
    let harness = TestHarness::new().await;

    for file_flags in harness
        .file_rights
        .valid_combos_with(fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY)
    {
        let root = root_directory(vec![file(TEST_FILE, vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());

        let file = open_node::<fio::FileMarker>(&test_dir, file_flags, TEST_FILE).await;
        let _: u64 = file
            .write("".as_bytes())
            .await
            .expect("write failed")
            .map_err(zx::Status::from_raw)
            .expect("write error");
    }
}

#[fuchsia::test]
async fn file_write_with_insufficient_rights() {
    let harness = TestHarness::new().await;

    for file_flags in harness
        .file_rights
        .valid_combos_without(fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY)
    {
        let root = root_directory(vec![file(TEST_FILE, vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());

        let file = open_node::<fio::FileMarker>(&test_dir, file_flags, TEST_FILE).await;
        let result =
            file.write("".as_bytes()).await.expect("write failed").map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::BAD_HANDLE))
    }
}

#[fuchsia::test]
async fn file_write_at_with_sufficient_rights() {
    let harness = TestHarness::new().await;

    for file_flags in harness
        .file_rights
        .valid_combos_with(fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY)
    {
        let root = root_directory(vec![file(TEST_FILE, vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());

        let file = open_node::<fio::FileMarker>(&test_dir, file_flags, TEST_FILE).await;
        let _: u64 = file
            .write_at("".as_bytes(), 0)
            .await
            .expect("write_at failed")
            .map_err(zx::Status::from_raw)
            .expect("write_at error");
    }
}

#[fuchsia::test]
async fn file_write_at_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    for file_flags in harness
        .file_rights
        .valid_combos_without(fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY)
    {
        let root = root_directory(vec![file(TEST_FILE, vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());

        let file = open_node::<fio::FileMarker>(&test_dir, file_flags, TEST_FILE).await;
        let result = file
            .write_at("".as_bytes(), 0)
            .await
            .expect("write_at failed")
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::BAD_HANDLE));
    }
}
