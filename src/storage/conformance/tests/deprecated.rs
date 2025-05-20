// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn deprecated_open_dir_without_describe_flag() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        assert_eq!(dir_flags & fio::OpenFlags::DESCRIBE, fio::OpenFlags::empty());
        let (client, server) = create_proxy::<fio::NodeMarker>();

        dir.deprecated_open(
            dir_flags | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            ".",
            server,
        )
        .expect("Cannot open directory");

        assert_on_open_not_received(&client).await;
    }
}

#[fuchsia::test]
async fn deprecated_open_file_without_describe_flag() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.combinations_deprecated() {
        assert_eq!(file_flags & fio::OpenFlags::DESCRIBE, fio::OpenFlags::empty());
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let (client, server) = create_proxy::<fio::NodeMarker>();

        dir.deprecated_open(
            file_flags | fio::OpenFlags::NOT_DIRECTORY,
            fio::ModeType::empty(),
            TEST_FILE,
            server,
        )
        .expect("Cannot open file");

        assert_on_open_not_received(&client).await;
    }
}

#[fuchsia::test]
async fn deprecated_open_epitaph_no_describe() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());

    let (client, server) = create_proxy::<fio::NodeMarker>();
    dir.deprecated_open(
        fio::OpenFlags::RIGHT_READABLE,
        fio::ModeType::empty(),
        "does_not_exist",
        server,
    )
    .expect("Open should not fail!");
    // Since Open1 is asynchronous, we need to invoke another method on the channel to check that
    // opening failed.
    let e = client
        .get_attributes(Default::default())
        .await
        .expect_err("GetAttributes should fail on a nonexistent file.");
    assert_matches!(e, fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FOUND, .. });
}

/// Checks that open fails with ZX_ERR_BAD_PATH when it should.
#[fuchsia::test]
async fn deprecated_open_path() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

    // Valid paths:
    for path in [".", "/", "/dir/"] {
        deprecated_open_node::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_READABLE, path).await;
    }

    // Invalid paths:
    for path in [
        "", "//", "///", "////", "./", "/dir//", "//dir//", "/dir//", "/dir/../", "/dir/..",
        "/dir/./", "/dir/.", "/./", "./dir",
    ] {
        assert_eq!(
            deprecated_open_node_status::<fio::NodeMarker>(
                &dir,
                fio::OpenFlags::RIGHT_READABLE,
                path
            )
            .await
            .expect_err("open succeeded"),
            zx::Status::INVALID_ARGS,
            "path: {}",
            path,
        );
    }
}

/// Check that a trailing flash with OPEN_FLAG_NOT_DIRECTORY returns ZX_ERR_INVALID_ARGS.
#[fuchsia::test]
async fn deprecated_open_trailing_slash_with_not_directory() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());
    assert_eq!(
        deprecated_open_node_status::<fio::NodeMarker>(
            &dir,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "foo/"
        )
        .await
        .expect_err("open succeeded"),
        zx::Status::INVALID_ARGS
    );
}
/// Creates a directory with all rights, and checks it can be opened for all subsets of rights.
#[fuchsia::test]
async fn deprecated_open_dir_with_sufficient_rights() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let (client, server) = create_proxy::<fio::NodeMarker>();
        dir.deprecated_open(
            dir_flags | fio::OpenFlags::DESCRIBE | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            ".",
            server,
        )
        .expect("Cannot open directory");

        assert_eq!(get_open_status(&client).await, zx::Status::OK);
    }
}

/// Creates a directory with no rights, and checks opening it with any rights fails.
#[fuchsia::test]
async fn deprecated_open_dir_with_insufficient_rights() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(vec![], fio::Flags::empty());

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        if dir_flags.is_empty() {
            continue;
        }
        let (client, server) = create_proxy::<fio::NodeMarker>();
        dir.deprecated_open(
            dir_flags | fio::OpenFlags::DESCRIBE | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            ".",
            server,
        )
        .expect("Cannot open directory");

        assert_eq!(get_open_status(&client).await, zx::Status::ACCESS_DENIED);
    }
}

/// Opens a directory, and checks that a child directory can be opened using the same rights.
#[fuchsia::test]
async fn deprecated_open_child_dir_with_same_rights() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("child", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

        let parent_dir = deprecated_open_node::<fio::DirectoryMarker>(
            &dir,
            dir_flags | fio::OpenFlags::DIRECTORY,
            ".",
        )
        .await;

        // Open child directory with same flags as parent.
        let (child_dir_client, child_dir_server) = create_proxy::<fio::NodeMarker>();
        parent_dir
            .deprecated_open(
                dir_flags | fio::OpenFlags::DESCRIBE | fio::OpenFlags::DIRECTORY,
                fio::ModeType::empty(),
                "child",
                child_dir_server,
            )
            .expect("Cannot open directory");

        assert_eq!(get_open_status(&child_dir_client).await, zx::Status::OK);
    }
}

/// Opens a directory as readable, and checks that a child directory cannot be opened as writable.
#[fuchsia::test]
async fn deprecated_open_child_dir_with_extra_rights() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("child", vec![])];
    let dir = harness.get_directory(entries, fio::PERM_READABLE);

    // Open parent as readable.
    let parent_dir = deprecated_open_node::<fio::DirectoryMarker>(
        &dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
        ".",
    )
    .await;

    // Opening child as writable should fail.
    let (child_dir_client, child_dir_server) = create_proxy::<fio::NodeMarker>();
    parent_dir
        .deprecated_open(
            fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DESCRIBE | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            "child",
            child_dir_server,
        )
        .expect("Cannot open directory");

    assert_eq!(get_open_status(&child_dir_client).await, zx::Status::ACCESS_DENIED);
}

/// Creates a child directory and opens it with OPEN_FLAG_POSIX_WRITABLE/EXECUTABLE, ensuring that
/// the requested rights are expanded to only those which the parent directory connection has.
#[fuchsia::test]
async fn deprecated_open_child_dir_with_posix_flags() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("child", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let readable = dir_flags & fio::OpenFlags::RIGHT_READABLE;
        let parent_dir = deprecated_open_node::<fio::DirectoryMarker>(
            &dir,
            dir_flags | fio::OpenFlags::DIRECTORY,
            ".",
        )
        .await;

        let (child_dir_client, child_dir_server) = create_proxy::<fio::NodeMarker>();
        parent_dir
            .deprecated_open(
                readable
                    | fio::OpenFlags::POSIX_WRITABLE
                    | fio::OpenFlags::POSIX_EXECUTABLE
                    | fio::OpenFlags::DESCRIBE
                    | fio::OpenFlags::DIRECTORY,
                fio::ModeType::empty(),
                "child",
                child_dir_server,
            )
            .expect("Cannot open directory");

        assert_eq!(
            get_open_status(&child_dir_client).await,
            zx::Status::OK,
            "Failed to open directory, flags = {:?}",
            dir_flags
        );
        // Ensure expanded rights do not exceed those of the parent directory connection.
        let (status, flags) =
            child_dir_client.deprecated_get_flags().await.expect("Failed to get node flags!");
        assert_matches!(zx::Status::ok(status), Ok(()));
        assert_eq!(flags & dir_flags, dir_flags);
    }
}

/// Ensures that opening a file with more rights than the directory connection fails
/// with Status::ACCESS_DENIED.
#[fuchsia::test]
async fn deprecated_open_file_with_extra_rights() {
    let harness = TestHarness::new().await;

    // Combinations to test of the form (directory flags, [file flag combinations]).
    // All file flags should have more rights than those of the directory flags.
    let test_right_combinations = [
        (fio::OpenFlags::empty(), harness.file_rights.combinations_deprecated()),
        (
            fio::OpenFlags::RIGHT_READABLE,
            harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES),
        ),
        (
            fio::OpenFlags::RIGHT_WRITABLE,
            harness.file_rights.combinations_containing_deprecated(fio::Rights::READ_BYTES),
        ),
    ];

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

    for (dir_flags, file_flag_combos) in test_right_combinations.iter() {
        let dir_proxy = deprecated_open_node::<fio::DirectoryMarker>(
            &dir,
            *dir_flags | fio::OpenFlags::DIRECTORY,
            ".",
        )
        .await;

        for file_flags in file_flag_combos {
            if file_flags.is_empty() {
                continue; // The rights in file_flags must *exceed* those in dir_flags.
            }
            // Ensure the combination is valid (e.g. that file_flags is requesting more rights
            // than those in dir_flags).
            assert!(
                (*file_flags & harness.dir_rights.all_flags_deprecated())
                    != (*dir_flags & harness.dir_rights.all_flags_deprecated()),
                "Invalid test: file rights must exceed dir! (flags: dir = {:?}, file = {:?})",
                *dir_flags,
                *file_flags
            );

            let (client, server) = create_proxy::<fio::NodeMarker>();

            dir_proxy
                .deprecated_open(
                    *file_flags | fio::OpenFlags::DESCRIBE | fio::OpenFlags::NOT_DIRECTORY,
                    fio::ModeType::empty(),
                    TEST_FILE,
                    server,
                )
                .expect("Cannot open file");

            assert_eq!(
                get_open_status(&client).await,
                zx::Status::ACCESS_DENIED,
                "Opened a file with more rights than the directory! (flags: dir = {:?}, file = {:?})",
                *dir_flags,
                *file_flags
            );
        }
    }
}
