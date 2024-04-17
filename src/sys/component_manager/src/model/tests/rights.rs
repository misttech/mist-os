// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, FrameworkCapability},
        model::{
            component::WeakComponentInstance, error::CapabilityProviderError,
            testing::routing_test_helpers::*,
        },
    },
    ::routing::capability_source::InternalCapability,
    ::routing_test_helpers::{rights::CommonRightsTest, RoutingTestModel},
    async_trait::async_trait,
    cm_rust::*,
    cm_rust_testing::*,
    cm_util::TaskGroup,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    vfs::{directory::entry::OpenRequest, remote::remote_dir},
};

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

struct MockFrameworkDirectoryProvider {
    test_dir_proxy: fio::DirectoryProxy,
}
struct MockFrameworkDirectory {
    test_dir_proxy: fio::DirectoryProxy,
}

#[async_trait]
impl CapabilityProvider for MockFrameworkDirectoryProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        open_request
            .open_remote(remote_dir(Clone::clone(&self.test_dir_proxy)))
            .map_err(|e| CapabilityProviderError::VfsOpenError(e))
    }
}

impl FrameworkCapability for MockFrameworkDirectory {
    fn matches(&self, capability: &InternalCapability) -> bool {
        matches!(capability, InternalCapability::Directory(n) if n.as_str() == "foo_data")
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        let test_dir_proxy = fuchsia_fs::directory::clone_no_describe(&self.test_dir_proxy, None)
            .expect("failed to clone test dir");
        Box::new(MockFrameworkDirectoryProvider { test_dir_proxy })
    }
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
    let test_dir_proxy = fuchsia_fs::directory::clone_no_describe(&test.test_dir_proxy, None)
        .expect("failed to clone test dir");
    let directory_host = Box::new(MockFrameworkDirectory { test_dir_proxy });
    test.model.context().add_framework_capability(directory_host).await;
    test.check_use(vec!["b"].try_into().unwrap(), CheckUse::default_directory(ExpectedResult::Ok))
        .await;
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
    let test_dir_proxy = fuchsia_fs::directory::clone_no_describe(&test.test_dir_proxy, None)
        .expect("failed to clone test dir");
    let directory_host = Box::new(MockFrameworkDirectory { test_dir_proxy });
    test.model.context().add_framework_capability(directory_host).await;
    test.check_use(
        vec!["b"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Err(zx::Status::ACCESS_DENIED)),
    )
    .await;
}
