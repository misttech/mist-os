// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    io_conformance_util::{test_harness::TestHarness, *},
};

#[fuchsia::test]
async fn test_open_node_on_directory() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (_proxy, on_representation) = test_dir
        .open2_node_get_representation::<fio::NodeMarker>(
            ".",
            fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_matches!(on_representation, fio::Representation::Connector(fio::ConnectorInfo { .. }));
}

#[fuchsia::test]
async fn test_open_node_on_file() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file("file", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let (_proxy, representation) = test_dir
        .open2_node_get_representation::<fio::NodeMarker>(
            "file",
            fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_matches!(representation, fio::Representation::Connector(fio::ConnectorInfo { .. }));

    // Test the must-be-directory flag which should result in an error.
    let error: zx::Status = test_dir
        .open2_node::<fio::NodeMarker>(
            "file",
            fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::MUST_BE_DIRECTORY),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error, zx::Status::NOT_DIR);
}

#[fuchsia::test]
async fn test_set_attr_and_set_flags_on_node() {
    let harness = TestHarness::new().await;

    if !harness.config.supports_open2.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file("file", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());

    let proxy = test_dir
        .open2_node::<fio::NodeMarker>(
            "file",
            fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
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

    let proxy = test_dir
        .open2_node::<fio::NodeMarker>(
            "file",
            fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
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

    let (_proxy, representation) = test_dir
        .open2_node_get_representation::<fio::NodeMarker>(
            ".",
            fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                protocols: Some(fio::NodeProtocols {
                    node: Some(fio::NodeProtocolFlags::default()),
                    ..Default::default()
                }),
                attributes: Some(
                    fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::ABILITIES,
                ),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_matches!(representation,
        fio::Representation::Connector(fio::ConnectorInfo {
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

// TODO(https://fxbug.dev/293947862): Add tests for fuchsia.io/Node.Reopen.
