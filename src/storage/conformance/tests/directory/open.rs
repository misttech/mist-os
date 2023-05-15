// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::stream::TryStreamExt as _,
    io_conformance_util::{test_harness::TestHarness, *},
};

#[fuchsia::test]
async fn open_dir_without_describe_flag() {
    let harness = TestHarness::new().await;
    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    for dir_flags in harness.dir_rights.valid_combos() {
        assert_eq!(dir_flags & fio::OpenFlags::DESCRIBE, fio::OpenFlags::empty());
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

        root_dir
            .open(dir_flags | fio::OpenFlags::DIRECTORY, fio::ModeType::empty(), ".", server)
            .expect("Cannot open directory");

        assert_on_open_not_received(&client).await;
    }
}

#[fuchsia::test]
async fn open_file_without_describe_flag() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.valid_combos() {
        assert_eq!(file_flags & fio::OpenFlags::DESCRIBE, fio::OpenFlags::empty());
        let root = root_directory(vec![file(TEST_FILE, vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");

        test_dir
            .open(
                file_flags | fio::OpenFlags::NOT_DIRECTORY,
                fio::ModeType::empty(),
                TEST_FILE,
                server,
            )
            .expect("Cannot open file");

        assert_on_open_not_received(&client).await;
    }
}

/// Checks that open fails with ZX_ERR_BAD_PATH when it should.
#[fuchsia::test]
async fn open_path() {
    let harness = TestHarness::new().await;
    if !harness.config.conformant_path_handling.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    // Valid paths:
    for path in [".", "/", "/dir/"] {
        open_node::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_READABLE, path).await;
    }

    // Invalid paths:
    for path in [
        "", "//", "///", "////", "./", "/dir//", "//dir//", "/dir//", "/dir/../", "/dir/..",
        "/dir/./", "/dir/.", "/./", "./dir",
    ] {
        assert_eq!(
            open_node_status::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_READABLE, path)
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
    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());
    assert_eq!(
        open_node_status::<fio::NodeMarker>(
            &root_dir,
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
    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let _root_dir = harness.get_directory(
        root,
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
    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    // Opening as READABLE must succeed.
    open_node::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    if harness.config.mutable_file.unwrap_or_default() {
        // Opening as WRITABLE must succeed.
        open_node::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;
        // Opening as EXECUTABLE must fail (W^X).
        open_node_status::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_EXECUTABLE, TEST_FILE)
            .await
            .expect_err("open succeeded");
    } else {
        // If files are immutable, check that opening as WRITABLE results in access denied.
        // All other combinations are valid in this case.
        assert_eq!(
            open_node_status::<fio::NodeMarker>(
                &root_dir,
                fio::OpenFlags::RIGHT_WRITABLE,
                TEST_FILE
            )
            .await
            .expect_err("open succeeded"),
            zx::Status::ACCESS_DENIED
        );
    }
}

// Validate allowed rights for VmoFile objects (ensures cannot be opened as executable).
#[fuchsia::test]
async fn validate_vmo_file_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_vmo_file.unwrap_or_default() {
        return;
    }
    // Create a test directory with a VmoFile object, and ensure the directory has all rights.
    let root = root_directory(vec![vmo_file(TEST_FILE, TEST_FILE_CONTENTS, 128 * 1024)]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());
    // Opening with READ/WRITE should succeed.
    open_node::<fio::NodeMarker>(
        &root_dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        TEST_FILE,
    )
    .await;
    // Opening with EXECUTE must fail to ensure W^X enforcement.
    assert!(matches!(
        open_node_status::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_EXECUTABLE, TEST_FILE)
            .await
            .expect_err("open succeeded"),
        zx::Status::ACCESS_DENIED | zx::Status::NOT_SUPPORTED
    ));
}

// Validate allowed rights for ExecutableFile objects (ensures cannot be opened as writable).
#[fuchsia::test]
async fn validate_executable_file_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_executable_file.unwrap_or_default() {
        return;
    }
    // Create a test directory with an ExecutableFile object, and ensure the directory has all rights.
    let root = root_directory(vec![executable_file(TEST_FILE)]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());
    // Opening with READABLE/EXECUTABLE should succeed.
    open_node::<fio::NodeMarker>(
        &root_dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        TEST_FILE,
    )
    .await;
    // Opening with WRITABLE must fail to ensure W^X enforcement.
    assert_eq!(
        open_node_status::<fio::NodeMarker>(&root_dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE)
            .await
            .expect_err("open succeeded"),
        zx::Status::ACCESS_DENIED
    );
}

/// Creates a directory with all rights, and checks it can be opened for all subsets of rights.
#[fuchsia::test]
async fn open_dir_with_sufficient_rights() {
    let harness = TestHarness::new().await;

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    for dir_flags in harness.dir_rights.valid_combos() {
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        root_dir
            .open(
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

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, fio::OpenFlags::empty());

    for dir_flags in harness.dir_rights.valid_combos() {
        if dir_flags.is_empty() {
            continue;
        }
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        root_dir
            .open(
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

    for dir_flags in harness.dir_rights.valid_combos() {
        let root = root_directory(vec![directory("child", vec![])]);
        let root_dir = harness.get_directory(root, harness.dir_rights.all());

        let parent_dir = open_node::<fio::DirectoryMarker>(
            &root_dir,
            dir_flags | fio::OpenFlags::DIRECTORY,
            ".",
        )
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

    let root = root_directory(vec![directory("child", vec![])]);
    let root_dir = harness.get_directory(root, fio::OpenFlags::RIGHT_READABLE);

    // Open parent as readable.
    let parent_dir = open_node::<fio::DirectoryMarker>(
        &root_dir,
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

    for dir_flags in harness.dir_rights.valid_combos() {
        let root = root_directory(vec![directory("child", vec![])]);
        let root_dir = harness.get_directory(root, dir_flags);
        let readable = dir_flags & fio::OpenFlags::RIGHT_READABLE;
        let parent_dir = open_node::<fio::DirectoryMarker>(
            &root_dir,
            dir_flags | fio::OpenFlags::DIRECTORY,
            ".",
        )
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
        assert_eq!(get_node_flags(&child_dir_client).await & dir_flags, dir_flags);
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
        (fio::OpenFlags::empty(), harness.file_rights.valid_combos()),
        (
            fio::OpenFlags::RIGHT_READABLE,
            harness.file_rights.valid_combos_with(fio::OpenFlags::RIGHT_WRITABLE),
        ),
        (
            fio::OpenFlags::RIGHT_WRITABLE,
            harness.file_rights.valid_combos_with(fio::OpenFlags::RIGHT_READABLE),
        ),
    ];

    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    for (dir_flags, file_flag_combos) in test_right_combinations.iter() {
        let dir_proxy = open_node::<fio::DirectoryMarker>(
            &root_dir,
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
                (*file_flags & harness.dir_rights.all()) != (*dir_flags & harness.dir_rights.all()),
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
async fn open2_directory_unsupported() {
    let harness = TestHarness::new().await;

    if harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    // fuchsia.io/Directory.Open2
    let (open2_proxy, open2_server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    dir_proxy
        .open2(
            ".",
            &fio::ConnectionProtocols::Node(fio::NodeOptions::default()),
            open2_server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        open2_proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
    );
}

#[fuchsia::test]
async fn open2_rights() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    const CONTENT: &[u8] = b"content";
    let test_dir = harness.get_directory(
        root_directory(vec![file(TEST_FILE, CONTENT.to_vec())]),
        fio::OpenFlags::RIGHT_READABLE,
    );

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            &TEST_FILE,
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::WRITE_BYTES),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::ACCESS_DENIED, .. })
    );

    // Check that empty rights get copied from the parent.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
    test_dir
        .open2(
            &TEST_FILE,
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    assert_eq!(
        proxy.get_connection_info().await.expect("get_connection_info failed").rights,
        test_dir.get_connection_info().await.expect("get_connection_info failed").rights
    );

    // We should be able to read from the file, but not write.
    assert_eq!(&fuchsia_fs::file::read(&proxy).await.expect("read failed"), CONTENT);
    assert_matches!(
        fuchsia_fs::file::write(&proxy, "data").await,
        Err(fuchsia_fs::file::WriteError::WriteError(zx::Status::BAD_HANDLE))
    );
}

#[fuchsia::test]
async fn open2_invalid() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // It's an error to specify more than one protocol when trying to create an object.
    for mode in [fio::OpenMode::MaybeCreate, fio::OpenMode::AlwaysCreate] {
        let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        test_dir
            .open2(
                "file",
                &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                    protocols: Some(fio::NodeProtocols {
                        file: Some(fio::FileProtocolFlags::default()),
                        directory: Some(fio::DirectoryProtocolOptions::default()),
                        ..Default::default()
                    }),
                    mode: Some(mode),
                    ..Default::default()
                }),
                server.into_channel(),
            )
            .unwrap();
        assert_matches!(
            proxy.take_event_stream().try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::INVALID_ARGS, .. })
        );
    }

    // It's an error to specify create attributes when opening an object.
    for mode in [None, Some(fio::OpenMode::OpenExisting)] {
        let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        test_dir
            .open2(
                "file",
                &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                    protocols: Some(fio::NodeProtocols {
                        file: Some(fio::FileProtocolFlags::default()),
                        ..Default::default()
                    }),
                    mode,
                    create_attributes: Some(fio::MutableNodeAttributes {
                        creation_time: Some(1),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                server.into_channel(),
            )
            .unwrap();
        assert_matches!(
            proxy.take_event_stream().try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::INVALID_ARGS, .. })
        );
    }
}

#[fuchsia::test]
async fn open2_create_dot_fails_with_already_exists() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            ".",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    directory: Some(fio::DirectoryProtocolOptions::default()),
                    ..Default::default()
                }),
                mode: Some(fio::OpenMode::AlwaysCreate),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::ALREADY_EXISTS, .. })
    );
}

#[fuchsia::test]
async fn open2_open_directory() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness.get_directory(
        root_directory(vec![directory("dir", vec![])]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Should be able to open the directory specifying just the directory protocol.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    test_dir
        .open2(
            "dir",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    directory: Some(fio::DirectoryProtocolOptions::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    // Check it's a directory...
    let (proxy2, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    proxy
        .open2(
            ".",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions::default()),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(proxy2.get_connection_info().await, Ok(_));

    // Any node protocol should work.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    test_dir
        .open2(
            "dir",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions::default()),
            server.into_channel(),
        )
        .unwrap();

    // Check it's a directory...
    let (proxy2, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    proxy
        .open2(
            ".",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions::default()),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(proxy2.get_connection_info().await, Ok(_));

    // Attempting to open the directory as a file should fail.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "dir",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FILE, .. })
    );

    // And as a symbolic link...
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "dir",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    symlink: Some(fio::SymlinkProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::WRONG_TYPE, .. })
    );
}

#[fuchsia::test]
async fn open2_open_file() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    const CONTENT: &[u8] = b"content";
    let test_dir = harness.get_directory(
        root_directory(vec![file("file", CONTENT.to_vec())]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Should be able to open the file specifying just the file protocol.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    // Check it's a file...
    assert_eq!(fuchsia_fs::file::read(&proxy).await.expect("read failed"), CONTENT);

    // Any node protocol should work.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions::default()),
            server.into_channel(),
        )
        .unwrap();

    // Check it's a file...
    assert_eq!(fuchsia_fs::file::read(&proxy).await.expect("read failed"), CONTENT);

    // Attempting to open the file as a directory should fail.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    directory: Some(fio::DirectoryProtocolOptions::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_DIR, .. })
    );

    // And as a symbolic link...
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    symlink: Some(fio::SymlinkProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();
    assert_matches!(
        proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::WRONG_TYPE, .. })
    );
}

#[fuchsia::test]
async fn open2_file_append() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness.get_directory(
        root_directory(vec![file("file", b"foo".to_vec())]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::APPEND),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    // Append to the file.
    assert_matches!(fuchsia_fs::file::write(&proxy, " bar").await, Ok(()));

    // Read back to check.
    proxy.seek(fio::SeekOrigin::Start, 0).await.expect("seek FIDL failed").expect("seek failed");
    assert_eq!(fuchsia_fs::file::read(&proxy).await.expect("read failed"), b"foo bar");
}

#[fuchsia::test]
async fn open2_file_truncate() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness.get_directory(
        root_directory(vec![file("file", b"foo".to_vec())]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::TRUNCATE),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    assert_eq!(fuchsia_fs::file::read(&proxy).await.expect("read failed"), b"");
}

#[fuchsia::test]
async fn open2_directory_get_representation() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness.get_directory(root_directory(vec![]), fio::OpenFlags::RIGHT_READABLE);

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    test_dir
        .open2(
            ".",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                attributes: Some(
                    fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::ABILITIES,
                ),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    assert_matches!(
        proxy
            .take_event_stream()
            .try_next()
            .await
            .expect("expected OnRepresentation event")
            .expect("missing OnRepresentation event")
            .into_on_representation(),
        Some(fio::Representation::Directory(fio::DirectoryInfo {
            attributes: Some(fio::NodeAttributes2 { mutable_attributes, immutable_attributes }),
            ..
        }))
        if mutable_attributes == fio::MutableNodeAttributes::default()
            && immutable_attributes
                == fio::ImmutableNodeAttributes {
                    protocols: Some(fio::NodeProtocolKinds::DIRECTORY),
                    abilities: Some(
                        fio::Operations::GET_ATTRIBUTES
                            | fio::Operations::UPDATE_ATTRIBUTES
                            | fio::Operations::ENUMERATE
                            | fio::Operations::TRAVERSE
                            | fio::Operations::MODIFY_DIRECTORY
                    ),
                    ..Default::default()
                }
    );
}

#[fuchsia::test]
async fn open2_file_get_representation() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let test_dir = harness
        .get_directory(root_directory(vec![file("file", vec![])]), fio::OpenFlags::RIGHT_READABLE);

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &mut fio::ConnectionProtocols::Node(fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::APPEND),
                    ..Default::default()
                }),
                attributes: Some(
                    fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::ABILITIES,
                ),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    assert_matches!(
        proxy
            .take_event_stream()
            .try_next()
            .await
            .expect("expected OnRepresentation event")
            .expect("missing OnRepresentation event")
            .into_on_representation(),
        Some(fio::Representation::File(fio::FileInfo {
            is_append: Some(true),
            attributes: Some(fio::NodeAttributes2 { mutable_attributes, immutable_attributes }),
            ..
        }))
        if mutable_attributes == fio::MutableNodeAttributes::default()
            && immutable_attributes
                == fio::ImmutableNodeAttributes {
                    protocols: Some(fio::NodeProtocolKinds::FILE),
                    abilities: Some(
                        fio::Operations::GET_ATTRIBUTES
                            | fio::Operations::UPDATE_ATTRIBUTES
                            | fio::Operations::READ_BYTES
                            | fio::Operations::WRITE_BYTES
                            | if harness.config.supports_executable_file.unwrap_or_default() {
                                fio::Operations::EXECUTE
                            } else {
                                fio::Operations::empty()
                            },
                    ),
                    ..Default::default()
                }
    );
}

// TODO(fxbug.dev/123390): Add open2 symlink tests.
// TODO(fxbug.dev/77623): Add open2 connect tests.
