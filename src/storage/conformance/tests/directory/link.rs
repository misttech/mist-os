// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use io_conformance_util::flags::Rights;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn link_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory || !harness.config.supports_get_token {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in harness
        .dir_rights
        .combinations_containing_deprecated(fio::Rights::WRITE_BYTES | fio::Rights::READ_BYTES)
    {
        let entries = vec![
            directory("src", vec![file("old.txt", contents.to_vec())]),
            directory("dest", vec![]),
        ];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;
        let dest_dir = open_rw_dir(&dir, "dest").await;
        let dest_token = get_token(&dest_dir).await;

        // Link src/old.txt -> dest/new.txt.
        let status = src_dir.link("old.txt", dest_token, "new.txt").await.expect("link failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK, "dir_flags={dir_flags:?}");

        // Check dest/new.txt was created and has correct contents.
        assert_eq!(read_file(&dir, "dest/new.txt").await, contents);

        // Check src/old.txt still exists.
        assert_eq!(read_file(&dir, "src/old.txt").await, contents);
    }
}

#[fuchsia::test]
async fn link_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory || !harness.config.supports_get_token {
        return;
    }
    let contents = "abcdef".as_bytes();

    for dir_flags in harness.dir_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let entries = vec![
            directory("src", vec![file("old.txt", contents.to_vec())]),
            directory("dest", vec![]),
        ];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let src_dir = open_dir_with_flags(&dir, dir_flags, "src").await;
        let dest_dir = open_rw_dir(&dir, "dest").await;
        let dest_token = get_token(&dest_dir).await;

        // Link src/old.txt -> dest/new.txt.
        let status = src_dir.link("old.txt", dest_token, "new.txt").await.expect("link failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::BAD_HANDLE);

        // Check dest/new.txt was not created.
        assert_file_not_found(&dir, "dest/new.txt").await;

        // Check src/old.txt still exists.
        assert_eq!(read_file(&dir, "src/old.txt").await, contents);
    }
}

// This tests link with all io2 flags.
#[fuchsia::test]
async fn io2_link() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_modify_directory || !harness.config.supports_get_token {
        return;
    }

    let entries = vec![file(TEST_FILE, b"abcdef".to_vec())];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

    let all_rights = Rights::new(fio::RW_STAR_DIR);

    for rights in all_rights.combinations() {
        let dir =
            dir.open3_node::<fio::DirectoryMarker>(".", rights, None).await.expect("open3 failed");

        let (status, token) = dir.get_token().await.unwrap();
        let status = zx::Status::ok(status);
        if rights.contains(fio::Flags::PERM_MODIFY) {
            status.expect("get_token failed");

            let status = zx::Status::ok(
                dir.link(TEST_FILE, token.unwrap(), &format!("new {rights:?}")).await.unwrap(),
            );

            if rights == all_rights.all_flags() {
                // link should only succeed with *all* of the fio::RW_STAR_DIR rights since
                // otherwise rights escalation is possible.  We do not check for EXECUTE because
                // mutable filesystems that support link don't currently support EXECUTE rights.
                status.expect("link failed");
            } else {
                assert_eq!(status, Err(zx::Status::BAD_HANDLE), "rights={rights:?}");
            }
        } else {
            assert_eq!(status, Err(zx::Status::BAD_HANDLE), "rights={rights:?}");
        }
    }
}
