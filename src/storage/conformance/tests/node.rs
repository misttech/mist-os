// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::TryStreamExt,
    io_conformance_util::{file, root_directory, test_harness::TestHarness},
};

#[fuchsia::test]
async fn test_open_node_on_directory() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            ".",
            &fio::ConnectionProtocols::Node(fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
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
        Some(fio::Representation::Connector(fio::ConnectorInfo { .. }))
    );
}

#[fuchsia::test]
async fn test_open_node_on_file() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file("file", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &fio::ConnectionProtocols::Node(fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
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
        Some(fio::Representation::Connector(fio::ConnectorInfo { .. }))
    );

    // Test the must-be-directory flag.
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::MUST_BE_DIRECTORY),
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
}

#[fuchsia::test]
async fn test_set_attr_and_set_flags_on_node() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file("file", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    assert_eq!(
        zx::Status::ok(
            proxy
                .set_attr(
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
        zx::Status::ok(proxy.set_flags(fio::OpenFlags::APPEND).await.expect("set_flags failed")),
        Err(zx::Status::BAD_HANDLE)
    );
}

#[fuchsia::test]
async fn test_node_clone() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file("file", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            "file",
            &fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            server.into_channel(),
        )
        .unwrap();

    let (proxy2, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    proxy.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server).expect("clone failed");

    assert_matches!(
        proxy2.get_connection_info().await.expect("get_connection_info failed"),
        fio::ConnectionInfo { rights: Some(fio::Operations::GET_ATTRIBUTES), .. }
    );
}

#[fuchsia::test]
async fn test_open_node_with_attributes() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (proxy, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    test_dir
        .open2(
            ".",
            &fio::ConnectionProtocols::Node(fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
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
        Some(fio::Representation::Connector(fio::ConnectorInfo {
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
