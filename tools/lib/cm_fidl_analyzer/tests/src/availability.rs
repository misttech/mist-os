// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod tests {
    use {
        crate::routing::RoutingTestBuilderForAnalyzer,
        cm_rust::{
            Availability, ExposeDecl, ExposeDirectoryDecl, ExposeProtocolDecl, ExposeServiceDecl,
            ExposeSource, ExposeTarget,
        },
        cm_rust_testing::ComponentDeclBuilder,
        moniker::{AbsoluteMoniker, AbsoluteMonikerBase},
        routing_test_helpers::{
            availability::CommonAvailabilityTest, CheckUse, ExpectedResult, RoutingTestModel,
            RoutingTestModelBuilder, ServiceInstance,
        },
        std::path::PathBuf,
    };

    #[fuchsia::test]
    async fn offer_availability_successful_routes() {
        CommonAvailabilityTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_offer_availability_successful_routes()
            .await
    }

    #[fuchsia::test]
    async fn offer_availability_invalid_routes() {
        CommonAvailabilityTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_offer_availability_invalid_routes()
            .await
    }

    #[fuchsia::test]
    async fn expose_availability_successful_routes() {
        CommonAvailabilityTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_expose_availability_successful_routes()
            .await
    }

    #[fuchsia::test]
    async fn expose_availability_invalid_routes() {
        CommonAvailabilityTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_expose_availability_invalid_routes()
            .await
    }

    // The following tests only run in `RoutingTestBuilderForAnalyzer` and not `RoutingTestBuilder`.
    // That's because the latter tries to check both the expose is valid and that the source of the
    // expose responds to FIDL calls. When routing from `void`, there is no FIDL server to answer
    // such calls.

    /// Creates the following topology:
    ///
    ///           a
    ///          /
    ///         /
    ///        b
    ///
    /// `a` exposes a variety of capabilities from `b`. `b` exposes those from `void`.
    #[fuchsia::test]
    pub async fn test_expose_availability_from_void() {
        struct TestCase {
            expose_availability: Availability,
            expected_res: ExpectedResult,
        }
        for test_case in &[
            TestCase {
                expose_availability: Availability::Required,
                expected_res: ExpectedResult::Err(fuchsia_zircon_status::Status::UNAVAILABLE),
            },
            TestCase {
                expose_availability: Availability::Optional,
                expected_res: ExpectedResult::Ok,
            },
        ] {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Child("b".to_owned()),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.expose_availability.clone(),
                        }))
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Child("b".to_owned()),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.expose_availability.clone(),
                        }))
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Child("b".to_owned()),
                            source_name: "dir".parse().unwrap(),
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: test_case.expose_availability.clone(),
                        }))
                        .add_lazy_child("b")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Void,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: Availability::Optional,
                        }))
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Void,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: Availability::Optional,
                        }))
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Void,
                            source_name: "dir".parse().unwrap(),
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: Availability::Optional,
                        }))
                        .build(),
                ),
            ];
            let builder = RoutingTestBuilderForAnalyzer::new("a", components);
            let model = builder.build().await;

            for check_use in vec![
                CheckUse::Service {
                    path: "/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: test_case.expected_res.clone(),
                },
                CheckUse::Protocol {
                    path: "/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: test_case.expected_res.clone(),
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: test_case.expected_res.clone(),
                },
            ] {
                model.check_use_exposed_dir(AbsoluteMoniker::root(), check_use).await;
            }
        }
    }
}
