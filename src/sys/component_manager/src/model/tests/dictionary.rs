// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        framework::factory::FactoryCapabilityHost,
        model::testing::{out_dir::OutDir, routing_test_helpers::*},
    },
    cm_rust::*,
    cm_rust_testing::*,
    fidl::endpoints::{self, ServerEnd},
    fidl_fidl_examples_routing_echo::EchoMarker,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon as zx,
    futures::TryStreamExt,
    moniker::Moniker,
    routing_test_helpers::RoutingTestModel,
    std::path::PathBuf,
    test_case::test_case,
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
async fn extend_from_self() {
    // Tests extending a dictionary, when the source dictionary is defined by self.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol_default("bar")
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .target_name("A")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("self_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Parent)
                        .target(OfferTarget::Capability("origin_dict".parse().unwrap())),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("origin_dict".parse().unwrap())),
                )
                .offer(
                    OfferBuilder::dictionary()
                        .name("origin_dict")
                        .source(OfferSource::Self_)
                        .target_static_child("leaf"),
                )
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
                                name: "leaf".parse().unwrap(),
                                collection: None,
                            }),
                            "origin_dict",
                        )
                        .build(),
                )
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
                .offer(
                    OfferBuilder::protocol()
                        .name("bar")
                        .target_name("B")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("origin_dict".parse().unwrap())),
                )
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
async fn extend_from_program() {
    // Tests extending a dictionary, when the source dictionary is provided by the program.

    const ROUTER_PATH: &str = "svc/fuchsia.component.sandbox.Router";
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .capability(
                    CapabilityBuilder::dictionary()
                        .name("dict")
                        .source_dictionary(DictionarySource::Program, ROUTER_PATH),
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
                .build(),
        ),
    ];
    let test = RoutingTestBuilder::new("root", components).build().await;

    let factory_host = FactoryCapabilityHost::new();
    let (factory, stream) =
        endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
    let _factory_task = fasync::Task::spawn(async move {
        factory_host.serve(stream).await.unwrap();
    });

    // Create a dictionary with a Sender at "A" for the Echo protocol.
    let dict = factory.create_dictionary().await.unwrap();
    let dict = dict.into_proxy().unwrap();
    let (receiver_client, mut receiver_stream) =
        endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
    let connector = factory.create_connector(receiver_client).await.unwrap();
    dict.insert("A", fsandbox::Capability::Connector(connector)).await.unwrap().unwrap();

    // Serve the Echo protocol from the Receiver.
    let _receiver_task = fasync::Task::spawn(async move {
        let mut task_group = fasync::TaskGroup::new();
        while let Ok(Some(request)) = receiver_stream.try_next().await {
            match request {
                fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                    let channel: ServerEnd<EchoMarker> = channel.into();
                    task_group.spawn(OutDir::echo_protocol_fn(channel.into_stream().unwrap()));
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
    let dict_clone = Clone::clone(&dict);
    root_out_dir.add_entry(
        format!("/{ROUTER_PATH}").parse().unwrap(),
        vfs::service::endpoint(move |scope, channel| {
            let server_end: ServerEnd<fsandbox::RouterMarker> = channel.into_zx_channel().into();
            let mut stream = server_end.into_stream().unwrap();
            let dict = Clone::clone(&dict_clone);
            scope.spawn(async move {
                while let Ok(Some(request)) = stream.try_next().await {
                    match request {
                        fsandbox::RouterRequest::Route { payload: _, responder } => {
                            let client_end = dict.clone().await.unwrap();
                            let capability = fsandbox::Capability::Dictionary(client_end);
                            let _ = responder.send(Ok(capability));
                        }
                        fsandbox::RouterRequest::_UnknownMethod { .. } => {
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
    let _ = dict.remove("A").await.unwrap().unwrap();
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

enum Statement {
    Declare,
    Extend,
}

/// Tests extending a dictionary, when the source dictionary is defined by self.
/// Additionally, the source dictionary may be defined before or after the target dictionary.
#[test_case(Statement::Declare, Statement::Extend)]
#[test_case(Statement::Extend, Statement::Declare)]
#[fuchsia::test]
async fn dict_extend_from_self_different_decl_ordering(first: Statement, second: Statement) {
    let mut root = ComponentDeclBuilder::new().child_default("leaf").use_(
        UseBuilder::protocol().source(UseSource::Self_).name("foo").from_dictionary("target"),
    );
    for statement in [first, second] {
        root = match statement {
            Statement::Declare => root.capability(
                CapabilityBuilder::dictionary()
                    .name("source")
                    .source_dictionary(
                        DictionarySource::Child(ChildRef {
                            name: "leaf".parse().unwrap(),
                            collection: None,
                        }),
                        "child_dict",
                    )
                    .build(),
            ),
            Statement::Extend => root.capability(
                CapabilityBuilder::dictionary()
                    .name("target")
                    .source_dictionary(DictionarySource::Self_, "source")
                    .build(),
            ),
        }
    }
    let root = root.build();

    let components = vec![
        ("root", root),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .dictionary_default("child_dict")
                .offer(
                    OfferBuilder::protocol()
                        .name("foo")
                        .source(OfferSource::Self_)
                        .target(OfferTarget::Capability("child_dict".parse().unwrap())),
                )
                .expose(ExposeBuilder::dictionary().name("child_dict").source(ExposeSource::Self_))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    test.check_use(
        Moniker::default(),
        CheckUse::Protocol { path: "/svc/foo".parse().unwrap(), expected_res: ExpectedResult::Ok },
    )
    .await;
}
