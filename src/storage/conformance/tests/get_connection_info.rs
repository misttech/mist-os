// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use io_conformance_util::flags::Rights;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

/// Verify allowed file operations map to the rights of the connection (when connection was opened
/// with Open1).
#[fuchsia::test]
async fn get_connection_info_file_using_open_deprecated() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.combinations_deprecated() {
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let file = open_file_with_flags(&dir, file_flags, TEST_FILE).await;

        // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
        let mut expected_operations = fio::Operations::GET_ATTRIBUTES;
        if file_flags.contains(fio::OpenFlags::RIGHT_READABLE) {
            expected_operations |= fio::Operations::READ_BYTES;
        }
        if file_flags.contains(fio::OpenFlags::RIGHT_WRITABLE) {
            expected_operations |=
                fio::Operations::WRITE_BYTES | fio::Operations::UPDATE_ATTRIBUTES;
        }
        if file_flags.contains(fio::OpenFlags::RIGHT_EXECUTABLE) {
            expected_operations |= fio::Operations::EXECUTE;
        }

        assert_eq!(
            file.get_connection_info().await.unwrap(),
            fio::ConnectionInfo { rights: Some(expected_operations), ..Default::default() }
        );
    }
}

/// Verify allowed file operations map to the rights of the connection.
#[fuchsia::test]
async fn get_connection_info_file() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.combinations() {
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let file = dir
            .open3_node::<fio::FileMarker>(TEST_FILE, fio::Flags::PROTOCOL_FILE | file_flags, None)
            .await
            .expect("open3 failed");

        assert_eq!(
            file.get_connection_info().await.unwrap(),
            fio::ConnectionInfo {
                rights: Some(fio::Operations::from_bits_truncate(file_flags.bits())),
                ..Default::default()
            }
        );
    }
}

/// Verify that only allowed file operations map to the rights of the connection.
#[fuchsia::test]
async fn get_connection_info_file_with_directory_rights() {
    let harness = TestHarness::new().await;

    const MUTABLE_FILE_ALLOWED_RIGHT_FLAGS: fio::Flags = fio::Flags::empty()
        .union(fio::Flags::PERM_WRITE)
        .union(fio::Flags::PERM_MODIFY)
        .union(fio::Flags::PERM_EXECUTE)
        .union(fio::Flags::PERM_SET_ATTRIBUTES);

    const FILE_ALLOWED_RIGHTS: fio::Operations = fio::Operations::empty()
        .union(fio::Operations::GET_ATTRIBUTES)
        .union(fio::Operations::READ_BYTES)
        .union(fio::Operations::WRITE_BYTES)
        .union(fio::Operations::UPDATE_ATTRIBUTES)
        .union(fio::Operations::EXECUTE);

    for directory_flags in harness.dir_rights.combinations_containing(fio::Rights::GET_ATTRIBUTES) {
        if directory_flags.intersects(MUTABLE_FILE_ALLOWED_RIGHT_FLAGS)
            && !harness.config.supports_mutable_file
        {
            continue;
        }
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let file = dir
            .open3_node::<fio::FileMarker>(
                TEST_FILE,
                fio::Flags::PROTOCOL_FILE | directory_flags,
                None,
            )
            .await
            .expect("open3 failed");

        let expected_file_rights = fio::Operations::from_bits_truncate(directory_flags.bits())
            .intersection(FILE_ALLOWED_RIGHTS);

        assert_eq!(
            file.get_connection_info().await.unwrap(),
            fio::ConnectionInfo { rights: Some(expected_file_rights), ..Default::default() }
        );
    }
}

/// Verify allowed operations for a node reference connection to a file (when connection was opened
/// with Open1).
#[fuchsia::test]
async fn get_connection_info_file_node_reference_deprecated() {
    let harness = TestHarness::new().await;

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
    let file = open_file_with_flags(&dir, fio::OpenFlags::NODE_REFERENCE, TEST_FILE).await;
    // Node references should only have the ability to get attributes.
    // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
    assert_eq!(
        file.get_connection_info().await.unwrap(),
        fio::ConnectionInfo { rights: Some(fio::Operations::GET_ATTRIBUTES), ..Default::default() }
    );
}

/// Verify allowed operations for a node reference connection to a file.
#[fuchsia::test]
async fn get_connection_info_file_node_reference() {
    let harness = TestHarness::new().await;

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    let node_allowed_rights = Rights::new(fio::Rights::GET_ATTRIBUTES);
    // This will return combinations of rights, including empty rights
    for rights in node_allowed_rights.rights_combinations() {
        let file = dir
            .open3_node::<fio::FileMarker>(
                TEST_FILE,
                fio::Flags::PROTOCOL_FILE
                    | fio::Flags::PROTOCOL_NODE
                    | fio::Flags::from_bits_truncate(rights.bits()),
                None,
            )
            .await
            .expect("open3 failed");

        // Note that in Open3, unless the connection was opened with GET_ATTRIBUTES rights, we do
        // not expect the connection to have that right. This is different from Open1 where
        // GET_ATTRIBUTES connection right is assumed when opening as a node.
        // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
        assert_eq!(
            file.get_connection_info().await.unwrap(),
            fio::ConnectionInfo { rights: Some(rights), ..Default::default() }
        );
    }
}

/// Verify allowed operations for a direct connection to a directory (when connection was opened
/// with Open1).
#[fuchsia::test]
async fn get_connection_info_directory_using_open_deprecated() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let dir = open_dir_with_flags(&dir, dir_flags, "dir").await;

        // TODO(https://fxbug.dev/42157659): Restrict GET_ATTRIBUTES, it is always requested when
        // opening nodes via Open1.
        let mut rights = fio::Operations::GET_ATTRIBUTES;
        if dir_flags.contains(fio::OpenFlags::RIGHT_READABLE) {
            rights |= fio::R_STAR_DIR;
        }
        if dir_flags.contains(fio::OpenFlags::RIGHT_WRITABLE) {
            rights |= fio::W_STAR_DIR;
        }
        if dir_flags.contains(fio::OpenFlags::RIGHT_EXECUTABLE) {
            rights |= fio::X_STAR_DIR;
        }
        assert_eq!(
            dir.get_connection_info().await.unwrap(),
            fio::ConnectionInfo { rights: Some(rights), ..Default::default() },
            "flags={dir_flags:?}"
        );
    }
}

/// Verify allowed operations for a direct connection to a directory.
#[fuchsia::test]
async fn get_connection_info_directory() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations() {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
        let dir = dir
            .open3_node::<fio::DirectoryMarker>(
                "dir",
                fio::Flags::PROTOCOL_DIRECTORY | dir_flags,
                None,
            )
            .await
            .expect("open3 failed");

        assert_eq!(
            dir.get_connection_info().await.unwrap(),
            fio::ConnectionInfo {
                rights: Some(fio::Operations::from_bits_truncate(dir_flags.bits())),
                ..Default::default()
            },
            "flags={dir_flags:?}"
        );
    }
}

/// Verify allowed operations for a node reference connection to a directory (when connection was
/// opened with Open1).
#[fuchsia::test]
async fn get_connection_info_directory_node_reference_using_open_deprecated() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());
    let dir = open_dir_with_flags(&dir, fio::OpenFlags::NODE_REFERENCE, "dir").await;
    assert_eq!(
        dir.get_connection_info().await.unwrap(),
        fio::ConnectionInfo { rights: Some(fio::Operations::GET_ATTRIBUTES), ..Default::default() }
    );
}

/// Verify allowed operations for a node reference connection to a directory.
#[fuchsia::test]
async fn get_connection_info_directory_node_reference() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    let node_allowed_rights = Rights::new(fio::Rights::GET_ATTRIBUTES);
    // This will return combinations of rights, including empty rights
    for rights in node_allowed_rights.rights_combinations() {
        let dir = dir
            .open3_node::<fio::DirectoryMarker>(
                "dir",
                fio::Flags::PROTOCOL_DIRECTORY
                    | fio::Flags::PROTOCOL_NODE
                    | fio::Flags::from_bits_truncate(rights.bits()),
                None,
            )
            .await
            .expect("open3 failed");

        // Note that in Open3, unless the connection was opened with GET_ATTRIBUTES rights, we do
        // not expect the connection to have that right. This is different from Open1 where
        // GET_ATTRIBUTES connection right is assumed when opening as a node.
        // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
        assert_eq!(
            dir.get_connection_info().await.unwrap(),
            fio::ConnectionInfo { rights: Some(rights), ..Default::default() }
        );
    }
}
