// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        CheckUse, ComponentEventRoute, ExpectedResult, RoutingTestModel, RoutingTestModelBuilder,
        ServiceInstance,
    },
    cm_moniker::InstancedRelativeMoniker,
    cm_rust::*,
    cm_rust_testing::{ComponentDeclBuilder, DirectoryDeclBuilder, ProtocolDeclBuilder},
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio,
    fuchsia_zircon_status as zx_status,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase},
    std::{
        convert::TryInto,
        marker::PhantomData,
        path::{Path, PathBuf},
        str::FromStr,
    },
};

pub struct CommonAvailabilityTest<T: RoutingTestModelBuilder> {
    builder: PhantomData<T>,
}

#[derive(Debug)]
struct TestCase {
    /// The availability of either an `Offer` or `Expose` declaration.
    provider_availability: Availability,
    use_availability: Availability,
}

impl<T: RoutingTestModelBuilder> CommonAvailabilityTest<T> {
    pub fn new() -> Self {
        Self { builder: PhantomData }
    }

    const VALID_AVAILABILITY_PAIRS: &'static [TestCase] = &[
        TestCase {
            provider_availability: Availability::Required,
            use_availability: Availability::Required,
        },
        TestCase {
            provider_availability: Availability::Optional,
            use_availability: Availability::Optional,
        },
        TestCase {
            provider_availability: Availability::Required,
            use_availability: Availability::Optional,
        },
        TestCase {
            provider_availability: Availability::SameAsTarget,
            use_availability: Availability::Required,
        },
        TestCase {
            provider_availability: Availability::SameAsTarget,
            use_availability: Availability::Optional,
        },
        TestCase {
            provider_availability: Availability::Required,
            use_availability: Availability::Transitional,
        },
        TestCase {
            provider_availability: Availability::Optional,
            use_availability: Availability::Transitional,
        },
        TestCase {
            provider_availability: Availability::Transitional,
            use_availability: Availability::Transitional,
        },
        TestCase {
            provider_availability: Availability::SameAsTarget,
            use_availability: Availability::Transitional,
        },
    ];

    pub async fn test_offer_availability_successful_routes(&self) {
        for test_case in Self::VALID_AVAILABILITY_PAIRS {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .offer(OfferDecl::Service(OfferServiceDecl {
                            source: OfferSource::static_child("b".to_string()),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            source_instance_filter: None,
                            renamed_instances: None,
                            availability: test_case.provider_availability.clone(),
                        }))
                        .offer(OfferDecl::Protocol(OfferProtocolDecl {
                            source: OfferSource::static_child("b".to_string()),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.provider_availability.clone(),
                        }))
                        .offer(OfferDecl::Directory(OfferDirectoryDecl {
                            source: OfferSource::static_child("b".to_string()),
                            source_name: "dir".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            target_name: "dir".parse().unwrap(),
                            rights: Some(fio::R_STAR_DIR),
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.provider_availability.clone(),
                        }))
                        .directory(
                            DirectoryDeclBuilder::new("data")
                                .path("/data")
                                .rights(fio::RW_STAR_DIR)
                                .build(),
                        )
                        .storage(StorageDecl {
                            name: "cache".parse().unwrap(),
                            backing_dir: "data".parse().unwrap(),
                            source: StorageDirectorySource::Self_,
                            subdir: Some(PathBuf::from("cache")),
                            storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                        })
                        .offer(OfferDecl::Storage(OfferStorageDecl {
                            source: OfferSource::Self_,
                            target: OfferTarget::static_child("c".to_string()),
                            source_name: "cache".parse().unwrap(),
                            target_name: "cache".parse().unwrap(),
                            availability: test_case.provider_availability.clone(),
                        }))
                        .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                            source: OfferSource::Parent,
                            source_name: "started".parse().unwrap(),
                            scope: None,
                            filter: None,
                            target: OfferTarget::Child(ChildRef {
                                name: "c".into(),
                                collection: None,
                            }),
                            target_name: "started".parse().unwrap(),
                            availability: test_case.provider_availability.clone(),
                        }))
                        .add_lazy_child("b")
                        .add_lazy_child("c")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .service(ServiceDecl {
                            name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_path: Some("/svc/foo.service".parse().unwrap()),
                        })
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .protocol(ProtocolDeclBuilder::new("fuchsia.examples.Echo").build())
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .directory(
                            DirectoryDeclBuilder::new("dir")
                                .path("/data/dir")
                                .rights(fio::R_STAR_DIR)
                                .build(),
                        )
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Self_,
                            source_name: "dir".parse().unwrap(),
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: cm_rust::Availability::Required,
                        }))
                        .build(),
                ),
                (
                    "c",
                    ComponentDeclBuilder::new()
                        .use_(UseDecl::Service(UseServiceDecl {
                            source: UseSource::Parent,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_path: "/svc/fuchsia.examples.EchoService".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Protocol(UseProtocolDecl {
                            source: UseSource::Parent,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_path: "/svc/fuchsia.examples.Echo".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Directory(UseDirectoryDecl {
                            source: UseSource::Parent,
                            source_name: "dir".parse().unwrap(),
                            target_path: "/dir".parse().unwrap(),
                            rights: fio::R_STAR_DIR,
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Storage(UseStorageDecl {
                            source_name: "cache".parse().unwrap(),
                            target_path: "/storage".parse().unwrap(),
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::EventStream(UseEventStreamDecl {
                            source: UseSource::Parent,
                            source_name: "started".parse().unwrap(),
                            target_path: "/event/stream".parse().unwrap(),
                            scope: None,
                            filter: None,
                            availability: test_case.use_availability.clone(),
                        }))
                        .build(),
                ),
            ];
            let mut builder = T::new("a", components);
            builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
                name: "started".parse().unwrap(),
            })]);
            let model = builder.build().await;
            model
                .create_static_file(Path::new("dir/hippo"), "hello")
                .await
                .expect("failed to create file");
            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedRelativeMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute { component: "/".to_string(), scope: None }],
                    name: "started".parse().unwrap(),
                },
            ] {
                model.check_use(vec!["c"].try_into().unwrap(), check_use).await;
            }
        }
    }

    pub async fn test_offer_availability_invalid_routes(&self) {
        struct TestCase {
            source: OfferSource,
            storage_source: Option<OfferSource>,
            offer_availability: Availability,
            use_availability: Availability,
        }
        for test_case in &[
            TestCase {
                source: OfferSource::static_child("b".to_string()),
                storage_source: Some(OfferSource::Self_),
                offer_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Optional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Transitional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Transitional,
                use_availability: Availability::Required,
            },
        ] {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .offer(OfferDecl::Service(OfferServiceDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            source_instance_filter: None,
                            renamed_instances: None,
                            availability: test_case.offer_availability.clone(),
                        }))
                        .offer(OfferDecl::Protocol(OfferProtocolDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.offer_availability.clone(),
                        }))
                        .offer(OfferDecl::Directory(OfferDirectoryDecl {
                            source: test_case.source.clone(),
                            source_name: "dir".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            target_name: "dir".parse().unwrap(),
                            rights: Some(fio::Operations::CONNECT),
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.offer_availability.clone(),
                        }))
                        .offer(OfferDecl::Storage(OfferStorageDecl {
                            source: test_case
                                .storage_source
                                .as_ref()
                                .map(Clone::clone)
                                .unwrap_or(test_case.source.clone()),
                            source_name: "data".parse().unwrap(),
                            target_name: "data".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            availability: test_case.offer_availability.clone(),
                        }))
                        .storage(StorageDecl {
                            name: "data".parse().unwrap(),
                            source: StorageDirectorySource::Child("b".to_string()),
                            backing_dir: "dir".parse().unwrap(),
                            subdir: None,
                            storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                        })
                        .add_lazy_child("b")
                        .add_lazy_child("c")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .service(ServiceDecl {
                            name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_path: Some("/svc/foo.service".parse().unwrap()),
                        })
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .protocol(ProtocolDeclBuilder::new("fuchsia.examples.Echo").build())
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .directory(
                            DirectoryDeclBuilder::new("dir")
                                .path("/dir")
                                .rights(fio::Operations::CONNECT)
                                .build(),
                        )
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Self_,
                            source_name: "dir".parse().unwrap(),
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: cm_rust::Availability::Required,
                        }))
                        .build(),
                ),
                (
                    "c",
                    ComponentDeclBuilder::new()
                        .use_(UseDecl::Service(UseServiceDecl {
                            source: UseSource::Parent,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_path: "/svc/fuchsia.examples.EchoService".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Protocol(UseProtocolDecl {
                            source: UseSource::Parent,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_path: "/svc/fuchsia.examples.Echo".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Directory(UseDirectoryDecl {
                            source: UseSource::Parent,
                            source_name: "dir".parse().unwrap(),
                            target_path: "/dir".parse().unwrap(),
                            rights: fio::Operations::CONNECT,
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Storage(UseStorageDecl {
                            source_name: "data".parse().unwrap(),
                            target_path: "/data".parse().unwrap(),
                            availability: test_case.use_availability.clone(),
                        }))
                        .build(),
                ),
            ];
            let model = T::new("a", components).build().await;
            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
                CheckUse::Storage {
                    path: "/data".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            ] {
                model.check_use(vec!["c"].try_into().unwrap(), check_use).await;
            }
        }
    }

    /// Creates the following topology:
    ///
    ///           a
    ///          /
    ///         /
    ///        b
    ///
    /// And verifies exposing a variety of capabilities from `b`, testing the combination of
    /// availability settings and capability types.
    ///
    /// Storage and event stream capabilities cannot be exposed, hence omitted.
    pub async fn test_expose_availability_successful_routes(&self) {
        for test_case in Self::VALID_AVAILABILITY_PAIRS {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .use_(UseDecl::Service(UseServiceDecl {
                            source: UseSource::Child("b".to_owned()),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_path: cm_types::Path::from_str(
                                "/svc/fuchsia.examples.EchoService_a",
                            )
                            .unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Protocol(UseProtocolDecl {
                            source: UseSource::Child("b".to_owned()),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_path: "/svc/fuchsia.examples.Echo_a".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Directory(UseDirectoryDecl {
                            source: UseSource::Child("b".to_owned()),
                            source_name: "dir".parse().unwrap(),
                            target_path: cm_types::Path::from_str("/dir_a").unwrap(),
                            rights: fio::R_STAR_DIR,
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .add_lazy_child("b")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .service(ServiceDecl {
                            name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_path: Some("/svc/foo.service".parse().unwrap()),
                        })
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.provider_availability.clone(),
                        }))
                        .protocol(ProtocolDeclBuilder::new("fuchsia.examples.Echo").build())
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.provider_availability.clone(),
                        }))
                        .directory(
                            DirectoryDeclBuilder::new("dir")
                                .path("/data/dir")
                                .rights(fio::R_STAR_DIR)
                                .build(),
                        )
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Self_,
                            source_name: "dir".parse().unwrap(),
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: test_case.provider_availability.clone(),
                        }))
                        .build(),
                ),
            ];
            let builder = T::new("a", components);
            let model = builder.build().await;

            // Add a file to the directory capability in the component that declared it, so "b".
            model
                .create_static_file(Path::new("dir/hippo"), "hello")
                .await
                .expect("failed to create file");

            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService_a".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo_a".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Directory {
                    path: "/dir_a".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            ] {
                model.check_use(AbsoluteMoniker::root(), check_use).await;
            }

            for check_use in vec![
                CheckUse::Service {
                    path: "/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Protocol {
                    path: "/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            ] {
                model.check_use_exposed_dir(vec!["b"].try_into().unwrap(), check_use).await;
            }
        }
    }

    /// Creates the following topology:
    ///
    ///           a
    ///          /
    ///         /
    ///        b
    ///
    /// And verifies exposing a variety of capabilities from `b`. Except that either the route is
    /// broken, or the rules around availability are broken.
    pub async fn test_expose_availability_invalid_routes(&self) {
        struct TestCase {
            source: ExposeSource,
            expose_availability: Availability,
            use_availability: Availability,
        }
        for test_case in &[
            TestCase {
                source: ExposeSource::Self_,
                expose_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Optional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Transitional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Transitional,
                use_availability: Availability::Required,
            },
        ] {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .use_(UseDecl::Service(UseServiceDecl {
                            source: UseSource::Child("b".to_owned()),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_path: cm_types::Path::from_str(
                                "/svc/fuchsia.examples.EchoService_a",
                            )
                            .unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Protocol(UseProtocolDecl {
                            source: UseSource::Child("b".to_owned()),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_path: "/svc/fuchsia.examples.Echo_a".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .use_(UseDecl::Directory(UseDirectoryDecl {
                            source: UseSource::Child("b".to_owned()),
                            source_name: "dir".parse().unwrap(),
                            target_path: cm_types::Path::from_str("/dir_a").unwrap(),
                            rights: fio::R_STAR_DIR,
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.use_availability.clone(),
                        }))
                        .add_lazy_child("b")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .service(ServiceDecl {
                            name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_path: Some("/svc/foo.service".parse().unwrap()),
                        })
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.expose_availability.clone(),
                        }))
                        .protocol(ProtocolDeclBuilder::new("fuchsia.examples.Echo").build())
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.expose_availability.clone(),
                        }))
                        .directory(
                            DirectoryDeclBuilder::new("dir")
                                .path("/data/dir")
                                .rights(fio::R_STAR_DIR)
                                .build(),
                        )
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: test_case.source.clone(),
                            source_name: "dir".parse().unwrap(),
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: test_case.expose_availability.clone(),
                        }))
                        .build(),
                ),
            ];
            let builder = T::new("a", components);
            let model = builder.build().await;

            // Add a file to the directory capability in the component that declared it, so "b".
            model
                .create_static_file(Path::new("dir/hippo"), "hello")
                .await
                .expect("failed to create file");
            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService_a".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo_a".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
                CheckUse::Directory {
                    path: "/dir_a".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            ] {
                model.check_use(AbsoluteMoniker::root(), check_use).await;
            }
        }
    }
}
