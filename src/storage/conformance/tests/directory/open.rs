// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn open_dir_without_describe_flag() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags_deprecated());

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        assert_eq!(dir_flags & fio::OpenFlags::DESCRIBE, fio::OpenFlags::empty());
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

        dir.open(dir_flags | fio::OpenFlags::DIRECTORY, fio::ModeType::empty(), ".", server)
            .expect("Cannot open directory");

        assert_on_open_not_received(&client).await;
    }
}

#[fuchsia::test]
async fn open_file_without_describe_flag() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.combinations_deprecated() {
        assert_eq!(file_flags & fio::OpenFlags::DESCRIBE, fio::OpenFlags::empty());
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

        dir.open(
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
async fn open1_epitaph_no_describe() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags_deprecated());

    let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
    dir.open(fio::OpenFlags::RIGHT_READABLE, fio::ModeType::empty(), "does_not_exist", server)
        .expect("Open should not fail!");
    // Since Open1 is asynchronous, we need to invoke another method on the channel to check that
    // opening failed.
    let e = client
        .get_connection_info()
        .await
        .expect_err("GetConnectionInfo should fail on a nonexistent file.");
    assert_matches!(e, fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FOUND, .. });
}

/// Checks that open fails with ZX_ERR_BAD_PATH when it should.
#[fuchsia::test]
async fn open_path() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    // Valid paths:
    for path in [".", "/", "/dir/"] {
        open_node::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_READABLE, path).await;
    }

    // Invalid paths:
    for path in [
        "", "//", "///", "////", "./", "/dir//", "//dir//", "/dir//", "/dir/../", "/dir/..",
        "/dir/./", "/dir/.", "/./", "./dir",
    ] {
        assert_eq!(
            open_node_status::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_READABLE, path)
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
async fn open_trailing_slash_with_not_directory() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags_deprecated());
    assert_eq!(
        open_node_status::<fio::NodeMarker>(
            &dir,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "foo/"
        )
        .await
        .expect_err("open succeeded"),
        zx::Status::INVALID_ARGS
    );
}

// Validate allowed rights for Directory objects.
#[fuchsia::test]
async fn validate_directory_rights() {
    let harness = TestHarness::new().await;
    // Create a test directory and ensure we can open it with all supported rights.
    let entries = vec![file(TEST_FILE, vec![])];
    let _dir = harness.get_directory(
        entries,
        fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_EXECUTABLE,
    );
}

// Validate allowed rights for File objects (ensures writable files cannot be opened as executable).
#[fuchsia::test]
async fn validate_file_rights() {
    let harness = TestHarness::new().await;
    // Create a test directory with a single File object, and ensure the directory has all rights.
    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    // Opening as READABLE must succeed.
    open_node::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    if harness.config.supports_mutable_file {
        // Opening as WRITABLE must succeed.
        open_node::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;
    } else {
        // Opening as WRITABLE must fail.
        assert_eq!(
            open_node_status::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE)
                .await
                .expect_err("Opening as writable should fail"),
            zx::Status::ACCESS_DENIED
        );
    }
    // An executable file wasn't created, opening as EXECUTABLE must fail.
    open_node_status::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_EXECUTABLE, TEST_FILE)
        .await
        .expect_err("open succeeded");
}

// Validate allowed rights for ExecutableFile objects (ensures cannot be opened as writable).
#[fuchsia::test]
async fn validate_executable_file_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_executable_file {
        return;
    }
    // Create a test directory with an ExecutableFile object, and ensure the directory has all rights.
    let entries = vec![executable_file(TEST_FILE)];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
    // Opening with READABLE/EXECUTABLE should succeed.
    open_node::<fio::NodeMarker>(
        &dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        TEST_FILE,
    )
    .await;
    // Opening with WRITABLE must fail to ensure W^X enforcement.
    assert_eq!(
        open_node_status::<fio::NodeMarker>(&dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE)
            .await
            .expect_err("open succeeded"),
        zx::Status::ACCESS_DENIED
    );
}

/// Creates a directory with all rights, and checks it can be opened for all subsets of rights.
#[fuchsia::test]
async fn open_dir_with_sufficient_rights() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags_deprecated());

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        dir.open(
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
async fn open_dir_with_insufficient_rights() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(vec![], fio::OpenFlags::empty());

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        if dir_flags.is_empty() {
            continue;
        }
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        dir.open(
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
async fn open_child_dir_with_same_rights() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("child", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

        let parent_dir =
            open_node::<fio::DirectoryMarker>(&dir, dir_flags | fio::OpenFlags::DIRECTORY, ".")
                .await;

        // Open child directory with same flags as parent.
        let (child_dir_client, child_dir_server) =
            create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        parent_dir
            .open(
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
async fn open_child_dir_with_extra_rights() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("child", vec![])];
    let dir = harness.get_directory(entries, fio::OpenFlags::RIGHT_READABLE);

    // Open parent as readable.
    let parent_dir = open_node::<fio::DirectoryMarker>(
        &dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
        ".",
    )
    .await;

    // Opening child as writable should fail.
    let (child_dir_client, child_dir_server) =
        create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
    parent_dir
        .open(
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
async fn open_child_dir_with_posix_flags() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("child", vec![])];
        let dir = harness.get_directory(entries, dir_flags);
        let readable = dir_flags & fio::OpenFlags::RIGHT_READABLE;
        let parent_dir =
            open_node::<fio::DirectoryMarker>(&dir, dir_flags | fio::OpenFlags::DIRECTORY, ".")
                .await;

        let (child_dir_client, child_dir_server) =
            create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        parent_dir
            .open(
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
            child_dir_client.get_flags().await.expect("Failed to get node flags!");
        assert_matches!(zx::Status::ok(status), Ok(()));
        assert_eq!(flags & dir_flags, dir_flags);
    }
}

/// Ensures that opening a file with more rights than the directory connection fails
/// with Status::ACCESS_DENIED.
#[fuchsia::test]
async fn open_file_with_extra_rights() {
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
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    for (dir_flags, file_flag_combos) in test_right_combinations.iter() {
        let dir_proxy =
            open_node::<fio::DirectoryMarker>(&dir, *dir_flags | fio::OpenFlags::DIRECTORY, ".")
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

            let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

            dir_proxy
                .open(
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

#[fuchsia::test]
async fn open3_rights() {
    let harness = TestHarness::new().await;

    const CONTENT: &[u8] = b"content";
    let dir = harness
        .get_directory(vec![file(TEST_FILE, CONTENT.to_vec())], fio::OpenFlags::RIGHT_READABLE);
    // Should fail to open the file if the rights exceed those allowed by the directory.
    let status = dir
        .open3_node::<fio::NodeMarker>(&TEST_FILE, fio::Flags::PERM_WRITE, None)
        .await
        .expect_err("open should fail if rights exceed those of the parent connection");
    assert_eq!(status, zx::Status::ACCESS_DENIED);

    // Calling open3 with no rights set is the same as calling open3 with empty rights.
    let proxy = dir
        .open3_node::<fio::FileMarker>(&TEST_FILE, fio::Flags::PROTOCOL_FILE, None)
        .await
        .unwrap();
    assert_eq!(
        proxy.get_connection_info().await.expect("get_connection_info failed").rights,
        Some(fio::Rights::empty())
    );

    // Opening with rights that the connection has should succeed.
    let proxy = dir
        .open3_node::<fio::FileMarker>(
            &TEST_FILE,
            fio::Flags::PROTOCOL_FILE | fio::Flags::PERM_GET_ATTRIBUTES | fio::Flags::PERM_READ,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        proxy.get_connection_info().await.expect("get_connection_info failed").rights,
        Some(fio::Rights::READ_BYTES | fio::Operations::GET_ATTRIBUTES)
    );
    // We should be able to read from the file, but not write.
    assert_eq!(&fuchsia_fs::file::read(&proxy).await.expect("read failed"), CONTENT);
    assert_matches!(
        fuchsia_fs::file::write(&proxy, "data").await,
        Err(fuchsia_fs::file::WriteError::WriteError(zx::Status::BAD_HANDLE))
    );
}

#[fuchsia::test]
async fn open3_invalid() {
    let harness = TestHarness::new().await;

    let dir = harness
        .get_directory(vec![], fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

    // It's an error to specify more than one protocol when trying to create an object.
    for create_flag in [
        fio::Flags::FLAG_MAYBE_CREATE,
        fio::Flags::FLAG_MUST_CREATE,
        // FLAG_MUST_CREATE takes precedence over FLAG_MAYBE_CREATE.
        fio::Flags::FLAG_MAYBE_CREATE | fio::Flags::FLAG_MUST_CREATE,
    ] {
        let status = dir
            .open3_node::<fio::NodeMarker>(
                "file",
                fio::Flags::PROTOCOL_FILE | fio::Flags::PROTOCOL_DIRECTORY | create_flag,
                None,
            )
            .await
            .expect_err("open should fail if multiple protocols are set with FLAG_*_CREATE");
        assert_eq!(status, zx::Status::INVALID_ARGS);
    }
}

#[fuchsia::test]
async fn open3_create_dot_fails_with_already_exists() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_modify_directory {
        return;
    }

    let dir = harness
        .get_directory(vec![], fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

    let status = dir
        .open3_node::<fio::DirectoryMarker>(
            ".",
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MUST_CREATE,
            None,
        )
        .await
        .expect_err("open should fail when trying to create the dot path");
    assert_eq!(status, zx::Status::ALREADY_EXISTS);
}

#[fuchsia::test]
async fn open3_open_directory() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(
        vec![directory("dir", vec![])],
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Should be able to open using the directory protocol.
    let (_, representation) = dir
        .open3_node_repr::<fio::DirectoryMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_DIRECTORY,
            None,
        )
        .await
        .expect("open using directory protocol failed");
    assert_matches!(representation, fio::Representation::Directory(_));

    // Should also be able to open without specifying an exact protocol due to protocol resolution.
    let (_, representation) = dir
        .open3_node_repr::<fio::DirectoryMarker>("dir", fio::Flags::FLAG_SEND_REPRESENTATION, None)
        .await
        .expect("open using node protocol resolution failed");
    assert_matches!(representation, fio::Representation::Directory(_));

    // Should be able to open the file specifying multiple protocols as long as one matches.
    let (_, representation) = dir
        .open3_node_repr::<fio::FileMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_FILE
                | fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::PROTOCOL_SYMLINK,
            None,
        )
        .await
        .expect("failed to open directory with multiple protocols");
    assert_matches!(representation, fio::Representation::Directory(_));

    // Attempting to open the directory as a file should fail with ZX_ERR_NOT_FILE.
    let status = dir
        .open3_node::<fio::NodeMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_FILE,
            None,
        )
        .await
        .expect_err("opening directory as file should fail");
    assert_eq!(status, zx::Status::NOT_FILE);

    // Attempting to open with file protocols should fail with ZX_ERR_INVALID_ARGS. It is worth
    // noting that the behaviour for opening file flags with directory is not clearly defined. Linux
    // allows opening a directory with `O_APPEND` but not `O_TRUNC`.
    let status = dir
        .open3_node::<fio::NodeMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::FILE_APPEND
                | fio::Flags::FILE_TRUNCATE,
            None,
        )
        .await
        .expect_err("opening directory as file should fail");
    assert_eq!(status, zx::Status::INVALID_ARGS);

    // Attempting to open the directory as a symbolic link should fail with ZX_ERR_WRONG_TYPE.
    let status = dir
        .open3_node::<fio::NodeMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_SYMLINK,
            None,
        )
        .await
        .expect_err("opening directory as symlink should fail");
    assert_eq!(status, zx::Status::WRONG_TYPE);
}

#[fuchsia::test]
async fn open3_open_file() {
    let harness = TestHarness::new().await;

    const CONTENT: &[u8] = b"content";
    let dir = harness.get_directory(
        vec![file("file", CONTENT.to_vec())],
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Should be able to open the file specifying just the file protocol.
    let (_, representation) = dir
        .open3_node_repr::<fio::FileMarker>(
            "file",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_FILE,
            None,
        )
        .await
        .expect("failed to open file with file protocol");
    assert_matches!(representation, fio::Representation::File(_));

    // Should also be able to open without specifying an exact protocol due to protocol resolution.
    let (_, representation) = dir
        .open3_node_repr::<fio::FileMarker>("file", fio::Flags::FLAG_SEND_REPRESENTATION, None)
        .await
        .expect("failed to open file with protocol resolution");
    assert_matches!(representation, fio::Representation::File(_));

    // Should be able to open the file specifying multiple protocols as long as one matches.
    let (_, representation) = dir
        .open3_node_repr::<fio::FileMarker>(
            "file",
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_FILE
                | fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::PROTOCOL_SYMLINK,
            None,
        )
        .await
        .expect("failed to open file with multiple protocols");
    assert_matches!(representation, fio::Representation::File(_));

    // Attempting to open the file as a directory should fail with ZX_ERR_NOT_DIR.
    let status = dir
        .open3_node_repr::<fio::NodeMarker>(
            "file",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_DIRECTORY,
            None,
        )
        .await
        .expect_err("should fail to open file as directory");
    assert_eq!(status, zx::Status::NOT_DIR);

    // Attempting to open the file as a symbolic link should fail with ZX_ERR_WRONG_TYPE.
    let status = dir
        .open3_node_repr::<fio::NodeMarker>(
            "file",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_SYMLINK,
            None,
        )
        .await
        .expect_err("should fail to open file as symlink");
    assert_eq!(status, zx::Status::WRONG_TYPE);
}

#[fuchsia::test]
async fn open3_file_append() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_append {
        return;
    }

    let dir = harness.get_directory(
        vec![file("file", b"foo".to_vec())],
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    let proxy = dir
        .open3_node::<fio::FileMarker>(
            "file",
            fio::Flags::PERM_READ | fio::Flags::PERM_WRITE | fio::Flags::FILE_APPEND,
            None,
        )
        .await
        .unwrap();

    // Append to the file.
    assert_matches!(fuchsia_fs::file::write(&proxy, " bar").await, Ok(()));

    // Read back to check.
    proxy.seek(fio::SeekOrigin::Start, 0).await.expect("seek FIDL failed").expect("seek failed");
    assert_eq!(fuchsia_fs::file::read(&proxy).await.expect("read failed"), b"foo bar");
}

#[fuchsia::test]
async fn open3_file_truncate_invalid() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_truncate {
        return;
    }

    let dir =
        harness.get_directory(vec![file("file", b"foo".to_vec())], fio::OpenFlags::RIGHT_READABLE);

    let status = dir
        .open3_node::<fio::FileMarker>("file", fio::Flags::FILE_TRUNCATE, None)
        .await
        .expect_err("open with truncate requires rights to write bytes");
    assert_eq!(status, zx::Status::INVALID_ARGS);
}

#[fuchsia::test]
async fn open3_file_truncate() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_truncate {
        return;
    }

    let dir = harness.get_directory(
        vec![file("file", b"foo".to_vec())],
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    let proxy = dir
        .open3_node::<fio::FileMarker>(
            "file",
            fio::Flags::PERM_READ | fio::Flags::PERM_WRITE | fio::Flags::FILE_TRUNCATE,
            None,
        )
        .await
        .unwrap();

    assert_eq!(fuchsia_fs::file::read(&proxy).await.expect("read failed"), b"");
}

#[fuchsia::test]
async fn open3_directory_get_representation() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(vec![], fio::OpenFlags::RIGHT_READABLE);

    let (_, representation) = dir
        .open3_node_repr::<fio::DirectoryMarker>(
            ".",
            fio::Flags::FLAG_SEND_REPRESENTATION,
            Some(fio::Options {
                attributes: Some(
                    fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::ABILITIES,
                ),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    assert_matches!(
        representation,
        fio::Representation::Directory(fio::DirectoryInfo {
            attributes: Some(fio::NodeAttributes2 { mutable_attributes, immutable_attributes }),
            ..
        })
        if mutable_attributes == fio::MutableNodeAttributes::default()
            && immutable_attributes
                == fio::ImmutableNodeAttributes {
                    protocols: Some(fio::NodeProtocolKinds::DIRECTORY),
                    abilities: Some(harness.supported_dir_abilities()),
                    ..Default::default()
                }
    );
}

#[fuchsia::test]
async fn open3_file_get_representation() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(vec![file("file", vec![])], fio::OpenFlags::RIGHT_READABLE);

    let (_, representation) = dir
        .open3_node_repr::<fio::FileMarker>(
            "file",
            fio::Flags::FLAG_SEND_REPRESENTATION
                | if harness.config.supports_append {
                    fio::Flags::FILE_APPEND
                } else {
                    fio::Flags::empty()
                },
            Some(fio::Options {
                attributes: Some(
                    fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::ABILITIES,
                ),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    assert_matches!(
        representation,
        fio::Representation::File(fio::FileInfo {
            is_append,
            attributes: Some(fio::NodeAttributes2 { mutable_attributes, immutable_attributes }),
            ..
        })
        if mutable_attributes == fio::MutableNodeAttributes::default()
            && immutable_attributes
                == fio::ImmutableNodeAttributes {
                    protocols: Some(fio::NodeProtocolKinds::FILE),
                    abilities: Some(harness.supported_file_abilities()),
                    ..Default::default()
                }
            && is_append == Some(harness.config.supports_append)
    );
}

#[fuchsia::test]
async fn open3_dir_optional_rights() {
    let harness = TestHarness::new().await;

    let dir = harness
        .get_directory(vec![], fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

    let proxy = dir
        .open3_node::<fio::DirectoryMarker>(
            ".",
            fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::PERM_READ
                | fio::Flags::PERM_INHERIT_WRITE
                | fio::Flags::PERM_INHERIT_EXECUTE,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        proxy.get_connection_info().await.expect("get_connection_info failed").rights.unwrap(),
        fio::Operations::READ_BYTES | fio::INHERITED_WRITE_PERMISSIONS,
    );
}

#[fuchsia::test]
async fn open3_request_attributes_rights_failure() {
    let harness = TestHarness::new().await;

    let dir = harness
        .get_directory(vec![], fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

    // Open with no rights.
    let proxy =
        dir.open3_node::<fio::DirectoryMarker>(".", fio::Flags::empty(), None).await.unwrap();

    // Requesting attributes when re-opening via `proxy` should fail without `PERM_GET_ATTRIBUTES`.
    assert_matches!(
        proxy
            .open3_node::<fio::DirectoryMarker>(
                ".",
                fio::Flags::FLAG_SEND_REPRESENTATION,
                Some(fio::Options {
                    attributes: Some(fio::NodeAttributesQuery::PROTOCOLS),
                    ..Default::default()
                })
            )
            .await,
        Err(zx::Status::ACCESS_DENIED)
    );
}

#[fuchsia::test]
async fn open3_open_existing_directory() {
    let harness = TestHarness::new().await;

    let dir = harness.get_directory(
        vec![directory("dir", vec![])],
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Should not be able to open non-existing directory entry without `FLAG_*_CREATE`.
    let status = dir
        .open3_node::<fio::NodeMarker>(
            "this_path_does_not_exist",
            fio::Flags::PROTOCOL_DIRECTORY,
            None,
        )
        .await
        .expect_err("should fail to open non-existing entry when OpenExisting is set");
    assert_eq!(status, zx::Status::NOT_FOUND);

    dir.open3_node::<fio::NodeMarker>(
        "dir",
        fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MAYBE_CREATE,
        None,
    )
    .await
    .expect("failed to open existing entry");
}

#[fuchsia::test]
async fn open3_directory_as_node_reference() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
    let directory_proxy = dir
        .open3_node::<fio::DirectoryMarker>(
            "dir",
            fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::PROTOCOL_NODE
                | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .expect("open3 failed");

    // We are allowed to call `get_attributes` on a node reference
    directory_proxy
        .get_attributes(fio::NodeAttributesQuery::empty())
        .await
        .unwrap()
        .expect("get_attributes failed");

    // Make sure that the directory protocol *was not* served by calling a directory-only method.
    // It should fail with PEER_CLOSED as this method is unknown.
    let err = directory_proxy
        .read_dirents(1)
        .await
        .expect_err("calling a directory-specific method on a node reference is not be allowed");
    assert!(err.is_closed());
}

#[fuchsia::test]
async fn open3_file_as_node_reference() {
    let harness = TestHarness::new().await;

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
    let file_proxy = dir
        .open3_node::<fio::FileMarker>(
            TEST_FILE,
            fio::Flags::PROTOCOL_FILE | fio::Flags::PROTOCOL_NODE | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .expect("open3 failed");

    // We are allowed to call `get_attributes` on a node reference
    file_proxy
        .get_attributes(fio::NodeAttributesQuery::empty())
        .await
        .unwrap()
        .expect("get_attributes failed");

    // Make sure that the directory protocol *was not* served by calling a file-only method.
    // It should fail with PEER_CLOSED as this method is unknown.
    let err = file_proxy
        .read(0)
        .await
        .expect_err("calling a file-specific method on a node reference is not be allowed");
    assert!(err.is_closed());
}
