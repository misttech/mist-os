// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn create_directory_with_create_if_absent_flag() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all_flags_deprecated());

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
    if !harness.config.supports_modify_directory {
        return;
    }

    for dir_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let root = root_directory(vec![]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all_flags_deprecated());
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
    if !harness.config.supports_modify_directory {
        return;
    }

    for dir_flags in harness.file_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let root = root_directory(vec![]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all_flags_deprecated());
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

#[fuchsia::test]
async fn create_directory() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // A request to create a new object requires that the parent connection has the right to modify.
    let dir_with_sufficient_rights = root_dir
        .open3_node::<fio::DirectoryMarker>(
            "test_dir",
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MUST_CREATE | fio::Flags::PERM_MODIFY,
            None,
        )
        .await
        .expect("open3 failed.");

    assert_matches!(
        dir_with_sufficient_rights
            .open3_node::<fio::DirectoryMarker>("dir", fio::Flags::FLAG_MAYBE_CREATE, None,)
            .await
            .expect_err("open should fail when creating directory without specifiying a protocol."),
        zx::Status::INVALID_ARGS
    );

    let (_, representation) = dir_with_sufficient_rights
        .open3_node_repr::<fio::DirectoryMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::FLAG_MAYBE_CREATE,
            None,
        )
        .await
        .expect("open with directory protocol failed.");
    assert_matches!(representation, fio::Representation::Directory(_));

    let (_, representation) = dir_with_sufficient_rights
        .open3_node_repr::<fio::FileMarker>(
            TEST_FILE,
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_FILE
                | fio::Flags::FLAG_MAYBE_CREATE,
            None,
        )
        .await
        .expect("open with with file protocol failed.");
    assert_matches!(representation, fio::Representation::File(_));
}

#[fuchsia::test]
async fn create_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // A request to create a new object requires that the parent connection has the right to modify.
    let dir_with_insufficient_rights = root_dir
        .open3_node::<fio::DirectoryMarker>(
            "test_dir",
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MUST_CREATE,
            None,
        )
        .await
        .expect("open3 failed.");

    // Should fail without update_attribues
    assert_matches!(
        dir_with_insufficient_rights
            .open3_node::<fio::DirectoryMarker>(
                "dir",
                fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MAYBE_CREATE,
                None,
            )
            .await
            .expect_err(
                "create directory should fail when parent node does not have modify rights."
            ),
        zx::Status::ACCESS_DENIED
    );
}

#[fuchsia::test]
async fn create_directory_with_create_attributes() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory
        || !harness.config.supported_attributes.contains(fio::NodeAttributesQuery::MODE)
    {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // A request to create create a new object requires that the parent directory has the right to
    // modify directory.
    let dir_with_sufficient_rights = root_dir
        .open3_node::<fio::DirectoryMarker>(
            "test_dir",
            fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::FLAG_MUST_CREATE
                | fio::Flags::PERM_MODIFY
                | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .expect("open3 failed.");

    let (_, representation) = dir_with_sufficient_rights
        .open3_node_repr::<fio::DirectoryMarker>(
            "dir",
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::FLAG_MAYBE_CREATE,
            Some(fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    mode: Some(111),
                    ..Default::default()
                }),
                attributes: Some(fio::NodeAttributesQuery::MODE),
                ..Default::default()
            }),
        )
        .await
        .expect("create directory failed.");
    assert_matches!(representation, fio::Representation::Directory(_));
    assert_matches!(
        representation,
        fio::Representation::Directory(fio::DirectoryInfo {
            attributes: Some(fio::NodeAttributes2 { mutable_attributes, immutable_attributes }),
            ..
        })
        if immutable_attributes == fio::ImmutableNodeAttributes::default()
            && mutable_attributes == fio::MutableNodeAttributes {
                mode: Some(111),
                ..Default::default()
            }
    );
}

#[fuchsia::test]
async fn open_directory_with_never_create_and_create_attributes() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory
        || !harness.config.supported_attributes.contains(fio::NodeAttributesQuery::MODE)
    {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![directory("dir", vec![])]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Only open an existing object, never create a new object.
    let flags = fio::Flags::FLAG_SEND_REPRESENTATION;

    let status = root_dir
        .open3_node_repr::<fio::DirectoryMarker>(
            "dir",
            flags,
            Some(fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    mode: Some(111),
                    ..Default::default()
                }),
                attributes: Some(fio::NodeAttributesQuery::MODE),
                ..Default::default()
            }),
        )
        .await
        .expect_err("open should fail when mode is never create and setting create attributes.");
    assert_matches!(status, zx::Status::INVALID_ARGS);
}

#[fuchsia::test]
async fn create_file() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // A request to create a new object requires that the parent connection has the right to modify.
    let dir_with_sufficient_rights = root_dir
        .open3_node::<fio::DirectoryMarker>(
            "test_dir",
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MUST_CREATE | fio::Flags::PERM_MODIFY,
            None,
        )
        .await
        .expect("open3 failed.");

    let (_, representation) = dir_with_sufficient_rights
        .open3_node_repr::<fio::FileMarker>(
            TEST_FILE,
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_FILE
                | fio::Flags::FLAG_MAYBE_CREATE,
            None,
        )
        .await
        .expect("open with file protocol failed.");
    assert_matches!(representation, fio::Representation::File(_));
}

#[fuchsia::test]
async fn create_file_with_insufficient_rights_open3() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // A request to create a new object requires that the parent connection has the right to modify.
    let dir_with_insufficient_rights = root_dir
        .open3_node::<fio::DirectoryMarker>(
            "test_dir",
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::FLAG_MUST_CREATE,
            None,
        )
        .await
        .expect("open3 failed.");

    // Should fail without update_attribues
    assert_matches!(
        dir_with_insufficient_rights
            .open3_node::<fio::FileMarker>(
                TEST_FILE,
                fio::Flags::PROTOCOL_FILE | fio::Flags::FLAG_MAYBE_CREATE,
                None,
            )
            .await
            .expect_err("create file should fail when parent node does not have modify rights."),
        zx::Status::ACCESS_DENIED
    );
}

#[fuchsia::test]
async fn create_file_with_create_attributes() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory
        || !harness.config.supported_attributes.contains(fio::NodeAttributesQuery::MODE)
    {
        return;
    }
    let root_dir = harness.get_directory(
        root_directory(vec![]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // A request to create create a new object requires that the parent directory has the right to
    // modify directory.
    let dir_with_sufficient_rights = root_dir
        .open3_node::<fio::DirectoryMarker>(
            "test_dir",
            fio::Flags::PROTOCOL_DIRECTORY
                | fio::Flags::FLAG_MUST_CREATE
                | fio::Flags::PERM_MODIFY
                | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .expect("open3 failed.");

    let (_, representation) = dir_with_sufficient_rights
        .open3_node_repr::<fio::FileMarker>(
            TEST_FILE,
            fio::Flags::FLAG_SEND_REPRESENTATION
                | fio::Flags::PROTOCOL_FILE
                | fio::Flags::FLAG_MAYBE_CREATE,
            Some(fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    mode: Some(123),
                    ..Default::default()
                }),
                attributes: Some(fio::NodeAttributesQuery::MODE),
                ..Default::default()
            }),
        )
        .await
        .expect("create file failed.");
    assert_matches!(representation, fio::Representation::File(_));
    assert_matches!(
        representation,
        fio::Representation::File(fio::FileInfo {
            attributes: Some(fio::NodeAttributes2 { mutable_attributes, immutable_attributes }),
            ..
        })
        if immutable_attributes == fio::ImmutableNodeAttributes::default()
            && mutable_attributes == fio::MutableNodeAttributes {
                mode: Some(123),
                ..Default::default()
            }
    );
}

#[fuchsia::test]
async fn open_file_with_never_create_and_create_attributes() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_modify_directory
        || !harness.config.supported_attributes.contains(fio::NodeAttributesQuery::MODE)
    {
        return;
    }

    let root_dir = harness.get_directory(
        root_directory(vec![file(TEST_FILE, vec![])]),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
    );

    // Only open an existing object, never create a new object.
    let flags = fio::Flags::FLAG_SEND_REPRESENTATION;

    let status = root_dir
        .open3_node_repr::<fio::FileMarker>(
            TEST_FILE,
            flags,
            Some(fio::Options {
                create_attributes: Some(fio::MutableNodeAttributes {
                    mode: Some(123),
                    ..Default::default()
                }),
                attributes: Some(fio::NodeAttributesQuery::MODE),
                ..Default::default()
            }),
        )
        .await
        .expect_err("open should fail when mode is never create and setting create attributes.");
    assert_matches!(status, zx::Status::INVALID_ARGS);
}
