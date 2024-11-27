// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability;
use crate::framework::capability_store::CapabilityStore;
use crate::model::testing::out_dir::OutDir;
use crate::model::testing::routing_test_helpers::*;
use cm_rust::*;
use cm_rust_testing::*;
use fidl::endpoints::{self, ServerEnd};
use fidl_fidl_examples_routing_echo::EchoMarker;
use futures::TryStreamExt;
use routing_test_helpers::RoutingTestModel;
use std::path::PathBuf;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync};

#[fuchsia::test]
async fn use_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source parent, self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("parent_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict"),
                )
                .use_(UseBuilder::protocol().name("B").from_dictionary("parent_dict"))
                .use_(
                    UseBuilder::protocol()
                        .source_static_child("leaf")
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
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
async fn use_protocol_from_dictionary_not_used() {
    // Create a dictionary with two protocols. `use` one of the protocols, but not the other.
    // Only the protocol that is `use`d should be accessible.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .protocol_default("bar")
                .dictionary_default("parent_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").from_dictionary("parent_dict"))
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
        CheckUse::Protocol {
            path: "/svc/B".parse().unwrap(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict")
                        .target_name("other_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
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
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
                .offer(
                    OfferBuilder::directory()
                        .name("bar_data")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap()))
                        .rights(fio::R_STAR_DIR),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .dictionary_default("self_dict")
                .offer(
                    OfferBuilder::directory()
                        .name("foo_data")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap()))
                        .rights(fio::R_STAR_DIR),
                )
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
                .use_(UseBuilder::directory().source_static_child("mid").name("A").path("/A"))
                .use_(UseBuilder::directory().source_static_child("mid").name("B").path("/B"))
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .dictionary_default("self_dict")
                .offer(
                    OfferBuilder::directory()
                        .name("foo_data")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap()))
                        .rights(fio::R_STAR_DIR),
                )
                .expose(
                    ExposeBuilder::directory()
                        .name("A")
                        .source(ExposeSource::Self_)
                        .from_dictionary("self_dict"),
                )
                .expose(
                    ExposeBuilder::directory()
                        .name("B")
                        .source_static_child("leaf")
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
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
                .offer(
                    OfferBuilder::directory()
                        .name("bar_data")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap()))
                        .rights(fio::R_STAR_DIR),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("nested")
                .dictionary_default("self_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Self_)
                        .name("A")
                        .from_dictionary("self_dict/nested"),
                )
                .use_(UseBuilder::protocol().name("B").from_dictionary("parent_dict/nested"))
                .use_(
                    UseBuilder::protocol()
                        .source_static_child("leaf")
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("A")
                        .target_name("A_svc")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf")
                        .from_dictionary("self_dict"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("B")
                        .target_name("B_svc")
                        .source(OfferSource::Parent)
                        .target_static_child("leaf")
                        .from_dictionary("parent_dict"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("C")
                        .target_name("C_svc")
                        .source_static_child("provider")
                        .target_static_child("leaf")
                        .from_dictionary("child_dict"),
                )
                .child_default("provider")
                .child_default("leaf")
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .offer(
                    OfferBuilder::protocol()
                        .name("A")
                        .target_name("A_svc")
                        .source(OfferSource::Parent)
                        .target_static_child("leaf")
                        .from_dictionary("dict"),
                )
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
async fn offer_protocol_from_dictionary_to_dictionary() {
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .dictionary_default("dict1")
                .dictionary_default("dict2")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source_static_child("provider")
                        .target(OfferTarget::Capability("dict1".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("A")
                        .target_name("A")
                        .from_dictionary("dict1")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict2".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict2")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
                .child_default("provider")
                .child_default("leaf")
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .expose(ExposeBuilder::protocol().name("foo").source(ExposeSource::Self_))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").from_dictionary("dict2").path("/svc/A"))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        "leaf".try_into().unwrap(),
        CheckUse::Protocol { path: "/svc/A".parse().unwrap(), expected_res: ExpectedResult::Ok },
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
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
                .offer(
                    OfferBuilder::protocol()
                        .name("A")
                        .target_name("A_svc")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf")
                        .from_dictionary("self_dict/nested"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("B")
                        .target_name("B_svc")
                        .source(OfferSource::Parent)
                        .target_static_child("leaf")
                        .from_dictionary("parent_dict/nested"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("C")
                        .target_name("C_svc")
                        .source_static_child("provider")
                        .target_static_child("leaf")
                        .from_dictionary("child_dict/nested"),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
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
                    UseBuilder::protocol().source_static_child("mid").name("A_svc").path("/svc/A"),
                )
                .use_(
                    UseBuilder::protocol().source_static_child("mid").name("B_svc").path("/svc/B"),
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
                        .source_static_child("leaf"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
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
                    UseBuilder::protocol().source_static_child("mid").name("A_svc").path("/svc/A"),
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
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
                    UseBuilder::protocol().source_static_child("mid").name("A_svc").path("/svc/A"),
                )
                .use_(
                    UseBuilder::protocol().source_static_child("mid").name("B_svc").path("/svc/B"),
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
                        .source_static_child("leaf"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .dictionary_default("nested")
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
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
                .expose(ExposeBuilder::dictionary().name("child_dict").source_static_child("leaf"))
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
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
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("parent_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("self_dict")
                .dictionary_default("root_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("self_dict")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("root_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("parent_dict")
                        .source(OfferSource::Parent)
                        .target(OfferTarget::Capability("root_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("child_dict")
                        .source_static_child("leaf")
                        .target(OfferTarget::Capability("root_dict".parse().unwrap())),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
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
async fn dictionary_from_program() {
    // Tests a dictionary that is backed by the program.

    const ROUTER_PATH: &str = "/svc/fuchsia.component.sandbox.DictionaryRouter";
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::dictionary().name("dict").path(ROUTER_PATH))
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
                .child_default("leaf")
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("A").from_dictionary("dict"))
                .build(),
        ),
    ];
    let test = RoutingTestBuilder::new("root", components).build().await;

    let host = CapabilityStore::new();
    let (store, server) = endpoints::create_proxy::<fsandbox::CapabilityStoreMarker>().unwrap();
    capability::open_framework(&host, test.model.root(), server.into()).await.unwrap();

    // Create a dictionary with a Sender at "A" for the Echo protocol.
    let dict_id = 1;
    store.dictionary_create(dict_id).await.unwrap().unwrap();
    let (receiver_client, mut receiver_stream) =
        endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
    let connector_id = 10;
    store.connector_create(connector_id, receiver_client).await.unwrap().unwrap();
    store
        .dictionary_insert(
            dict_id,
            &fsandbox::DictionaryItem { key: "A".into(), value: connector_id },
        )
        .await
        .unwrap()
        .unwrap();

    // Serve the Echo protocol from the Receiver.
    let _receiver_task = fasync::Task::spawn(async move {
        let mut task_group = fasync::TaskGroup::new();
        while let Ok(Some(request)) = receiver_stream.try_next().await {
            match request {
                fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                    let channel: ServerEnd<EchoMarker> = channel.into();
                    task_group.spawn(OutDir::echo_protocol_fn(channel.into_stream()));
                }
                fsandbox::ReceiverRequest::_UnknownMethod { .. } => {
                    unimplemented!()
                }
            }
        }
    });

    // Serve the Router protocol from the root's out dir. Its implementation calls Dictionary/Clone
    // and returns the handle.
    let mut root_out_dir = OutDir::new();
    let dict_store2 = store.clone();
    root_out_dir.add_entry(
        ROUTER_PATH.parse().unwrap(),
        vfs::service::endpoint(move |scope, channel| {
            let server_end: ServerEnd<fsandbox::DictionaryRouterMarker> =
                channel.into_zx_channel().into();
            let mut stream = server_end.into_stream();
            let store = dict_store2.clone();
            scope.spawn(async move {
                while let Ok(Some(request)) = stream.try_next().await {
                    match request {
                        fsandbox::DictionaryRouterRequest::Route { payload: _, responder } => {
                            let dup_dict_id = dict_id + 1;
                            store.duplicate(dict_id, dup_dict_id).await.unwrap().unwrap();
                            let capability = store.export(dup_dict_id).await.unwrap().unwrap();
                            let fsandbox::Capability::Dictionary(dict) = capability else {
                                panic!("capability was not a dictionary? {capability:?}");
                            };
                            let _ = responder.send(Ok(
                                fsandbox::DictionaryRouterRouteResponse::Dictionary(dict),
                            ));
                        }
                        fsandbox::DictionaryRouterRequest::_UnknownMethod { .. } => {
                            unimplemented!()
                        }
                    }
                }
            });
        }),
    );
    test.mock_runner.add_host_fn("test:///root_resolved", root_out_dir.host_fn());

    // Using "A" from the dictionary should succeed.
    for _ in 0..3 {
        test.check_use(
            "leaf".try_into().unwrap(),
            CheckUse::Protocol {
                path: "/svc/A".parse().unwrap(),
                expected_res: ExpectedResult::Ok,
            },
        )
        .await;
    }

    // Now, remove "A" from the dictionary. Using "A" this time should fail.
    let dest_id = 100;
    store
        .dictionary_remove(dict_id, "A", Some(&fsandbox::WrappedNewCapabilityId { id: dest_id }))
        .await
        .unwrap()
        .unwrap();
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("required_dict".parse().unwrap()))
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("optional_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("qux")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability(
                            "dict_with_optional_nested".parse().unwrap(),
                        ))
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("required_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("optional_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf")
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict_with_optional_nested")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("A")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf")
                        .from_dictionary("dict"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("B")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf")
                        .from_dictionary("dict/nested"),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("required_dict".parse().unwrap()))
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("optional_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("qux")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability(
                            "dict_with_optional_nested".parse().unwrap(),
                        ))
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("required_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("optional_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("mid")
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("dict_with_optional_nested")
                        .source(OfferSource::Self_)
                        .target_static_child("mid"),
                )
                .child_default("mid")
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .offer(
                    OfferBuilder::protocol()
                        .name("A")
                        .source(OfferSource::Parent)
                        .target_static_child("leaf")
                        .from_dictionary("required_dict"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("B")
                        .source(OfferSource::Parent)
                        .target_static_child("leaf")
                        .from_dictionary("optional_dict"),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("C")
                        .source(OfferSource::Parent)
                        .target_static_child("leaf")
                        .from_dictionary("dict_with_optional_nested/nested"),
                )
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
                        .source_static_child("leaf")
                        .name("A")
                        .availability(Availability::Optional),
                )
                .use_(
                    UseBuilder::protocol()
                        .source_static_child("leaf")
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap())),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("A")
                        .from_dictionary("dict")
                        .source(ExposeSource::Self_),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("B")
                        .from_dictionary("dict/nested")
                        .source(ExposeSource::Self_),
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
                .use_(UseBuilder::protocol().source_static_child("mid").name("A"))
                .use_(UseBuilder::protocol().source_static_child("mid").name("B"))
                .use_(UseBuilder::protocol().source_static_child("mid").name("C"))
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
                        .source_static_child("leaf"),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("B")
                        .from_dictionary("optional_dict")
                        .source_static_child("leaf"),
                )
                .expose(
                    ExposeBuilder::protocol()
                        .name("C")
                        .from_dictionary("dict_with_optional_nested/nested")
                        .source_static_child("leaf"),
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("dict".parse().unwrap()))
                        .availability(Availability::Optional),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("optional_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("qux")
                        .target_name("C")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("nested".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("nested")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability(
                            "dict_with_optional_nested".parse().unwrap(),
                        ))
                        .availability(Availability::Optional),
                )
                .expose(
                    ExposeBuilder::dictionary().name("required_dict").source(ExposeSource::Self_),
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
                        .source(ExposeSource::Self_),
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
