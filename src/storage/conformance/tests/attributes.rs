// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn set_attr_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for file_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![file("file", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = open_file_with_flags(&dir, file_flags, "file").await;

        let (status, old_attr) = file.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        // Set CREATION_TIME flag, but not MODIFICATION_TIME.
        let status = file
            .set_attr(
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

        let (status, new_attr) = file.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        // Check that only creation_time was updated.
        let expected = fio::NodeAttributes { creation_time: 111, ..old_attr };
        assert_eq!(new_attr, expected);
    }
}

#[fuchsia::test]
async fn set_attr_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for file_flags in harness.file_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![file("file", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let file = open_file_with_flags(&dir, file_flags, "file").await;

        let status = file
            .set_attr(
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
async fn set_attr_directory_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for dir_flags in
        harness.file_rights.combinations_containing_deprecated(fio::Rights::WRITE_BYTES)
    {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = open_dir_with_flags(&dir, dir_flags, "dir").await;

        let (status, old_attr) = dir.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        // Set CREATION_TIME flag, but not MODIFICATION_TIME.
        let status = dir
            .set_attr(
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

        let (status, new_attr) = dir.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        // Check that only creation_time was updated.
        let expected = fio::NodeAttributes { creation_time: 111, ..old_attr };
        assert_eq!(new_attr, expected);
    }
}

#[fuchsia::test]
async fn set_attr_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    for dir_flags in harness.file_rights.combinations_without_deprecated(fio::Rights::WRITE_BYTES) {
        let entries = vec![directory("dir", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
        let dir = open_dir_with_flags(&dir, dir_flags, "dir").await;

        let status = dir
            .set_attr(
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
async fn get_attributes_query_none() {
    let harness = TestHarness::new().await;
    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    // Node attributes that were not requested should return None
    let attributes = file_proxy
        .get_attributes(fio::NodeAttributesQuery::empty())
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(attributes, Default::default());
}

#[fuchsia::test]
async fn get_attributes_file_query_all() {
    let harness = TestHarness::new().await;
    let supported_attrs = harness.config.supported_attributes;
    const FILE_CONTENTS: &'static [u8] = b"test-file-contents";

    let entries = vec![file(TEST_FILE, FILE_CONTENTS.to_owned())];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    // All of the attributes are requested. Filesystems are allowed to return None for attributes
    // they don't support.
    let (mutable_attrs, immutable_attrs) = file_proxy
        .get_attributes(fio::NodeAttributesQuery::all())
        .await
        .unwrap()
        .expect("get_attributes failed");

    // If ctime and mtime are supported then they shouldn't be 0.
    if supported_attrs.contains(fio::NodeAttributesQuery::CREATION_TIME) {
        assert_matches!(mutable_attrs.creation_time, Some(1..));
    } else {
        assert_matches!(mutable_attrs.creation_time, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::MODIFICATION_TIME) {
        assert_matches!(mutable_attrs.modification_time, Some(1..));
    } else {
        assert_matches!(mutable_attrs.modification_time, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::ACCESS_TIME) {
        assert_matches!(mutable_attrs.access_time, Some(1..));
    } else {
        assert_matches!(mutable_attrs.access_time, None);
    }

    // The posix attributes weren't set so they should all be None.
    assert_matches!(mutable_attrs.mode, None);
    assert_matches!(mutable_attrs.uid, None);
    assert_matches!(mutable_attrs.gid, None);
    assert_matches!(mutable_attrs.rdev, None);

    // All node types must report at least protocols and abilities.
    assert_matches!(immutable_attrs.protocols, Some(fio::NodeProtocolKinds::FILE));
    assert!(immutable_attrs.abilities.is_some());

    // Other attributes have conditional support.
    if supported_attrs.contains(fio::NodeAttributesQuery::CONTENT_SIZE) {
        assert_matches!(immutable_attrs.content_size, Some(x) if x == FILE_CONTENTS.len() as u64);
    } else {
        assert_matches!(immutable_attrs.content_size, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::STORAGE_SIZE) {
        assert_matches!(immutable_attrs.storage_size, Some(..));
    } else {
        assert_matches!(immutable_attrs.storage_size, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::LINK_COUNT) {
        assert_matches!(immutable_attrs.link_count, Some(..));
    } else {
        assert_matches!(immutable_attrs.link_count, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::ID) {
        assert_matches!(immutable_attrs.id, Some(..));
    } else {
        assert_matches!(immutable_attrs.id, None);
    }
}

#[fuchsia::test]
async fn get_attributes_directory_query_all() {
    let harness = TestHarness::new().await;
    let supported_attrs = harness.config.supported_attributes;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    // fuchsia.io/Node.GetAttributes
    // All of the attributes are requested. Filesystems are allowed to return None for attributes
    // they don't support.
    let (mutable_attrs, immutable_attrs) = dir_proxy
        .get_attributes(fio::NodeAttributesQuery::all())
        .await
        .unwrap()
        .expect("get_attributes failed");

    // If timestamps are supported then they shouldn't be 0.
    if supported_attrs.contains(fio::NodeAttributesQuery::CREATION_TIME) {
        assert_matches!(mutable_attrs.creation_time, Some(1..));
    } else {
        assert_matches!(mutable_attrs.creation_time, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::MODIFICATION_TIME) {
        assert_matches!(mutable_attrs.modification_time, Some(1..));
    } else {
        assert_matches!(mutable_attrs.modification_time, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::ACCESS_TIME) {
        assert_matches!(mutable_attrs.access_time, Some(1..));
    } else {
        assert_matches!(mutable_attrs.access_time, None);
    }

    // The posix attributes weren't set so they should all be None.
    assert_matches!(mutable_attrs.mode, None);
    assert_matches!(mutable_attrs.uid, None);
    assert_matches!(mutable_attrs.gid, None);
    assert_matches!(mutable_attrs.rdev, None);

    // All node types must report at least protocols and abilities.
    assert_matches!(immutable_attrs.protocols, Some(fio::NodeProtocolKinds::DIRECTORY));
    assert!(immutable_attrs.abilities.is_some());

    // Other attributes have conditional support.
    if supported_attrs.contains(fio::NodeAttributesQuery::LINK_COUNT) {
        assert_matches!(immutable_attrs.link_count, Some(..));
    } else {
        assert_matches!(immutable_attrs.link_count, None);
    }
    if supported_attrs.contains(fio::NodeAttributesQuery::ID) {
        assert_matches!(immutable_attrs.id, Some(..));
    } else {
        assert_matches!(immutable_attrs.id, None);
    }
}

#[fuchsia::test]
async fn update_attributes_file_unsupported() {
    let harness = TestHarness::new().await;
    if harness.supports_mutable_attrs() || !harness.config.supports_mutable_file {
        return;
    }
    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;
    // fuchsia.io/Node.UpdateAttributes
    assert_eq!(
        file_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn update_attributes_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    let entries = vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    let status = file_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(111),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw);
    assert_eq!(status, Err(zx::Status::BAD_HANDLE));
}

#[fuchsia::test]
async fn update_attributes_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }
    let supported_attrs = harness.config.supported_attributes;

    let entries = vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;

    let new_attrs = fio::MutableNodeAttributes {
        creation_time: supported_attrs
            .contains(fio::NodeAttributesQuery::CREATION_TIME)
            .then_some(111),
        modification_time: supported_attrs
            .contains(fio::NodeAttributesQuery::MODIFICATION_TIME)
            .then_some(222),
        mode: supported_attrs.contains(fio::NodeAttributesQuery::MODE).then_some(333),
        uid: supported_attrs.contains(fio::NodeAttributesQuery::UID).then_some(444),
        gid: supported_attrs.contains(fio::NodeAttributesQuery::GID).then_some(555),
        rdev: supported_attrs.contains(fio::NodeAttributesQuery::RDEV).then_some(666),
        access_time: supported_attrs.contains(fio::NodeAttributesQuery::ACCESS_TIME).then_some(777),
        selinux_context: supported_attrs
            .contains(fio::NodeAttributesQuery::SELINUX_CONTEXT)
            .then_some(fio::SelinuxContext::Data(vec![7u8; 10])),
        ..Default::default()
    };

    let _ = file_proxy
        .update_attributes(&new_attrs)
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("update_attributes failed");

    let (mutable_attrs, _) = file_proxy
        .get_attributes(supported_attrs)
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(mutable_attrs, new_attrs);

    // Test that we should not be able to update non-supported attributes
    let unsupported_new_attrs = fio::MutableNodeAttributes {
        creation_time: (!supported_attrs.contains(fio::NodeAttributesQuery::CREATION_TIME))
            .then_some(111),
        modification_time: (!supported_attrs.contains(fio::NodeAttributesQuery::MODIFICATION_TIME))
            .then_some(222),
        mode: (!supported_attrs.contains(fio::NodeAttributesQuery::MODE)).then_some(333),
        uid: (!supported_attrs.contains(fio::NodeAttributesQuery::UID)).then_some(444),
        gid: (!supported_attrs.contains(fio::NodeAttributesQuery::GID)).then_some(555),
        rdev: (!supported_attrs.contains(fio::NodeAttributesQuery::RDEV)).then_some(666),
        access_time: (!supported_attrs.contains(fio::NodeAttributesQuery::ACCESS_TIME))
            .then_some(777),
        ..Default::default()
    };
    if unsupported_new_attrs != fio::MutableNodeAttributes::default() {
        let status = file_proxy
            .update_attributes(&unsupported_new_attrs)
            .await
            .expect("FIDL call failed")
            .map_err(zx::Status::from_raw)
            .expect_err("update unsupported attributes passed");
        assert_eq!(status, zx::Status::NOT_SUPPORTED);
    }
}

#[fuchsia::test]
async fn get_attributes_file_node_reference() {
    let harness = TestHarness::new().await;
    let entries = vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::NODE_REFERENCE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    let (_mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(fio::NodeAttributesQuery::PROTOCOLS)
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::FILE);
}

#[fuchsia::test]
async fn update_attributes_file_node_reference_not_allowed() {
    let harness = TestHarness::new().await;
    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let file_proxy = open_file_with_flags(&dir, fio::OpenFlags::NODE_REFERENCE, TEST_FILE).await;

    // Node references does not support fuchsia.io/Node.UpdateAttributes
    assert_eq!(
        file_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::BAD_HANDLE.into_raw())
    );
}

#[fuchsia::test]
async fn get_attributes_directory() {
    let harness = TestHarness::new().await;
    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    let (_mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(fio::NodeAttributesQuery::PROTOCOLS)
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::DIRECTORY);
}

#[fuchsia::test]
async fn update_attributes_directory_unsupported() {
    let harness = TestHarness::new().await;
    if harness.supports_mutable_attrs() {
        return;
    }

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE, "dir").await;

    // fuchsia.io/Node.UpdateAttributes
    assert_eq!(
        dir_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn update_attributes_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    let status = dir_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(111),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw);
    assert_eq!(status, Err(zx::Status::BAD_HANDLE));
}

#[fuchsia::test]
async fn update_attributes_directory_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.supports_mutable_attrs() {
        return;
    }
    let supported_attrs = harness.config.supported_attributes;

    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE, "dir").await;

    let new_attrs = fio::MutableNodeAttributes {
        creation_time: supported_attrs
            .contains(fio::NodeAttributesQuery::CREATION_TIME)
            .then_some(111),
        modification_time: supported_attrs
            .contains(fio::NodeAttributesQuery::MODIFICATION_TIME)
            .then_some(222),
        mode: supported_attrs.contains(fio::NodeAttributesQuery::MODE).then_some(333),
        uid: supported_attrs.contains(fio::NodeAttributesQuery::UID).then_some(444),
        gid: supported_attrs.contains(fio::NodeAttributesQuery::GID).then_some(555),
        rdev: supported_attrs.contains(fio::NodeAttributesQuery::RDEV).then_some(666),
        access_time: supported_attrs.contains(fio::NodeAttributesQuery::ACCESS_TIME).then_some(777),
        casefold: supported_attrs.contains(fio::NodeAttributesQuery::CASEFOLD).then_some(false),
        selinux_context: supported_attrs
            .contains(fio::NodeAttributesQuery::SELINUX_CONTEXT)
            .then_some(fio::SelinuxContext::Data(vec![7u8; 10])),
        ..Default::default()
    };

    let _ = dir_proxy
        .update_attributes(&new_attrs)
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("update_attributes failed");

    let (mutable_attrs, _) = dir_proxy
        .get_attributes(fio::NodeAttributesQuery::all())
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(mutable_attrs, new_attrs);

    // Test that we should not be able to update non-supported attributes
    let unsupported_new_attrs = fio::MutableNodeAttributes {
        creation_time: (!supported_attrs.contains(fio::NodeAttributesQuery::CREATION_TIME))
            .then_some(111),
        modification_time: (!supported_attrs.contains(fio::NodeAttributesQuery::MODIFICATION_TIME))
            .then_some(222),
        mode: (!supported_attrs.contains(fio::NodeAttributesQuery::MODE)).then_some(333),
        uid: (!supported_attrs.contains(fio::NodeAttributesQuery::UID)).then_some(444),
        gid: (!supported_attrs.contains(fio::NodeAttributesQuery::GID)).then_some(555),
        rdev: (!supported_attrs.contains(fio::NodeAttributesQuery::RDEV)).then_some(666),
        access_time: (!supported_attrs.contains(fio::NodeAttributesQuery::ACCESS_TIME))
            .then_some(777),
        ..Default::default()
    };
    if unsupported_new_attrs != fio::MutableNodeAttributes::default() {
        let status = dir_proxy
            .update_attributes(&unsupported_new_attrs)
            .await
            .expect("FIDL call failed")
            .map_err(zx::Status::from_raw)
            .expect_err("update unsupported attributes passed");
        assert_eq!(status, zx::Status::NOT_SUPPORTED);
    }
}

#[fuchsia::test]
async fn get_attributes_directory_node_reference() {
    let harness = TestHarness::new().await;
    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::NODE_REFERENCE, "dir").await;

    // fuchsia.io/Node.GetAttributes
    let (_mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(fio::NodeAttributesQuery::PROTOCOLS)
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::DIRECTORY);
}

#[fuchsia::test]
async fn update_attributes_directory_node_reference_not_allowed() {
    let harness = TestHarness::new().await;
    let entries = vec![directory("dir", vec![])];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());
    let dir_proxy = open_dir_with_flags(&dir, fio::OpenFlags::NODE_REFERENCE, "dir").await;

    // Node reference doesn't allow for updating attributes
    assert_eq!(
        dir_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::BAD_HANDLE.into_raw())
    );
}
