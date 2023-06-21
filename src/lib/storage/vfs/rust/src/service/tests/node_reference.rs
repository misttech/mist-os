// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests that connect to the service node itself.

use super::endpoint;

// Macros are exported into the root of the crate.
use crate::{
    assert_close, assert_event, assert_get_attr, assert_get_attributes, clone_get_proxy_assert,
    clone_get_service_proxy_assert_ok,
};

use crate::{
    directory::entry::DirectoryEntry,
    execution_scope::ExecutionScope,
    file::test_utils::{run_client, run_server_client},
    path::Path,
};

use {
    assert_matches::assert_matches,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio,
    fuchsia_async::TestExecutor,
    fuchsia_zircon::Status,
    futures::StreamExt,
    libc::{S_IRUSR, S_IWUSR},
};

#[test]
fn construction() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |proxy| async move {
            assert_close!(proxy);
        },
    );
}

#[test]
fn get_attr() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |proxy| async move {
            assert_get_attr!(
                proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );
            assert_close!(proxy);
        },
    );
}

#[test]
fn get_attributes() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |proxy| async move {
            assert_get_attributes!(
                proxy,
                fio::NodeAttributesQuery::all(),
                attributes!(
                    fio::NodeAttributesQuery::all(),
                    Mutable {
                        creation_time: 0,
                        modification_time: 0,
                        mode: 0,
                        uid: 0,
                        gid: 0,
                        rdev: 0
                    },
                    Immutable {
                        protocols: fio::NodeProtocolKinds::CONNECTOR,
                        abilities: fio::Operations::GET_ATTRIBUTES | fio::Operations::CONNECT,
                        content_size: 0,
                        storage_size: 0,
                        link_count: 1,
                        id: fio::INO_UNKNOWN,
                    }
                )
            );
            assert_close!(proxy);
        },
    );
}

#[test]
fn describe() {
    let exec = TestExecutor::new();

    let server = endpoint(|_scope, _channel| ());

    run_client(exec, || async move {
        let scope = ExecutionScope::new();
        let (proxy, server_end) =
            create_proxy::<fio::FileMarker>().expect("Failed to create connection endpoints");

        let flags = fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE;
        server.open(scope, flags, Path::dot(), server_end.into_channel().into());

        assert_event!(proxy, fio::FileEvent::OnOpen_ { s, info }, {
            assert_eq!(s, Status::OK.into_raw());
            assert_eq!(info, Some(Box::new(fio::NodeInfoDeprecated::Service(fio::Service))));
        });
    });
}

#[test]
fn clone() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |first_proxy| async move {
            assert_get_attr!(
                first_proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );

            let second_proxy = clone_get_service_proxy_assert_ok!(
                &first_proxy,
                fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE
            );

            assert_get_attr!(
                second_proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );

            assert_get_attr!(
                first_proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );

            assert_close!(second_proxy);
            assert_close!(first_proxy);
        },
    );
}

#[test]
fn clone_same_rights() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |first_proxy| async move {
            assert_get_attr!(
                first_proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );

            let second_proxy = clone_get_service_proxy_assert_ok!(
                &first_proxy,
                fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE
            );

            assert_get_attr!(
                second_proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );

            assert_get_attr!(
                first_proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );

            assert_close!(second_proxy);
            assert_close!(first_proxy);
        },
    );
}

#[test]
fn update_attributes_not_supported() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |node_proxy| async move {
            let response =
                node_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap();
            assert_eq!(response, Err(Status::NOT_SUPPORTED.into_raw()));
        },
    );
}

#[test]
fn reopen_not_supported() {
    run_server_client(
        fio::OpenFlags::NODE_REFERENCE,
        endpoint(|_scope, _channel| ()),
        |node_proxy| async move {
            let (client_end, server_end) = create_proxy::<fio::NodeMarker>().unwrap();

            node_proxy.reopen(&fio::RightsRequest::default(), server_end).unwrap();

            let mut event_stream = client_end.take_event_stream();
            assert_matches!(
                event_stream.next().await,
                Some(Err(fidl::Error::ClientChannelClosed { status: Status::NOT_SUPPORTED, .. }))
            );
        },
    );
}
