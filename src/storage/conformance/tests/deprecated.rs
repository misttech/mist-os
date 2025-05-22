// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::{create_proxy, DiscoverableProtocolMarker as _};
use fidl_fuchsia_io as fio;
use fidl_test_placeholders::EchoMarker;
use io_conformance_util::flags::Rights;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

const TEST_STRING: &'static str = "Hello, world!";

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

/// Opening a service node without any flags should connect the channel to the service.
#[fuchsia::test]
async fn deprecated_open_service() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_services {
        return;
    }
    let svc_dir = harness.open_service_directory().await;
    let (echo_proxy, echo_server) = create_proxy::<EchoMarker>();
    svc_dir
        .deprecated_open(
            fio::OpenFlags::empty(),
            fio::ModeType::empty(),
            EchoMarker::PROTOCOL_NAME,
            echo_server.into_channel().into(),
        )
        .unwrap();
    let echo_response = echo_proxy.echo_string(Some(TEST_STRING)).await.unwrap();
    assert_eq!(echo_response.unwrap(), TEST_STRING);
}

/// Opening a service node with [`fio::OpenFlags::NODE_REFERENCE`] should open the underlying node.
#[fuchsia::test]
async fn deprecated_open_service_node_reference() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_services {
        return;
    }
    let svc_dir = harness.open_service_directory().await;
    let (echo_proxy, echo_server) = create_proxy::<fio::NodeMarker>();
    svc_dir
        .deprecated_open(
            fio::OpenFlags::NODE_REFERENCE,
            fio::ModeType::empty(),
            EchoMarker::PROTOCOL_NAME,
            echo_server.into_channel().into(),
        )
        .unwrap();
    let (_, attrs) = echo_proxy
        .get_attributes(fio::NodeAttributesQuery::PROTOCOLS)
        .await
        .expect("transport error")
        .expect("get attributes");
    assert_eq!(attrs.protocols.unwrap(), fio::NodeProtocolKinds::CONNECTOR);
}

/// Creates a directory with a remote mount inside of it, and checks that the remote can be opened.
#[fuchsia::test]
async fn deprecated_open_remote_directory_test() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_remote_dir {
        return;
    }

    let remote_name = "remote_directory";
    let remote_dir = harness.get_directory(vec![], fio::PERM_READABLE | fio::PERM_WRITABLE);

    // Create a directory with the remote directory inside of it.
    let entries = vec![remote_directory(remote_name, remote_dir)];
    let dir = harness.get_directory(entries, fio::PERM_READABLE | fio::PERM_WRITABLE);

    deprecated_open_node::<fio::DirectoryMarker>(
        &dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
        remote_name,
    )
    .await;
}

/// Creates a directory with a remote mount containing a file inside of it, and checks that the
/// file can be opened through the remote.
#[fuchsia::test]
async fn deprecated_open_remote_file_test() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_remote_dir {
        return;
    }

    let remote_name = "remote_directory";
    let remote_entries = vec![file(TEST_FILE, vec![])];
    let remote_dir = harness.get_directory(remote_entries, fio::PERM_READABLE);

    // Create a directory with the remote directory inside of it.
    let entries = vec![remote_directory(remote_name, remote_dir)];
    let dir = harness.get_directory(entries, fio::PERM_READABLE | fio::PERM_WRITABLE);

    // Test opening file by opening the remote directory first and then opening the file.
    let remote_dir_proxy = deprecated_open_node::<fio::DirectoryMarker>(
        &dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
        remote_name,
    )
    .await;
    deprecated_open_node::<fio::NodeMarker>(
        &remote_dir_proxy,
        fio::OpenFlags::RIGHT_READABLE,
        TEST_FILE,
    )
    .await;

    // Test opening file directly though local directory by crossing remote automatically.
    deprecated_open_node::<fio::NodeMarker>(
        &dir,
        fio::OpenFlags::RIGHT_READABLE,
        [remote_name, "/", TEST_FILE].join("").as_str(),
    )
    .await;
}

/// Ensure specifying POSIX_* flags cannot cause rights escalation (https://fxbug.dev/42116881).
/// The test sets up the following hierarchy of nodes:
///
/// --------------------- RW   --------------------------
/// |  root_proxy       | ---> |  root                  |
/// --------------------- (a)  |   - /mount_point       | RWX  ---------------------
///                            |     (remote_proxy)     | ---> |  remote_dir       |
///                            -------------------------- (b)  ---------------------
///
/// To validate the right escalation issue has been resolved, we call Open() on the dir_proxy
/// passing in both POSIX_* flags, which if handled correctly, should result in opening
/// remote_dir_server as RW (and NOT RWX, which can occur if both flags are passed directly to the
/// remote instead of being removed).
#[fuchsia::test]
async fn deprecated_open_remote_directory_right_escalation_test() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_remote_dir {
        return;
    }

    let mount_point = "mount_point";

    // Use the test harness to serve a directory with RWX permissions.
    let remote_proxy = harness
        .get_directory(vec![], fio::PERM_READABLE | fio::PERM_WRITABLE | fio::PERM_EXECUTABLE);

    // Mount the remote directory through root, and ensure that the connection only has RW
    // RW permissions (which is thus a sub-set of the permissions the remote_proxy has).
    let entries = vec![remote_directory(mount_point, remote_proxy)];
    let root_proxy = harness.get_directory(entries, fio::PERM_READABLE | fio::PERM_WRITABLE);

    // Create a new proxy/server for opening the remote node through dir_proxy.
    // Here we pass the POSIX flag, which should only expand to the maximum set of
    // rights available along the open chain.
    let (node_proxy, node_server) = create_proxy::<fio::NodeMarker>();
    root_proxy
        .deprecated_open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE
                | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            mount_point,
            node_server,
        )
        .expect("Cannot open remote directory");

    // Since the root node only has RW permissions, and even though the remote has RWX,
    // we should only get RW permissions back.
    let (_, node_flags) = node_proxy.deprecated_get_flags().await.unwrap();
    assert_eq!(node_flags, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
}

#[fuchsia::test]
async fn deprecated_clone_file_with_same_or_fewer_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.file_rights.rights_combinations() {
        let file_rights = Rights::new(rights);
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file =
            deprecated_open_file_with_flags(&dir, file_rights.all_flags_deprecated(), TEST_FILE)
                .await;

        // Clone using every subset of flags.
        for clone_flags in file_rights.combinations_deprecated() {
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            file.deprecated_clone(clone_flags | fio::OpenFlags::DESCRIBE, server)
                .expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::OK);

            // Check flags of cloned connection are correct.
            let proxy = convert_node_proxy::<fio::FileProxy>(proxy);
            let (status, flags) = proxy.deprecated_get_flags().await.expect("get_flags failed");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            assert_eq!(flags, clone_flags);
        }
    }
}

#[fuchsia::test]
async fn deprecated_clone_file_with_same_rights_flag() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.combinations_deprecated() {
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = deprecated_open_file_with_flags(&dir, file_flags, TEST_FILE).await;

        // Clone using CLONE_FLAG_SAME_RIGHTS.
        let (proxy, server) = create_proxy::<fio::NodeMarker>();
        file.deprecated_clone(fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE, server)
            .expect("clone failed");
        let status = get_open_status(&proxy).await;
        assert_eq!(status, zx::Status::OK);

        // Check flags of cloned connection are correct.
        let proxy = convert_node_proxy::<fio::FileProxy>(proxy);
        let (status, flags) = proxy.deprecated_get_flags().await.expect("get_flags failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert_eq!(flags, file_flags);
    }
}

#[fuchsia::test]
async fn deprecated_clone_file_with_additional_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.file_rights.rights_combinations() {
        let file_rights = Rights::new(rights);
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file =
            deprecated_open_file_with_flags(&dir, file_rights.all_flags_deprecated(), TEST_FILE)
                .await;

        // Clone using every superset of flags, should fail.
        for clone_flags in
            harness.file_rights.combinations_containing_deprecated(file_rights.all_rights())
        {
            if clone_flags == file_rights.all_flags_deprecated() {
                continue;
            }
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            file.deprecated_clone(clone_flags | fio::OpenFlags::DESCRIBE, server)
                .expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::ACCESS_DENIED);
        }
    }
}

#[fuchsia::test]
async fn deprecated_clone_directory_with_same_or_fewer_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.dir_rights.rights_combinations() {
        let dir_rights = Rights::new(rights);
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir =
            deprecated_open_dir_with_flags(&dir, dir_rights.all_flags_deprecated(), "dir").await;

        // Clone using every subset of flags.
        for clone_flags in Rights::new(dir_rights.all_rights()).combinations_deprecated() {
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            dir.deprecated_clone(clone_flags | fio::OpenFlags::DESCRIBE, server)
                .expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::OK);

            // Check flags of cloned connection are correct.
            let (status, flags) = proxy.deprecated_get_flags().await.expect("get_flags failed");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            assert_eq!(flags, clone_flags);
        }
    }
}

#[fuchsia::test]
async fn deprecated_clone_directory_with_same_rights_flag() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = deprecated_open_dir_with_flags(&dir, dir_flags, "dir").await;

        // Clone using CLONE_FLAG_SAME_RIGHTS.
        let (proxy, server) = create_proxy::<fio::NodeMarker>();
        dir.deprecated_clone(fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE, server)
            .expect("clone failed");
        let status = get_open_status(&proxy).await;
        assert_eq!(status, zx::Status::OK);

        // Check flags of cloned connection are correct.
        let proxy = convert_node_proxy::<fio::DirectoryProxy>(proxy);
        let (status, flags) = proxy.deprecated_get_flags().await.expect("get_flags failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert_eq!(flags, dir_flags);
    }
}

#[fuchsia::test]
async fn deprecated_clone_directory_with_additional_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.dir_rights.rights_combinations() {
        let dir_rights = Rights::new(rights);
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir =
            deprecated_open_dir_with_flags(&dir, dir_rights.all_flags_deprecated(), "dir").await;

        // Clone using every superset of flags, should fail.
        for clone_flags in
            harness.dir_rights.combinations_containing_deprecated(dir_rights.all_rights())
        {
            if clone_flags == dir_rights.all_flags_deprecated() {
                continue;
            }
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            dir.deprecated_clone(clone_flags | fio::OpenFlags::DESCRIBE, server)
                .expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::ACCESS_DENIED);
        }
    }
}

#[fuchsia::test]
async fn deprecated_set_attr_and_set_flags_on_node() {
    let harness = TestHarness::new().await;
    let entries = vec![file("file", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

    let proxy =
        dir.open_node::<fio::NodeMarker>("file", fio::Flags::PROTOCOL_NODE, None).await.unwrap();

    assert_eq!(
        zx::Status::ok(
            proxy
                .deprecated_set_attr(
                    fio::NodeAttributeFlags::MODIFICATION_TIME,
                    &fio::NodeAttributes {
                        mode: 0,
                        id: 0,
                        content_size: 0,
                        storage_size: 0,
                        link_count: 0,
                        creation_time: 0,
                        modification_time: 1234
                    }
                )
                .await
                .expect("set_attr failed")
        ),
        Err(zx::Status::BAD_HANDLE)
    );
    assert_eq!(
        zx::Status::ok(
            proxy.deprecated_set_flags(fio::OpenFlags::APPEND).await.expect("set_flags failed")
        ),
        Err(zx::Status::BAD_HANDLE)
    );
}

#[fuchsia::test]
async fn deprecated_set_attr_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for file_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![file("file", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = deprecated_open_file_with_flags(&dir, file_flags, "file").await;

        let (status, old_attr) = file.deprecated_get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        // Set CREATION_TIME flag, but not MODIFICATION_TIME.
        let status = file
            .deprecated_set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let (status, new_attr) = file.deprecated_get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        // Check that only creation_time was updated.
        let expected = fio::NodeAttributes { creation_time: 111, ..old_attr };
        assert_eq!(new_attr, expected);
    }
}

#[fuchsia::test]
async fn deprecated_set_attr_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for file_flags in harness.file_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![file("file", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = deprecated_open_file_with_flags(&dir, file_flags, "file").await;

        let status = file
            .deprecated_set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::BAD_HANDLE);
    }
}

#[fuchsia::test]
async fn deprecated_set_attr_directory_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for dir_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = deprecated_open_dir_with_flags(&dir, dir_flags, "dir").await;

        let (status, old_attr) = dir.deprecated_get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        // Set CREATION_TIME flag, but not MODIFICATION_TIME.
        let status = dir
            .deprecated_set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let (status, new_attr) = dir.deprecated_get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        // Check that only creation_time was updated.
        let expected = fio::NodeAttributes { creation_time: 111, ..old_attr };
        assert_eq!(new_attr, expected);
    }
}

#[fuchsia::test]
async fn deprecated_set_attr_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for dir_flags in harness.file_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = deprecated_open_dir_with_flags(&dir, dir_flags, "dir").await;

        let status = dir
            .deprecated_set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::BAD_HANDLE);
    }
}
