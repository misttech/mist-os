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
                .protocol_default("foo")
                .dictionary_default("parent_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict"),
                )
                .use_(UseBuilder::protocol().name("B").from_dictionary("parent_dict"))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("leaf".into()))
                        .name("C")
                        .from_dictionary("child_dict"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
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
async fn use_protocol_from_dictionary_not_found() {
    // Test extracting a protocol from a dictionary, but the protocol is missing from the
    // dictionary.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "dict".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").from_dictionary("dict"))
                .child_default("leaf")
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;

    // Test extracting a protocol from a dictionary, but the dictionary is not routed to the
    // target.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "other_dict".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").from_dictionary("dict"))
                .child_default("leaf")
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

#[fuchsia::test]
async fn use_directory_from_dictionary_not_supported() {
    // Routing a directory into a dictionary isn't supported yet, it should fail.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("bar_data").path("/data/bar"))
                .dictionary_default("parent_dict")
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
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .dictionary_default("self_dict")
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
                .use_(
                    UseBuilder::directory()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict")
                        .path("/A"),
                )
                .use_(UseBuilder::directory().name("B").from_dictionary("parent_dict").path("/B"))
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
                .use_(
                    UseBuilder::directory()
                        .source(UseSource::Child("mid".into()))
                        .name("A")
                        .path("/A"),
                )
                .use_(
                    UseBuilder::directory()
                        .source(UseSource::Child("mid".into()))
                        .name("B")
                        .path("/B"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .dictionary_default("self_dict")
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
                .expose(
                    ExposeBuilder::directory()
                        .name("A")
                        .source(ExposeSource::Self_)
                        .from_dictionary("self_dict"),
                )
                .expose(
                    ExposeBuilder::directory()
                        .name("B")
                        .source(ExposeSource::Child("leaf".into()))
                        .from_dictionary("child_dict"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("bar_data").path("/data/bar"))
                .dictionary_default("child_dict")
                .expose(
                    ExposeBuilder::dictionary()
                        .name("child_dict")
                        .source(ExposeSource::Self_)
                        .availability(Availability::Required),
                )
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
                .protocol_default("foo")
                .dictionary_default("nested")
                .dictionary_default("parent_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("nested")
                .dictionary_default("self_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict/nested"),
                )
                .use_(UseBuilder::protocol().name("B").from_dictionary("parent_dict/nested"))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("leaf".into()))
                        .name("C")
                        .from_dictionary("child_dict/nested"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("nested")
                .dictionary_default("child_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
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
                .protocol_default("foo")
                .dictionary_default("parent_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
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
                    source_name: "foo".parse().unwrap(),
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
                .child_default("provider")
                .child_default("leaf")
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A_svc").path("/svc/A"))
                .use_(UseBuilder::protocol().name("B_svc").path("/svc/B"))
                .use_(UseBuilder::protocol().name("C_svc").path("/svc/C"))
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
async fn offer_protocol_from_dictionary_not_found() {
    // Test extracting a protocol from a dictionary, but the protocol is missing from the
    // dictionary.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("dict".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A_svc").path("/svc/A"))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "mid/leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

#[fuchsia::test]
async fn offer_protocol_from_nested_dictionary() {
    // Test extracting a protocol from a dictionary nested in another dictionary with source
    // parent, self, and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("nested")
                .dictionary_default("parent_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .dictionary_default("nested")
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
                    source_name: "foo".parse().unwrap(),
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
                .child_default("provider")
                .child_default("leaf")
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .dictionary_default("nested")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A_svc").path("/svc/A"))
                .use_(UseBuilder::protocol().name("B_svc").path("/svc/B"))
                .use_(UseBuilder::protocol().name("C_svc").path("/svc/C"))
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
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("mid".into()))
                        .name("A_svc")
                        .path("/svc/A"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("mid".into()))
                        .name("B_svc")
                        .path("/svc/B"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .expose(
                    ExposeBuilder::protocol()
                        .name("A")
                        .target_name("A_svc")
                        .from_dictionary("self_dict")
                        .source(ExposeSource::Self_),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("B")
                        .target_name("B_svc")
                        .from_dictionary("child_dict")
                        .source(ExposeSource::Child("leaf".into())),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
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
async fn expose_protocol_from_dictionary_not_found() {
    // Test extracting a protocol from a dictionary, but the protocol is missing.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("mid".into()))
                        .name("A_svc")
                        .path("/svc/A"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .expose(
                    ExposeBuilder::protocol()
                        .name("A")
                        .target_name("A_svc")
                        .source(ExposeSource::Self_)
                        .from_dictionary("dict")
                        .build(),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("dict").source(ExposeSource::Self_))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

#[fuchsia::test]
async fn expose_protocol_from_nested_dictionary() {
    // Test extracting a protocol from a dictionary nested in a dictionary with source self and
    // child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("mid".into()))
                        .name("A_svc")
                        .path("/svc/A"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("mid".into()))
                        .name("B_svc")
                        .path("/svc/B"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .dictionary_default("nested")
                .expose(
                    ExposeBuilder::protocol()
                        .name("A")
                        .target_name("A_svc")
                        .from_dictionary("self_dict/nested")
                        .source(ExposeSource::Self_),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("B")
                        .target_name("B_svc")
                        .from_dictionary("child_dict/nested")
                        .source(ExposeSource::Child("leaf".into())),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .dictionary_default("nested")
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
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
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
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .expose(ExposeBuilder::dictionary().name("self_dict").source(ExposeSource::Self_))
                .expose(
                    ExposeBuilder::dictionary()
                        .name("child_dict")
                        .source(ExposeSource::Child("leaf".into())),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("nested")
                .dictionary_default("child_dict")
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
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
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
                .protocol_default("foo")
                .dictionary_default("parent_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .dictionary_default("root_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
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
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("root_dict/self_dict"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("B")
                        .from_dictionary("root_dict/parent_dict"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("C")
                        .from_dictionary("root_dict/child_dict"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
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
                .protocol_default("bar")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "bar".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("origin_dict")
                .capability(
                    CapabilityBuilder::dictionary()
                        .name("self_dict")
                        .source_dictionary(DictionarySource::Self_, "origin_dict")
                        .build(),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("origin_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("B")
                        .from_dictionary("self_dict"),
                )
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
                .protocol_default("bar")
                .dictionary_default("origin_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
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
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .capability(
                    CapabilityBuilder::dictionary()
                        .name("self_dict")
                        .source_dictionary(DictionarySource::Parent, "origin_dict")
                        .build(),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("B")
                        .from_dictionary("self_dict"),
                )
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
                .protocol_default("foo")
                .capability(
                    CapabilityBuilder::dictionary()
                        .name("self_dict")
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
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict"),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("B")
                        .from_dictionary("self_dict"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("bar")
                .dictionary_default("origin_dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("origin_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeBuilder::dictionary().name("origin_dict").source(ExposeSource::Self_))
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

#[fuchsia::test]
async fn use_from_dictionary_availability_attenuated() {
    // required -> optional downgrade allowed, of:
    // - a capability in a dictionary
    // - a capability in a dictionary in a dictionary
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .dictionary_default("nested")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
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
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "dict".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .name("A")
                        .from_dictionary("dict")
                        .availability(Availability::Optional),
                )
                .use_(
                    UseBuilder::protocol()
                        .name("B")
                        .from_dictionary("dict/nested")
                        .availability(Availability::Optional),
                )
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/A".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/B".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

#[fuchsia::test]
async fn use_from_dictionary_availability_invalid() {
    // attempted optional -> required upgrade, disallowed, of:
    // - an optional capability in a dictionary.
    // - a capability in an optional dictionary.
    // - a capability in a dictionary in an optional dictionary.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .protocol_default("qux")
                .dictionary_default("required_dict")
                .dictionary_default("optional_dict")
                .dictionary_default("nested")
                .dictionary_default("dict_with_optional_nested")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("optional_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "qux".parse().unwrap(),
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
                    target: OfferTarget::Capability("dict_with_optional_nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "required_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "required_dict".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "optional_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "optional_dict".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "dict_with_optional_nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "dict_with_optional_nested".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").from_dictionary("required_dict"))
                .use_(UseBuilder::protocol().name("B").from_dictionary("optional_dict"))
                .use_(
                    UseBuilder::protocol()
                        .name("C")
                        .from_dictionary("dict_with_optional_nested/nested"),
                )
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/B".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/C".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

#[fuchsia::test]
async fn offer_from_dictionary_availability_attenuated() {
    // required -> optional downgrade allowed, of:
    // - a capability in a dictionary
    // - a capability in a dictionary in a dictionary
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .dictionary_default("nested")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
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
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("dict".parse().unwrap()),
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("dict/nested".parse().unwrap()),
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").availability(Availability::Optional))
                .use_(UseBuilder::protocol().name("B").availability(Availability::Optional))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/A".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/B".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

#[fuchsia::test]
async fn offer_from_dictionary_availability_invalid() {
    // attempted optional -> required upgrade, disallowed, of:
    // - an optional capability in a dictionary.
    // - a capability in an optional dictionary.
    // - a capability in a dictionary in an optional dictionary.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .protocol_default("qux")
                .dictionary_default("required_dict")
                .dictionary_default("optional_dict")
                .dictionary_default("nested")
                .dictionary_default("dict_with_optional_nested")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("optional_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "qux".parse().unwrap(),
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
                    target: OfferTarget::Capability("dict_with_optional_nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "required_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "required_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "optional_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "optional_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "dict_with_optional_nested".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "dict_with_optional_nested".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: OfferSource::Parent,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("required_dict".parse().unwrap()),
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: OfferSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("optional_dict".parse().unwrap()),
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: OfferSource::Parent,
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("dict_with_optional_nested/nested".parse().unwrap()),
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    availability: Availability::Required,
                }))
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A"))
                .use_(UseBuilder::protocol().name("B"))
                .use_(UseBuilder::protocol().name("C"))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "mid/leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
    test.check_use(
        "mid/leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/B".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
    test.check_use(
        "mid/leaf".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/C".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

#[fuchsia::test]
async fn expose_from_dictionary_availability_attenuated() {
    // required -> optional downgrade allowed, of:
    // - a capability in a dictionary
    // - a capability in a dictionary in a dictionary
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("leaf".into()))
                        .name("A")
                        .availability(Availability::Optional),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Child("leaf".into()))
                        .name("B")
                        .availability(Availability::Optional),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .dictionary_default("nested")
                .dictionary_default("dict")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
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
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(
                    ExposeBuilder::protocol()
                        .name("A")
                        .from_dictionary("dict")
                        .source(ExposeSource::Self_)
                        .availability(Availability::Required),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("B")
                        .from_dictionary("dict/nested")
                        .source(ExposeSource::Self_)
                        .availability(Availability::Required),
                )
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/A".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/B".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

#[fuchsia::test]
async fn expose_from_dictionary_availability_invalid() {
    // attempted optional -> required upgrade, disallowed, of:
    // - an optional capability in a dictionary.
    // - a capability in an optional dictionary.
    // - a capability in a dictionary in an optional dictionary.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().source(UseSource::Child("mid".into())).name("A"))
                .use_(UseBuilder::protocol().source(UseSource::Child("mid".into())).name("B"))
                .use_(UseBuilder::protocol().source(UseSource::Child("mid".into())).name("C"))
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .expose(
                    ExposeBuilder::protocol()
                        .name("A")
                        .from_dictionary("required_dict")
                        .source(ExposeSource::Child("leaf".into()))
                        .availability(Availability::Required),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("B")
                        .from_dictionary("optional_dict")
                        .source(ExposeSource::Child("leaf".into()))
                        .availability(Availability::Required),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("C")
                        .from_dictionary("dict_with_optional_nested/nested")
                        .source(ExposeSource::Child("leaf".into()))
                        .availability(Availability::Required),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .protocol_default("qux")
                .dictionary_default("required_dict")
                .dictionary_default("optional_dict")
                .dictionary_default("nested")
                .dictionary_default("dict_with_optional_nested")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("optional_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "qux".parse().unwrap(),
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
                    target: OfferTarget::Capability("dict_with_optional_nested".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                }))
                .expose(
                    ExposeBuilder::dictionary()
                        .name("required_dict")
                        .source(ExposeSource::Self_)
                        .availability(Availability::Required),
                )
                .expose(
                    ExposeBuilder::dictionary()
                        .name("optional_dict")
                        .source(ExposeSource::Self_)
                        .availability(Availability::Optional),
                )
                .expose(
                    ExposeBuilder::dictionary()
                        .name("dict_with_optional_nested")
                        .source(ExposeSource::Self_)
                        .availability(Availability::Required),
                )
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/A".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/B".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
    test.check_use(
        ".".try_into().unwrap(),
        CheckUse::Protocol {
            path: "/svc/C".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}
