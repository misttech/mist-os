// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::{create_proxy, Proxy as _, ServerEnd};
use fidl_fuchsia_io as fio;
use io_conformance_util::flags::Rights;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn clone_file_with_same_or_fewer_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.file_rights.rights_combinations() {
        let file_rights = Rights::new(rights);
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = open_file_with_flags(&dir, file_rights.all_flags_deprecated(), TEST_FILE).await;

        // Clone using every subset of flags.
        for clone_flags in file_rights.combinations_deprecated() {
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            file.clone(clone_flags | fio::OpenFlags::DESCRIBE, server).expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::OK);

            // Check flags of cloned connection are correct.
            let proxy = convert_node_proxy::<fio::FileProxy>(proxy);
            let (status, flags) = proxy.get_flags().await.expect("get_flags failed");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            assert_eq!(flags, clone_flags);
        }
    }
}

#[fuchsia::test]
async fn clone_file_with_same_rights_flag() {
    let harness = TestHarness::new().await;

    for file_flags in harness.file_rights.combinations_deprecated() {
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = open_file_with_flags(&dir, file_flags, TEST_FILE).await;

        // Clone using CLONE_FLAG_SAME_RIGHTS.
        let (proxy, server) = create_proxy::<fio::NodeMarker>();
        file.clone(fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE, server)
            .expect("clone failed");
        let status = get_open_status(&proxy).await;
        assert_eq!(status, zx::Status::OK);

        // Check flags of cloned connection are correct.
        let proxy = convert_node_proxy::<fio::FileProxy>(proxy);
        let (status, flags) = proxy.get_flags().await.expect("get_flags failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert_eq!(flags, file_flags);
    }
}

#[fuchsia::test]
async fn clone_file_with_additional_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.file_rights.rights_combinations() {
        let file_rights = Rights::new(rights);
        let entries = vec![file(TEST_FILE, vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = open_file_with_flags(&dir, file_rights.all_flags_deprecated(), TEST_FILE).await;

        // Clone using every superset of flags, should fail.
        for clone_flags in
            harness.file_rights.combinations_containing_deprecated(file_rights.all_rights())
        {
            if clone_flags == file_rights.all_flags_deprecated() {
                continue;
            }
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            file.clone(clone_flags | fio::OpenFlags::DESCRIBE, server).expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::ACCESS_DENIED);
        }
    }
}

#[fuchsia::test]
async fn clone_directory_with_same_or_fewer_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.dir_rights.rights_combinations() {
        let dir_rights = Rights::new(rights);
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = open_dir_with_flags(&dir, dir_rights.all_flags_deprecated(), "dir").await;

        // Clone using every subset of flags.
        for clone_flags in Rights::new(dir_rights.all_rights()).combinations_deprecated() {
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            dir.clone(clone_flags | fio::OpenFlags::DESCRIBE, server).expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::OK);

            // Check flags of cloned connection are correct.
            let (status, flags) = proxy.get_flags().await.expect("get_flags failed");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            assert_eq!(flags, clone_flags);
        }
    }
}

#[fuchsia::test]
async fn clone_directory_with_same_rights_flag() {
    let harness = TestHarness::new().await;

    for dir_flags in harness.dir_rights.combinations_deprecated() {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = open_dir_with_flags(&dir, dir_flags, "dir").await;

        // Clone using CLONE_FLAG_SAME_RIGHTS.
        let (proxy, server) = create_proxy::<fio::NodeMarker>();
        dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE, server)
            .expect("clone failed");
        let status = get_open_status(&proxy).await;
        assert_eq!(status, zx::Status::OK);

        // Check flags of cloned connection are correct.
        let proxy = convert_node_proxy::<fio::DirectoryProxy>(proxy);
        let (status, flags) = proxy.get_flags().await.expect("get_flags failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert_eq!(flags, dir_flags);
    }
}

#[fuchsia::test]
async fn clone_directory_with_additional_rights() {
    let harness = TestHarness::new().await;

    for rights in harness.dir_rights.rights_combinations() {
        let dir_rights = Rights::new(rights);
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = open_dir_with_flags(&dir, dir_rights.all_flags_deprecated(), "dir").await;

        // Clone using every superset of flags, should fail.
        for clone_flags in
            harness.dir_rights.combinations_containing_deprecated(dir_rights.all_rights())
        {
            if clone_flags == dir_rights.all_flags_deprecated() {
                continue;
            }
            let (proxy, server) = create_proxy::<fio::NodeMarker>();
            dir.clone(clone_flags | fio::OpenFlags::DESCRIBE, server).expect("clone failed");
            let status = get_open_status(&proxy).await;
            assert_eq!(status, zx::Status::ACCESS_DENIED);
        }
    }
}

#[fuchsia::test]
async fn clone2_file() {
    let harness = TestHarness::new().await;

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file = dir
        .open3_node::<fio::FileMarker>(
            &TEST_FILE,
            fio::Flags::PROTOCOL_FILE | fio::Flags::PERM_GET_ATTRIBUTES | fio::Flags::PERM_READ,
            None,
        )
        .await
        .unwrap();

    let (clone_proxy, clone_server) = fidl::endpoints::create_proxy::<fio::FileMarker>();
    file.clone2(ServerEnd::new(clone_server.into_channel())).unwrap();
    assert_eq!(
        clone_proxy.get_connection_info().await.unwrap(),
        fio::ConnectionInfo {
            rights: Some(fio::Rights::GET_ATTRIBUTES | fio::Rights::READ_BYTES),
            ..Default::default()
        }
    );
    // Make sure the file protocol was served by invoking some file methods.
    let _data: Vec<u8> = clone_proxy
        .read(0)
        .await
        .expect("read failed")
        .map_err(zx::Status::from_raw)
        .expect("read error");
}

#[fuchsia::test]
async fn clone2_file_node_reference() {
    let harness = TestHarness::new().await;

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let node = dir
        .open3_node::<fio::NodeMarker>(
            &TEST_FILE,
            fio::Flags::PROTOCOL_NODE | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .unwrap();

    // fuchsia.unknown/Cloneable.Clone2
    let (clone_proxy, clone_server) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
    node.clone2(ServerEnd::new(clone_server.into_channel())).unwrap();
    assert_eq!(
        clone_proxy.get_connection_info().await.unwrap(),
        fio::ConnectionInfo { rights: Some(fio::Rights::GET_ATTRIBUTES), ..Default::default() }
    );

    // We should fail to invoke file methods since this connection doesn't serve the file protocol.
    let wrong_protocol = fio::FileProxy::from_channel(clone_proxy.into_channel().unwrap());
    assert_matches!(
        wrong_protocol.read(0).await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::PEER_CLOSED, .. })
    );
}

#[fuchsia::test]
async fn clone2_directory() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir = dir
        .open3_node::<fio::DirectoryMarker>(
            "dir",
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::PERM_TRAVERSE,
            None,
        )
        .await
        .unwrap();

    let (clone_proxy, clone_server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    dir.clone2(ServerEnd::new(clone_server.into_channel())).unwrap();
    assert_eq!(
        clone_proxy.get_connection_info().await.unwrap(),
        fio::ConnectionInfo { rights: Some(fio::Rights::TRAVERSE), ..Default::default() }
    );
    // Make sure the directory protocol was served by invoking a directory method.
    assert_matches!(
        clone_proxy.open3_node::<fio::NodeMarker>(".", fio::Flags::PROTOCOL_NODE, None).await,
        Ok(_)
    );
}

#[fuchsia::test]
async fn clone2_directory_node_reference() {
    let harness = TestHarness::new().await;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let node = dir
        .open3_node::<fio::NodeMarker>(
            "dir",
            fio::Flags::PROTOCOL_NODE | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .unwrap();

    // fuchsia.unknown/Cloneable.Clone2
    let (clone_proxy, clone_server) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
    node.clone2(ServerEnd::new(clone_server.into_channel())).unwrap();
    assert_eq!(
        clone_proxy.get_connection_info().await.unwrap(),
        fio::ConnectionInfo { rights: Some(fio::Rights::GET_ATTRIBUTES), ..Default::default() }
    );

    // We should fail to invoke directory methods since this isn't a directory connection.
    let wrong_protocol = fio::DirectoryProxy::from_channel(clone_proxy.into_channel().unwrap());
    assert_matches!(
        wrong_protocol.open3_node::<fio::NodeMarker>(".", fio::Flags::PROTOCOL_NODE, None).await,
        Err(zx::Status::PEER_CLOSED)
    );
}
