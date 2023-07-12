// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        component_id_index::make_index_file, generate_storage_path, CheckUse, ExpectedResult,
        RoutingTestModel, RoutingTestModelBuilder,
    },
    cm_moniker::InstancedAbsoluteMoniker,
    cm_rust::*,
    cm_rust_testing::{ComponentDeclBuilder, DirectoryDeclBuilder},
    component_id_index::gen_instance_id,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio,
    fuchsia_zircon_status as zx_status,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ExtendedMoniker},
    routing::config::{CapabilityAllowlistKey, CapabilityAllowlistSource},
    std::{collections::HashSet, convert::TryInto, marker::PhantomData, path::PathBuf},
};

pub struct CommonStorageTest<T: RoutingTestModelBuilder> {
    builder: PhantomData<T>,
}

impl<T: RoutingTestModelBuilder> CommonStorageTest<T> {
    pub fn new() -> Self {
        Self { builder: PhantomData }
    }

    ///   component manager's namespace
    ///    |
    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "mystorage" with a source of realm at path /data
    /// a: offers cache storage to b from "mystorage"
    /// b: uses cache storage as /storage.
    pub async fn test_storage_dir_from_cm_namespace(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "tmp".parse().unwrap(),
                        source: StorageDirectorySource::Parent,
                        subdir: Some(PathBuf::from("cache")),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let namespace_capabilities = vec![CapabilityDecl::Directory(
            DirectoryDeclBuilder::new("tmp").path("/tmp").rights(fio::RW_STAR_DIR).build(),
        )];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        let model = builder.build().await;

        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["b:0"]).unwrap(),
                    ),
                    from_cm_namespace: true,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model.check_namespace_subdir_contents("/tmp/cache", vec!["b:0".to_string()]).await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has storage decl with name "mystorage" with a source of self at path /data
    /// a: offers cache storage to b from "mystorage"
    /// b: uses cache storage as /storage
    pub async fn test_storage_and_dir_from_parent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "data".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["b:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model.check_test_subdir_contents(".", vec!["b:0".to_string(), "foo".to_string()]).await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has storage decl with name "mystorage" with a source of self at path /data, with subdir
    ///    "cache"
    /// a: offers cache storage to b from "mystorage"
    /// b: uses cache storage as /storage
    pub async fn test_storage_and_dir_from_parent_with_subdir(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "data".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: Some(PathBuf::from("cache")),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["b:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model.check_test_subdir_contents(".", vec!["cache".to_string(), "foo".to_string()]).await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has storage decl with name "mystorage" with a source of self at path /data, but /data
    ///    has only read rights
    /// a: offers cache storage to b from "mystorage"
    /// b: uses cache storage as /storage
    pub async fn test_storage_and_dir_from_parent_rights_invalid(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::R_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "data".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data to b as /minfs
    /// b: has storage decl with name "mystorage" with a source of realm at path /minfs
    /// b: offers data storage to c from "mystorage"
    /// c: uses data storage as /storage
    pub async fn test_storage_from_parent_dir_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "data".parse().unwrap(),
                        target_name: "minfs".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Parent,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data to b as /minfs with subdir "subdir_1"
    /// b: has storage decl with name "mystorage" with a source of realm at path /minfs with subdir
    ///    "subdir_2"
    /// b: offers data storage to c from "mystorage"
    /// c: uses data storage as /storage
    pub async fn test_storage_from_parent_dir_from_grandparent_with_subdirs(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "data".parse().unwrap(),
                        target_name: "minfs".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: Some("subdir_1".parse().unwrap()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Parent,
                        subdir: Some("subdir_2".parse().unwrap()),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model.add_subdir_to_data_directory("subdir_1");
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("subdir_1/subdir_2".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_test_subdir_contents(".", vec!["foo".to_string(), "subdir_1".to_string()])
            .await;
        model.check_test_subdir_contents("subdir_1", vec!["subdir_2".to_string()]).await;
        model.check_test_subdir_contents("subdir_1/subdir_2", vec!["c:0".to_string()]).await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data to b as /minfs
    /// b: has storage decl with name "mystorage" with a source of realm at path /minfs, subdir "bar"
    /// b: offers data storage to c from "mystorage"
    /// c: uses data storage as /storage
    pub async fn test_storage_from_parent_dir_from_grandparent_with_subdir(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "data".parse().unwrap(),
                        target_name: "minfs".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Parent,
                        subdir: Some("bar".parse().unwrap()),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("bar".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model.check_test_subdir_contents(".", vec!["bar".to_string(), "foo".to_string()]).await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: has storage decl with name "mystorage" with a source of self at path /data
    /// a: offers data storage to b from "mystorage"
    /// b: offers data storage to c from realm
    /// c: uses data storage as /storage
    pub async fn test_storage_and_dir_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data-root")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "data-root".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Parent,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["b:0", "c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// b: exposes directory /data as /minfs
    /// a: has storage decl with name "mystorage" with a source of child b at path /minfs
    /// a: offers cache storage to c from "mystorage"
    /// c: uses cache storage as /storage
    pub async fn test_storage_from_parent_dir_from_sibling(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Child("b".to_string()),
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "minfs".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// b: exposes directory /data as /minfs with subdir "subdir_1"
    /// a: has storage decl with name "mystorage" with a source of child b at path /minfs and subdir
    ///    "subdir_2"
    /// a: offers cache storage to c from "mystorage"
    /// c: uses cache storage as /storage
    pub async fn test_storage_from_parent_dir_from_sibling_with_subdir(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Child("b".to_string()),
                        subdir: Some("subdir_2".parse().unwrap()),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "minfs".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: Some("subdir_1".parse().unwrap()),
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model.add_subdir_to_data_directory("subdir_1");
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("subdir_1/subdir_2".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_test_subdir_contents(".", vec!["foo".to_string(), "subdir_1".to_string()])
            .await;
        model.check_test_subdir_contents("subdir_1", vec!["subdir_2".to_string()]).await;
        model.check_test_subdir_contents("subdir_1/subdir_2", vec!["c:0".to_string()]).await;
    }

    ///   a
    ///  / \
    /// b   c
    ///      \
    ///       d
    ///
    /// b: exposes directory /data as /minfs
    /// a: has storage decl with name "mystorage" with a source of child b at path /minfs
    /// a: offers data, cache, and meta storage to c from "mystorage"
    /// c: uses cache and meta storage as /storage
    /// c: offers data and meta storage to d
    /// d: uses data and meta storage
    pub async fn test_storage_multiple_types(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Child("b".to_string()),
                        subdir: Some(PathBuf::from("data")),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Child("b".to_string()),
                        subdir: Some(PathBuf::from("cache")),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "minfs".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Parent,
                        target: OfferTarget::static_child("d".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Parent,
                        target: OfferTarget::static_child("d".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("data".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/cache".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use(
                vec!["c", "d"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0", "d:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("data".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use(
                vec!["c", "d"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/cache".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["c:0", "d:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has storage decl with name "mystorage" with a source of self at path /storage
    /// a: offers cache storage to b from "mystorage"
    /// b: uses data storage as /storage, fails to since data != cache
    /// b: uses meta storage, fails to since meta != cache
    pub async fn test_use_the_wrong_type_of_storage(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "data".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: offers directory from self at path "/data"
    /// b: uses data storage as /storage, fails to since data storage != "/data" directories
    pub async fn test_directories_are_not_storage(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has storage decl with name "mystorage" with a source of self at path /data
    /// a: does not offer any storage to b
    /// b: uses meta storage and data storage as /storage, fails to since it was not offered either
    pub async fn test_use_storage_when_not_offered(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_lazy_child("b")
                    .directory(
                        DirectoryDeclBuilder::new("minfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data to b as /minfs, but a is non-executable
    /// b: has storage decl with name "mystorage" with a source of realm at path /minfs
    /// b: offers data and meta storage to b from "mystorage"
    /// c: uses meta and data storage as /storage, fails to since a is non-executable
    pub async fn test_dir_offered_from_nonexecutable(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "data".parse().unwrap(),
                        target_name: "minfs".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .storage(StorageDecl {
                        name: "data".parse().unwrap(),
                        backing_dir: "minfs".parse().unwrap(),
                        source: StorageDirectorySource::Parent,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "data".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   component manager's namespace
    ///    |
    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "mystorage" with a source of parent at path /data
    /// a: offers cache storage to b from "mystorage"
    /// b: uses cache storage as /storage.
    /// Policy prevents b from using storage.
    pub async fn test_storage_dir_from_cm_namespace_prevented_by_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "tmp".parse().unwrap(),
                        source: StorageDirectorySource::Parent,
                        subdir: Some(PathBuf::from("cache")),
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let namespace_capabilities = vec![CapabilityDecl::Directory(
            DirectoryDeclBuilder::new("tmp").path("/tmp").rights(fio::RW_STAR_DIR).build(),
        )];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(AbsoluteMoniker::root()),
                source_name: "cache".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Storage,
            },
            HashSet::new(),
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["b:0"]).unwrap(),
                    ),
                    from_cm_namespace: true,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Err(zx_status::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   component manager's namespace
    ///    |
    ///    a
    ///    |
    ///    b
    ///    |
    ///    c
    ///
    /// Instance IDs defined only for `b` in the component ID index.
    /// Check that the correct storage layout is used when a component has an instance ID.
    pub async fn test_instance_id_from_index(&self) {
        let b_instance_id = Some(gen_instance_id(&mut rand::thread_rng()));
        let component_id_index_path = make_index_file(component_id_index::Index {
            instances: vec![component_id_index::InstanceIdEntry {
                instance_id: b_instance_id.clone(),
                moniker: Some(AbsoluteMoniker::parse_str("/b").unwrap()),
            }],
            ..component_id_index::Index::default()
        })
        .unwrap();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(
                        DirectoryDeclBuilder::new("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR)
                            .build(),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .storage(StorageDecl {
                        name: "cache".parse().unwrap(),
                        backing_dir: "data".parse().unwrap(),
                        source: StorageDirectorySource::Self_,
                        subdir: None,
                        storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Parent,
                        target: OfferTarget::static_child("c".to_string()),
                        source_name: "cache".parse().unwrap(),
                        target_name: "cache".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Storage(UseStorageDecl {
                        source_name: "cache".parse().unwrap(),
                        target_path: "/storage".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let mut builder = T::new("a", components);
        builder.set_component_id_index_path(
            component_id_index_path.path().to_str().unwrap().to_string(),
        );
        let model = builder.build().await;

        // instance `b` uses instance-id based paths.
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(
                        InstancedAbsoluteMoniker::try_from(vec!["b:0"]).unwrap(),
                    ),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model.check_test_subdir_contains(".", b_instance_id.as_ref().unwrap().to_string()).await;

        // instance `c` uses moniker-based paths.
        let storage_relation = InstancedAbsoluteMoniker::try_from(vec!["b:0", "c:0"]).unwrap();
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(storage_relation.clone()),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        let expected_storage_path =
            generate_storage_path(None, &storage_relation, None).to_str().unwrap().to_string();
        model.check_test_dir_tree_contains(expected_storage_path).await;
    }
}
