// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the remote node.

use super::{remote_dir, RemoteLike};

use crate::{assert_close, assert_read, assert_read_dirents, pseudo_directory};

use crate::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use crate::directory::entry_container::Directory;
use crate::directory::test_utils::{run_client, DirentsSameInodeBuilder};
use crate::execution_scope::ExecutionScope;
use crate::object_request::ObjectRequest;
use crate::path::Path;
use crate::{file, ObjectRequestRef};

use fidl::endpoints::{create_proxy, ServerEnd};
use futures::channel::oneshot;
use std::sync::{Arc, Mutex};
use zx_status::Status;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

fn set_up_remote(scope: ExecutionScope) -> fio::DirectoryProxy {
    let r = pseudo_directory! {
        "a" => file::read_only("a content"),
        "dir" => pseudo_directory! {
            "b" => file::read_only("b content"),
        }
    };

    let (remote_proxy, remote_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
        let flags = fio::OpenFlags::NODE_REFERENCE;
        server.clone().open(scope.clone(), flags, Path::dot(), server_end.into_channel().into());
        assert_close!(proxy);

        // Test open3.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
        let flags = fio::Flags::PROTOCOL_NODE;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|request| server.open3(scope, Path::dot(), flags, request));
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
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
        let flags = fio::OpenFlags::NODE_REFERENCE;
        server.clone().open(scope.clone(), flags, path.clone(), server_end.into_channel().into());
        assert_close!(proxy);

        // Test open3.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
        let flags = fio::Flags::PROTOCOL_NODE;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|request| server.open3(scope, Path::dot(), flags, request));
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
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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

        // Test open3.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::PERM_READ;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|request| server.open3(scope, Path::dot(), flags, request));
        expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
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
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY;
        let path = Path::validate_and_split("a").unwrap();
        server.clone().open(scope.clone(), flags, path.clone(), server_end.into_channel().into());
        assert_read!(proxy, "a content");
        assert_close!(proxy);

        // Test open3.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
        let flags = fio::Flags::PROTOCOL_FILE | fio::Flags::PERM_READ;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|request| server.open3(scope, path, flags, request));
        assert_read!(proxy, "a content");
        assert_close!(proxy);
    })
}

#[fuchsia::test]
async fn lazy_remote() {
    struct Remote(Mutex<Option<oneshot::Sender<()>>>);
    impl DirectoryEntry for Remote {
        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
            request.open_remote(self)
        }
    }
    impl GetEntryInfo for Remote {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Unknown)
        }
    }
    impl RemoteLike for Remote {
        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _flags: fio::OpenFlags,
            _path: Path,
            _server_end: ServerEnd<fio::NodeMarker>,
        ) {
            self.0.lock().unwrap().take().unwrap().send(()).unwrap();
        }

        fn open3(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _path: Path,
            _flags: fio::Flags,
            _object_request: ObjectRequestRef<'_>,
        ) -> Result<(), Status> {
            self.0.lock().unwrap().take().unwrap().send(()).unwrap();
            Ok(())
        }

        fn lazy(&self, _path: &Path) -> bool {
            true
        }
    }
    let (sender, mut receiver) = oneshot::channel();

    let root = pseudo_directory! {
        "remote" => Arc::new(Remote(Mutex::new(Some(sender)))),
    };

    let scope = ExecutionScope::new();

    let (client, server_end) = create_proxy();
    root.open(
        scope.clone(),
        fio::OpenFlags::empty(),
        Path::validate_and_split("remote").unwrap(),
        server_end,
    );

    // The open shouldn't get forwarded until we write something to client.
    assert_eq!(receiver.try_recv().unwrap(), None);

    // Sending get_attributes should cause the open to trigger.
    let _ = client.get_attributes(fio::NodeAttributesQuery::default());

    receiver.await.expect("Open not sent");
}
