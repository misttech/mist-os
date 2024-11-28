// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn rename_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory || !harness.config.supports_get_token {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![
            directory("src", vec![file("old.txt", contents.to_vec())]),
            directory("dest", vec![]),
        ];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;
        let dest_dir = open_rw_dir(&dir, "dest").await;
        let dest_token = get_token(&dest_dir).await;

        // Rename src/old.txt -> dest/new.txt.
        let status = src_dir
            .rename("old.txt", zx::Event::from(dest_token), "new.txt")
            .await
            .expect("rename failed");
        assert!(status.is_ok());

        // Check dest/new.txt was created and has correct contents.
        assert_eq!(read_file(&dir, "dest/new.txt").await, contents);

        // Check src/old.txt no longer exists.
        assert_file_not_found(&dir, "src/old.txt").await;
    }
}

#[fuchsia::test]
async fn rename_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory || !harness.config.supports_get_token {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in harness.file_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let entries = vec![
            directory("src", vec![file("old.txt", contents.to_vec())]),
            directory("dest", vec![]),
        ];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;
        let dest_dir = open_rw_dir(&dir, "dest").await;
        let dest_token = get_token(&dest_dir).await;

        // Try renaming src/old.txt -> dest/new.txt.
        let status = src_dir
            .rename("old.txt", zx::Event::from(dest_token), "new.txt")
            .await
            .expect("rename failed");
        assert!(status.is_err());
        assert_eq!(status.err().unwrap(), zx::Status::BAD_HANDLE.into_raw());
    }
}

#[fuchsia::test]
async fn rename_with_slash_in_path_fails() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory || !harness.config.supports_get_token {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![
            directory("src", vec![file("old.txt", contents.to_vec())]),
            directory("dest", vec![]),
        ];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;
        let dest_dir = open_rw_dir(&dir, "dest").await;

        // Including a slash in the src or dest path should fail.
        let status = dir
            .rename("src/old.txt", zx::Event::from(get_token(&dest_dir).await), "new.txt")
            .await
            .expect("rename failed");
        assert!(status.is_err());
        assert_eq!(status.err().unwrap(), zx::Status::INVALID_ARGS.into_raw());
        let status = src_dir
            .rename("old.txt", zx::Event::from(get_token(&dest_dir).await), "nested/new.txt")
            .await
            .expect("rename failed");
        assert!(status.is_err());
        assert_eq!(status.err().unwrap(), zx::Status::INVALID_ARGS.into_raw());
    }
}
