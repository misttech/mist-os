// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn test_open_node_on_directory() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags_deprecated());

    let (_proxy, on_representation) = dir
        .open3_node_repr::<fio::NodeMarker>(
            ".",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_NODE,
            None,
        )
        .await
        .unwrap();
    assert_matches!(on_representation, fio::Representation::Connector(fio::ConnectorInfo { .. }));

    // If other protocol types are specified, the target must match at least one.
    let error: zx::Status = dir
        .open3_node::<fio::NodeMarker>(
            ".",
            fio::Flags::PROTOCOL_NODE | fio::Flags::PROTOCOL_FILE,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error, zx::Status::NOT_FILE);
}

#[fuchsia::test]
async fn test_open_node_on_file() {
    let harness = TestHarness::new().await;

    let entries = vec![file("file", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    let (_proxy, representation) = dir
        .open3_node_repr::<fio::NodeMarker>(
            "file",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_NODE,
            None,
        )
        .await
        .unwrap();
    assert_matches!(representation, fio::Representation::Connector(fio::ConnectorInfo { .. }));

    // If other protocol types are specified, the target must match at least one.
    let error: zx::Status = dir
        .open3_node::<fio::NodeMarker>(
            "file",
            fio::Flags::PROTOCOL_NODE | fio::Flags::PROTOCOL_DIRECTORY,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error, zx::Status::NOT_DIR);

    let error: zx::Status = dir
        .open3_node::<fio::NodeMarker>(
            "file",
            fio::Flags::PROTOCOL_NODE | fio::Flags::PROTOCOL_SYMLINK,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error, zx::Status::WRONG_TYPE);
}

#[fuchsia::test]
async fn test_set_attr_and_set_flags_on_node() {
    let harness = TestHarness::new().await;
    let entries = vec![file("file", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    let proxy =
        dir.open3_node::<fio::NodeMarker>("file", fio::Flags::PROTOCOL_NODE, None).await.unwrap();

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
    let entries = vec![file("file", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags_deprecated());

    let proxy = dir
        .open3_node::<fio::NodeMarker>(
            "file",
            fio::Flags::PROTOCOL_NODE | fio::Flags::PERM_GET_ATTRIBUTES,
            None,
        )
        .await
        .unwrap();

    let (cloned, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
    proxy.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server).expect("clone failed");

    assert_matches!(
        cloned.get_connection_info().await.expect("get_connection_info failed"),
        fio::ConnectionInfo { rights: Some(fio::Rights::GET_ATTRIBUTES), .. }
    );
}

#[fuchsia::test]
async fn test_open_node_with_attributes() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags_deprecated());

    let (_proxy, representation) = dir
        .open3_node_repr::<fio::NodeMarker>(
            ".",
            fio::Flags::FLAG_SEND_REPRESENTATION | fio::Flags::PROTOCOL_NODE,
            Some(fio::Options {
                attributes: Some(
                    fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::ABILITIES,
                ),
                ..Default::default()
            }),
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
