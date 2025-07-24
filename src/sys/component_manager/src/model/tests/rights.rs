// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::ComponentInstance;
use crate::model::routing::RoutingFailureErrorReporter;
use crate::model::testing::routing_test_helpers::*;
use ::routing_test_helpers::rights::CommonRightsTest;
use ::routing_test_helpers::RoutingTestModel;
use cm_rust::*;
use cm_rust_testing::*;
use fidl_fuchsia_io as fio;
use routing::error::RouteRequestErrorInfo;
use routing::WithPorcelain;
use sandbox::{DirConnector, Request, Router};

#[fuchsia::test]
async fn offer_increasing_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new().test_offer_increasing_rights().await
}

#[fuchsia::test]
async fn offer_incompatible_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new().test_offer_incompatible_rights().await
}

#[fuchsia::test]
async fn expose_increasing_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new().test_expose_increasing_rights().await
}

#[fuchsia::test]
async fn expose_incompatible_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new().test_expose_incompatible_rights().await
}

#[fuchsia::test]
async fn capability_increasing_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new().test_capability_increasing_rights().await
}

#[fuchsia::test]
async fn capability_incompatible_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new().test_capability_incompatible_rights().await
}

#[fuchsia::test]
async fn offer_from_component_manager_namespace_directory_incompatible_rights() {
    CommonRightsTest::<RoutingTestBuilder>::new()
        .test_offer_from_component_manager_namespace_directory_incompatible_rights()
        .await
}

#[fuchsia::test]
async fn framework_directory_rights() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(
                    OfferBuilder::directory()
                        .name("foo_data")
                        .source(OfferSource::Framework)
                        .target_static_child("b")
                        .subdir("foo"),
                )
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::directory().name("foo_data").path("/data/hippo"))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    let foo_dir_proxy = fuchsia_fs::directory::open_directory_async(
        &test.test_dir_proxy,
        "foo",
        fio::PERM_READABLE,
    )
    .unwrap();
    test.model
        .context()
        .add_framework_capability(
            "foo_data",
            Router::<DirConnector>::new_ok(DirConnector::from_proxy(
                foo_dir_proxy,
                cm_types::RelativePath::dot(),
                fio::PERM_READABLE,
            )),
        )
        .await;
    test.check_use("b".try_into().unwrap(), CheckUse::default_directory(ExpectedResult::Ok)).await;
}

#[fuchsia::test]
async fn framework_directory_incompatible_rights() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(
                    OfferBuilder::directory()
                        .name("foo_data")
                        .source(OfferSource::Framework)
                        .target_static_child("b")
                        .subdir("foo"),
                )
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::directory()
                        .name("foo_data")
                        .path("/data/hippo")
                        .rights(fio::X_STAR_DIR),
                )
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    test.model
        .context()
        .add_framework_capability(
            "foo_data",
            WithPorcelain::<_, _, ComponentInstance>::with_porcelain_no_default(
                Router::<DirConnector>::new(move |_request: Option<Request>, _debug: bool| {
                    panic!("routing should have failed before we get here")
                }),
                CapabilityTypeName::Directory,
            )
            .availability(Availability::Required)
            .rights(Some(fio::R_STAR_DIR.into()))
            // It would be more realistic to use component `b` here, but that would cause `a`
            // to get resolve, fixing its sandbox before we're able to inject the additional
            // framework capability, so we need to pass some value that doesn't result in `a`
            // being resolved.
            .target_above_root(test.model.top_instance())
            .error_info(RouteRequestErrorInfo::from(&cm_rust::CapabilityDecl::Directory(
                cm_rust::DirectoryDecl {
                    name: "foo_data".parse().unwrap(),
                    source_path: None,
                    rights: fio::R_STAR_DIR,
                },
            )))
            .error_reporter(RoutingFailureErrorReporter::new())
            .build(),
        )
        .await;
    test.check_use(
        "b".try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Err(zx::Status::ACCESS_DENIED)),
    )
    .await;
}
