// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::testing::routing_test_helpers::*, cm_rust::*, cm_rust_testing::*,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx, routing_test_helpers::RoutingTestModel,
    std::path::PathBuf,
};

#[fuchsia::test]
async fn use_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source parent, self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("leaf".into()),
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            "mid".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn use_directory_from_dictionary_not_supported() {
    // Routing a directory into a dictionary isn't supported yet, it should fail.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("bar_data").path("/data/bar").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("leaf".into()),
                    target_name: "parent_dict".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source: OfferSource::Self_,
                    source_name: "bar_data".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("foo_data").path("/data/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_data".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/A".parse().unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict".parse().unwrap()),
                    target_path: "/B".parse().unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Directory {
            path: "/A".parse().unwrap(),
            file: PathBuf::from("hippo"),
            expected_res: ExpectedResult::Err(zx::Status::NOT_SUPPORTED),
        },
    )
    .await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Directory {
            path: "/B".parse().unwrap(),
            file: PathBuf::from("hippo"),
            expected_res: ExpectedResult::Err(zx::Status::NOT_SUPPORTED),
        },
    )
    .await;
}

#[fuchsia::test]
async fn expose_directory_from_dictionary_not_supported() {
    // Routing a directory into a dictionary isn't supported yet, it should fail.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "A".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/A".parse().unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "B".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/B".parse().unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("foo_data").path("/data/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_data".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                    source: ExposeSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_name: "A".parse().unwrap(),
                    rights: None,
                    subdir: None,
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                    source: ExposeSource::Child("leaf".into()),
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_name: "B".parse().unwrap(),
                    rights: None,
                    subdir: None,
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("bar_data").path("/data/bar").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target: ExposeTarget::Parent,
                    target_name: "child_dict".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source: OfferSource::Self_,
                    source_name: "bar_data".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Directory {
            path: "/A".parse().unwrap(),
            file: PathBuf::from("hippo"),
            expected_res: ExpectedResult::Err(zx::Status::NOT_SUPPORTED),
        },
    )
    .await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Directory {
            path: "/B".parse().unwrap(),
            file: PathBuf::from("hippo"),
            expected_res: ExpectedResult::Err(zx::Status::NOT_SUPPORTED),
        },
    )
    .await;
}

#[fuchsia::test]
async fn use_protocol_from_nested_dictionary() {
    // Test extracting a protocol from a dictionary nested in another dictionary with source
    // parent, self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict/nested".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict/nested".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("leaf".into()),
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("child_dict/nested".parse().unwrap()),
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            "mid".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn offer_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source parent, self, and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict".parse().unwrap()),
                    target_name: "B_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("provider".into()),
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_name: "C_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("provider".into()))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "A_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "C_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            "mid/leaf".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn offer_protocol_from_nested_dictionary() {
    // Test extracting a protocol from a dictionary nested in another dictionary with source
    // parent, self, and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict/nested".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict/nested".parse().unwrap()),
                    target_name: "B_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("provider".into()),
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("child_dict/nested".parse().unwrap()),
                    target_name: "C_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("provider".into()))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "A_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "C_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            "mid/leaf".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn expose_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "A_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "B_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Child("leaf".into()),
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_name: "B_svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B"] {
        test.check_use(
            ".".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn expose_protocol_from_nested_dictionary() {
    // Test extracting a protocol from a dictionary nested in a dictionary with source self and
    // child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "A_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "B_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict/nested".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Child("leaf".into()),
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("child_dict/nested".parse().unwrap()),
                    target_name: "B_svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B"] {
        test.check_use(
            ".".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn dictionary_in_exposed_dir() {
    // Test extracting a protocol from a dictionary with source self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "self_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "self_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Child("leaf".into()),
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("nested").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "nested".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    // The dictionaries in the exposed dir will be converted to subdirectories.
    for path in ["/self_dict/A", "/child_dict/nested/B"] {
        test.check_use_exposed_dir(
            ".".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn offer_dictionary_to_dictionary() {
    // Tests dictionary nesting when the nested dictionary comes from parent, self, or child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .dictionary(DictionaryDeclBuilder::new("root_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "self_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "self_dict".parse().unwrap(),
                    target: OfferTarget::Capability("root_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Parent,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::Capability("root_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::static_child("leaf".into()),
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: OfferTarget::Capability("root_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("root_dict/self_dict".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("root_dict/parent_dict".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("root_dict/child_dict".parse().unwrap()),
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            "mid".try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn extend_from_self() {
    // Tests extending a dictionary, when the source dictionary is defined by self.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("bar_svc").path("/svc/foo").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "bar_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("origin_dict").build())
                .dictionary(
                    DictionaryDeclBuilder::new("self_dict")
                        .source_dictionary(DictionarySource::Self_, "origin_dict")
                        .build(),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "bar_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("origin_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    // Test using twice in a row
    for _ in 0..2 {
        for path in ["/svc/A", "/svc/B"] {
            test.check_use(
                "leaf".try_into().unwrap(),
                CheckUse::Protocol {
                    path: path.parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        }
    }
}

#[fuchsia::test]
async fn extend_from_parent() {
    // Tests extending a dictionary, when the source dictionary is defined by parent.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("bar_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("origin_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("origin_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "origin_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "origin_dict".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(
                    DictionaryDeclBuilder::new("self_dict")
                        .source_dictionary(DictionarySource::Parent, "origin_dict")
                        .build(),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    // Test using twice in a row
    for _ in 0..2 {
        for path in ["/svc/A", "/svc/B"] {
            test.check_use(
                "leaf".try_into().unwrap(),
                CheckUse::Protocol {
                    path: path.parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        }
    }
}

#[fuchsia::test]
async fn extend_from_child() {
    // Tests extending a dictionary, when the source dictionary is defined by a child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(
                    DictionaryDeclBuilder::new("self_dict")
                        .source_dictionary(
                            DictionarySource::Child(ChildRef {
                                name: "leaf".into(),
                                collection: None,
                            }),
                            "origin_dict",
                        )
                        .build(),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("bar_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("origin_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("origin_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "origin_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "origin_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    // Test using twice in a row
    for _ in 0..2 {
        for path in ["/svc/A", "/svc/B"] {
            test.check_use(
                ".".try_into().unwrap(),
                CheckUse::Protocol {
                    path: path.parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        }
    }
}
