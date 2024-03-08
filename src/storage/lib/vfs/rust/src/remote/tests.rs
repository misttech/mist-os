// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the remote node.

use super::{remote_dir, RemoteLike};

use crate::{assert_close, assert_read, assert_read_dirents, pseudo_directory};

use crate::{
    directory::{
        entry_container::Directory,
        test_utils::{run_client, DirentsSameInodeBuilder},
    },
    execution_scope::ExecutionScope,
    object_request::ToObjectRequest as _,
    path::Path,
    test_utils::test_file::TestFile,
};

use {fidl::endpoints::ServerEnd, fidl_fuchsia_io as fio, fuchsia_async as fasync};

fn set_up_remote(scope: ExecutionScope) -> fio::DirectoryProxy {
    let r = pseudo_directory! {
        "a" => TestFile::read_only("a content"),
        "dir" => pseudo_directory! {
            "b" => TestFile::read_only("b content"),
        }
    };

    let (remote_proxy, remote_server_end) =
        fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    r.open(
        scope,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
        Path::dot(),
        ServerEnd::new(remote_server_end.into_channel()),
    );

    remote_proxy
}

#[fuchsia::test]
async fn test_set_up_remote() {
    let scope = ExecutionScope::new();
    let remote_proxy = set_up_remote(scope.clone());
    assert_close!(remote_proxy);
}

// Tests for opening a remote node with the NODE_REFERENCE flag. The remote node uses the existing
// Service connection type after construction, which is tested in service/tests/node_reference.rs.
#[test]
fn remote_dir_construction_open_node_ref() {
    let exec = fasync::TestExecutor::new();
    let scope = ExecutionScope::new();

    let remote_proxy = set_up_remote(scope.clone());
    let server = remote_dir(remote_proxy);

    run_client(exec, || async move {
        // Test open1.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        let flags = fio::OpenFlags::NODE_REFERENCE;
        server.clone().open(scope.clone(), flags, Path::dot(), server_end.into_channel().into());
        assert_close!(proxy);

        // Test open2.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            protocols: Some(fio::NodeProtocols {
                node: Some(Default::default()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let object_request = protocols.to_object_request(server_end);
        object_request.handle(|request| server.open2(scope, Path::dot(), protocols, request));
        assert_close!(proxy);
    })
}

#[test]
fn remote_dir_node_ref_with_path() {
    let exec = fasync::TestExecutor::new();
    let scope = ExecutionScope::new();

    let remote_proxy = set_up_remote(scope.clone());
    let server = remote_dir(remote_proxy);

    let path = Path::validate_and_split("dir/b").unwrap();

    run_client(exec, || async move {
        // Test open1.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        let flags = fio::OpenFlags::NODE_REFERENCE;
        server.clone().open(scope.clone(), flags, path.clone(), server_end.into_channel().into());
        assert_close!(proxy);

        // Test open2.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            protocols: Some(fio::NodeProtocols {
                node: Some(Default::default()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let object_request = protocols.to_object_request(server_end);
        object_request.handle(|request| server.open2(scope, path, protocols, request));
        assert_close!(proxy);
    })
}

// Tests for opening a remote node where we actually want the open request to be forwarded.
#[test]
fn remote_dir_direct_connection() {
    let exec = fasync::TestExecutor::new();
    let scope = ExecutionScope::new();

    let remote_proxy = set_up_remote(scope.clone());
    let server = remote_dir(remote_proxy);

    run_client(exec, || async move {
        // Test open1.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY;
        server.clone().open(scope.clone(), flags, Path::dot(), server_end.into_channel().into());
        let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
        expected
            // (10 + 1) = 11
            .add(fio::DirentType::Directory, b".")
            // 11 + (10 + 1) = 22
            .add(fio::DirentType::File, b"a");
        assert_read_dirents!(proxy, 22, expected.into_vec());
        assert_close!(proxy);

        // Test open2.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            protocols: Some(fio::NodeProtocols {
                directory: Some(Default::default()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let object_request = protocols.to_object_request(server_end);
        object_request.handle(|request| server.open2(scope, Path::dot(), protocols, request));
        let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
        expected
            // (10 + 1) = 11
            .add(fio::DirentType::Directory, b".")
            // 11 + (10 + 1) = 22
            .add(fio::DirentType::File, b"a");
        assert_read_dirents!(proxy, 22, expected.into_vec());
        assert_close!(proxy);
    })
}

#[test]
fn remote_dir_direct_connection_dir_contents() {
    let exec = fasync::TestExecutor::new();
    let scope = ExecutionScope::new();

    let remote_proxy = set_up_remote(scope.clone());
    let server = remote_dir(remote_proxy);

    run_client(exec, || async move {
        // Test open1.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY;
        let path = Path::validate_and_split("a").unwrap();
        server.clone().open(scope.clone(), flags, path.clone(), server_end.into_channel().into());
        assert_read!(proxy, "a content");
        assert_close!(proxy);

        // Test open2.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            protocols: Some(fio::NodeProtocols {
                file: Some(Default::default()),
                ..Default::default()
            }),
            rights: Some(fio::Operations::READ_BYTES),
            ..Default::default()
        });
        let object_request = protocols.to_object_request(server_end);
        object_request.handle(|request| server.open2(scope, path, protocols, request));
        assert_read!(proxy, "a content");
        assert_close!(proxy);
    })
}
