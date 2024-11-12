// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn unlink_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in harness
        .dir_rights
        .combinations_containing_deprecated(fio::Rights::READ_BYTES | fio::Rights::WRITE_BYTES)
    {
        let entries = vec![directory("src", vec![file("file.txt", contents.to_vec())])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;

        let file = open_node::<fio::FileMarker>(
            &src_dir,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "file.txt",
        )
        .await;

        src_dir
            .unlink("file.txt", &fio::UnlinkOptions::default())
            .await
            .expect("unlink fidl failed")
            .expect("unlink failed");

        // Check file is gone.
        assert_file_not_found(&dir, "src/file.txt").await;

        // Ensure file connection remains usable.
        let read_result = file
            .read(contents.len() as u64)
            .await
            .expect("read failed")
            .map_err(zx::Status::from_raw)
            .expect("read error");

        assert_eq!(read_result, contents);
    }
}

#[fuchsia::test]
async fn unlink_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in harness.dir_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let entries = vec![directory("src", vec![file("file.txt", contents.to_vec())])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;

        assert_eq!(
            src_dir
                .unlink("file.txt", &fio::UnlinkOptions::default())
                .await
                .expect("unlink fidl failed")
                .expect_err("unlink succeeded"),
            zx::sys::ZX_ERR_BAD_HANDLE
        );

        // Check file still exists.
        assert_eq!(read_file(&dir, "src/file.txt").await, contents);
    }
}

#[fuchsia::test]
async fn unlink_directory_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    for dir_flags in harness.dir_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![directory("src", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        // Re-open dir with flags being tested.
        let dir = open_dir_with_flags(&dir, dir_flags, ".").await;

        dir.unlink("src", &fio::UnlinkOptions::default())
            .await
            .expect("unlink fidl failed")
            .expect("unlink failed");
    }
}

#[fuchsia::test]
async fn unlink_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    for dir_flags in harness.dir_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let entries = vec![directory("src", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        // Re-open dir with flags being tested.
        let dir = open_dir_with_flags(&dir, dir_flags, ".").await;

        assert_eq!(
            dir.unlink("src", &fio::UnlinkOptions::default())
                .await
                .expect("unlink fidl failed")
                .expect_err("unlink succeeded"),
            zx::sys::ZX_ERR_BAD_HANDLE
        );
    }
}

#[fuchsia::test]
async fn unlink_must_be_directory() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_modify_directory {
        return;
    }

    let entries = vec![directory("dir", vec![]), file("file", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    let must_be_directory = fio::UnlinkOptions {
        flags: Some(fio::UnlinkFlags::MUST_BE_DIRECTORY),
        ..Default::default()
    };
    dir.unlink("dir", &must_be_directory)
        .await
        .expect("unlink fidl failed")
        .expect("unlink dir failed");
    assert_eq!(
        dir.unlink("file", &must_be_directory)
            .await
            .expect("unlink fidl failed")
            .expect_err("unlink file succeeded"),
        zx::sys::ZX_ERR_NOT_DIR
    );
}
