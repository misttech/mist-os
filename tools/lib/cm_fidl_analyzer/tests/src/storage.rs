// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod tests {
    use {
        crate::routing::RoutingTestBuilderForAnalyzer,
        cm_fidl_analyzer::route::VerifyRouteResult,
        cm_moniker::InstancedMoniker,
        cm_rust::{
            CapabilityDecl, CapabilityTypeName, OfferSource, OfferTarget, StorageDirectorySource,
        },
        cm_rust_testing::*,
        component_id_index::InstanceId,
        fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio,
        fuchsia_zircon_status as zx_status,
        moniker::{Moniker, MonikerBase},
        routing::{mapper::RouteSegment, RegistrationDecl},
        routing_test_helpers::{
            component_id_index::make_index_file, storage::CommonStorageTest, CheckUse,
            ExpectedResult, RoutingTestModel, RoutingTestModelBuilder,
        },
        std::collections::HashSet,
    };

    #[fuchsia::test]
    async fn storage_dir_from_cm_namespace() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_dir_from_cm_namespace()
            .await
    }

    #[fuchsia::test]
    async fn storage_and_dir_from_parent() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_and_dir_from_parent()
            .await
    }

    #[fuchsia::test]
    async fn storage_and_dir_from_parent_with_subdir() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_and_dir_from_parent_with_subdir()
            .await
    }

    #[fuchsia::test]
    async fn storage_and_dir_from_parent_rights_invalid() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_and_dir_from_parent_rights_invalid()
            .await
    }

    #[fuchsia::test]
    async fn storage_from_parent_dir_from_grandparent() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_from_parent_dir_from_grandparent()
            .await
    }

    #[fuchsia::test]
    async fn storage_from_parent_dir_from_grandparent_with_subdirs() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_from_parent_dir_from_grandparent_with_subdirs()
            .await
    }

    #[fuchsia::test]
    async fn storage_from_parent_dir_from_grandparent_with_subdir() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_from_parent_dir_from_grandparent_with_subdir()
            .await
    }

    #[fuchsia::test]
    async fn storage_and_dir_from_grandparent() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_and_dir_from_grandparent()
            .await
    }

    #[fuchsia::test]
    async fn storage_from_parent_dir_from_sibling() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_from_parent_dir_from_sibling()
            .await
    }

    #[fuchsia::test]
    async fn storage_from_parent_dir_from_sibling_with_subdir() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_from_parent_dir_from_sibling_with_subdir()
            .await
    }

    #[fuchsia::test]
    async fn storage_multiple_types() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_multiple_types()
            .await
    }

    #[fuchsia::test]
    async fn use_the_wrong_type_of_storage() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_use_the_wrong_type_of_storage()
            .await
    }

    #[fuchsia::test]
    async fn directories_are_not_storage() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_directories_are_not_storage()
            .await
    }

    #[fuchsia::test]
    async fn use_storage_when_not_offered() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_use_storage_when_not_offered()
            .await
    }

    #[fuchsia::test]
    async fn dir_offered_from_nonexecutable() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_dir_offered_from_nonexecutable()
            .await
    }

    #[fuchsia::test]
    async fn storage_dir_from_cm_namespace_prevented_by_policy() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_storage_dir_from_cm_namespace_prevented_by_policy()
            .await
    }

    #[fuchsia::test]
    async fn instance_id_from_index() {
        CommonStorageTest::<RoutingTestBuilderForAnalyzer>::new()
            .test_instance_id_from_index()
            .await
    }

    ///   component manager's namespace
    ///    |
    ///   provider (provides storage capability, restricted to component ID index)
    ///    |
    ///   consumer (not in component ID index)
    ///
    /// Tests that consumer cannot use restricted storage as it isn't in the component ID
    /// index.
    ///
    /// This test only runs for the static model. Component Manager has a similar test that
    /// instead expects failure when a component is started, if that component uses restricted
    /// storage and is not in the component ID index.
    #[fuchsia::test]
    async fn use_restricted_storage_failure() {
        let parent_consumer_instance_id = InstanceId::new_random(&mut rand::thread_rng());
        let index = {
            let mut index = component_id_index::Index::default();
            index
                .insert(
                    Moniker::parse_str("parent_consumer").unwrap(),
                    parent_consumer_instance_id.clone(),
                )
                .unwrap();
            index
        };
        let component_id_index_path = make_index_file(index).unwrap();
        let components = vec![
            (
                "provider",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("cache")
                            .backing_dir("data")
                            .source(StorageDirectorySource::Self_)
                            .storage_id(fdecl::StorageId::StaticInstanceId),
                    )
                    .offer(
                        OfferBuilder::storage()
                            .name("cache")
                            .source(OfferSource::Self_)
                            .target(OfferTarget::static_child("consumer".to_string())),
                    )
                    .child_default("consumer")
                    .build(),
            ),
            (
                "consumer",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("cache").path("/storage"))
                    .build(),
            ),
        ];
        let mut builder = RoutingTestBuilderForAnalyzer::new("provider", components);
        builder.set_component_id_index_path(
            component_id_index_path.path().to_owned().try_into().unwrap(),
        );
        let model = builder.build().await;

        model
            .check_use(
                Moniker::parse_str("consumer").unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(InstancedMoniker::try_from(vec!["consumer:0"]).unwrap()),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
            )
            .await;
    }

    /// Tests verification of a storage capability route from an unused offer. (Routes from offers
    /// are only verified if the target does not use the offered capability.)
    ///
    ///   directory_provider
    ///    |
    ///   storage_provider
    ///    |
    ///   not_consumer
    ///
    /// `directory_provider` declares a directory capability and offers it to `storage_provider`.
    /// `storage_provider` declares a storage capability with that directory as the backing dir,
    /// and offers the storage capability to `not_consumer`. `not_consumer` does not use the
    /// storage capability.
    ///
    /// Note that since the capability is not used, there is no requirement on the contents of the
    /// component ID index, even though the storage capability uses restricted storage.
    ///
    /// This test only runs for the static model, since Component Manager doesn't route capabilities
    /// from offer declarations.
    #[fuchsia::test]
    async fn route_storage_from_offer() {
        let directory_decl = CapabilityBuilder::directory()
            .name("data")
            .path("/data")
            .rights(fio::RW_STAR_DIR)
            .build();
        let offer_directory_decl = OfferBuilder::directory()
            .name("data")
            .source(OfferSource::Self_)
            .target(OfferTarget::static_child("storage_provider".to_string()))
            .rights(fio::RW_STAR_DIR)
            .build();
        let storage_decl = CapabilityBuilder::storage()
            .name("cache")
            .backing_dir("data")
            .source(StorageDirectorySource::Parent)
            .storage_id(fdecl::StorageId::StaticInstanceId)
            .build();
        let offer_storage_decl = OfferBuilder::storage()
            .name("cache")
            .source(OfferSource::Self_)
            .target(OfferTarget::static_child("not_consumer".to_string()))
            .build();
        let components = vec![
            (
                "directory_provider",
                ComponentDeclBuilder::new()
                    .capability(directory_decl.clone())
                    .offer(offer_directory_decl.clone())
                    .child_default("storage_provider")
                    .build(),
            ),
            (
                "storage_provider",
                ComponentDeclBuilder::new()
                    .capability(storage_decl.clone())
                    .offer(offer_storage_decl.clone())
                    .child_default("not_consumer")
                    .build(),
            ),
            ("not_consumer", ComponentDeclBuilder::new().build()),
        ];
        let test =
            RoutingTestBuilderForAnalyzer::new("directory_provider", components).build().await;
        let storage_provider = test
            .look_up_instance(&Moniker::parse_str("/storage_provider").unwrap())
            .await
            .expect("storage_provider instance");

        let route_maps = test.model.check_routes_for_instance(
            &storage_provider,
            &HashSet::from_iter(vec![CapabilityTypeName::Storage].into_iter()),
        );
        assert_eq!(route_maps.len(), 1);

        let storage = route_maps
            .get(&CapabilityTypeName::Storage)
            .expect("expected a storage capability route");
        let CapabilityDecl::Storage(inner_storage_decl) = storage_decl.clone() else {
            unreachable!()
        };

        assert_eq!(
            storage,
            &vec![
                VerifyRouteResult {
                    using_node: Moniker::parse_str("/storage_provider").unwrap(),
                    capability: Some("cache".parse().unwrap()),
                    error: None,
                    route: vec![
                        RouteSegment::OfferBy {
                            moniker: Moniker::parse_str("/storage_provider").unwrap(),
                            capability: offer_storage_decl
                        },
                        RouteSegment::DeclareBy {
                            moniker: Moniker::parse_str("/storage_provider").unwrap(),
                            capability: storage_decl
                        }
                    ]
                },
                VerifyRouteResult {
                    using_node: Moniker::parse_str("/storage_provider").unwrap(),
                    capability: Some("cache".parse().unwrap()),
                    error: None,
                    route: vec![
                        RouteSegment::RegisterBy {
                            moniker: Moniker::parse_str("/storage_provider").unwrap(),
                            capability: RegistrationDecl::Directory(inner_storage_decl.into())
                        },
                        RouteSegment::OfferBy {
                            moniker: Moniker::root(),
                            capability: offer_directory_decl
                        },
                        RouteSegment::DeclareBy {
                            moniker: Moniker::root(),
                            capability: directory_decl
                        }
                    ],
                }
            ]
        );
    }
}
