// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod availability;
pub mod component_id_index;
pub mod policy;
pub mod rights;
pub mod storage;
pub mod storage_admin;

use {
    assert_matches::assert_matches,
    async_trait::async_trait,
    cm_moniker::InstancedMoniker,
    cm_rust::{
        Availability, CapabilityDecl, CapabilityTypeName, ChildRef, ComponentDecl, DependencyType,
        EventScope, EventStreamDecl, ExposeDecl, ExposeDirectoryDecl, ExposeProtocolDecl,
        ExposeRunnerDecl, ExposeServiceDecl, ExposeSource, ExposeTarget, NameMapping, OfferDecl,
        OfferDirectoryDecl, OfferEventStreamDecl, OfferProtocolDecl, OfferRunnerDecl,
        OfferServiceDecl, OfferSource, OfferTarget, ProgramDecl, ProtocolDecl, RegistrationSource,
        RunnerDecl, RunnerRegistration, ServiceDecl, UseDecl, UseDirectoryDecl, UseEventStreamDecl,
        UseProtocolDecl, UseServiceDecl, UseSource,
    },
    cm_rust_testing::{
        ChildDeclBuilder, ComponentDeclBuilder, DirectoryDeclBuilder, EnvironmentDeclBuilder,
        ProtocolDeclBuilder, TEST_RUNNER_NAME,
    },
    cm_types::Name,
    fidl::endpoints::ProtocolMarker,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio, fuchsia_zircon_status as zx,
    moniker::{ExtendedMoniker, Moniker, MonikerBase},
    routing::{
        capability_source::{
            AggregateCapability, CapabilitySource, ComponentCapability, InternalCapability,
        },
        component_id_index::ComponentInstanceId,
        component_instance::ComponentInstanceInterface,
        config::{
            AllowlistEntry, AllowlistEntryBuilder, CapabilityAllowlistKey,
            CapabilityAllowlistSource, DebugCapabilityAllowlistEntry, DebugCapabilityKey,
        },
        error::RoutingError,
        mapper::NoopRouteMapper,
        route_capability, RouteRequest, RouteSource,
    },
    std::{
        collections::{HashMap, HashSet},
        convert::TryInto,
        marker::PhantomData,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Construct a capability path for the hippo service.
pub fn default_service_capability() -> cm_types::Path {
    "/svc/hippo".parse().unwrap()
}

/// Construct a capability path for the hippo directory.
pub fn default_directory_capability() -> cm_types::Path {
    "/data/hippo".parse().unwrap()
}

/// Returns an empty component decl for an executable component.
pub fn default_component_decl() -> ComponentDecl {
    ComponentDecl::default()
}

/// Returns an empty component decl set up to have a non-empty program and to use the "test_runner"
/// runner.
pub fn component_decl_with_test_runner() -> ComponentDecl {
    ComponentDecl {
        program: Some(ProgramDecl {
            runner: Some(TEST_RUNNER_NAME.parse().unwrap()),
            info: fdata::Dictionary { entries: Some(vec![]), ..Default::default() },
        }),
        ..Default::default()
    }
}

/// Same as above but with the component also exposing Binder protocol.
pub fn component_decl_with_exposed_binder() -> ComponentDecl {
    ComponentDecl {
        program: Some(ProgramDecl {
            runner: Some(TEST_RUNNER_NAME.parse().unwrap()),
            info: fdata::Dictionary { entries: Some(vec![]), ..Default::default() },
        }),
        exposes: vec![ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Framework,
            source_name: fcomponent::BinderMarker::DEBUG_NAME.parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: fcomponent::BinderMarker::DEBUG_NAME.parse().unwrap(),
            availability: cm_rust::Availability::Required,
        })],
        ..Default::default()
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ExpectedResult {
    Ok,
    Err(zx::Status),
    ErrWithNoEpitaph,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ComponentEventRoute {
    /// Name of the component this was routed through
    pub component: String,
    /// Downscoping that was applied on behalf of the component
    /// None means that no downscoping was applied.
    /// Each String is the name of a child component to which the downscope
    /// applies.
    pub scope: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum ServiceInstance {
    Named(String),
    Aggregated(usize),
}

#[derive(Debug)]
pub enum CheckUse {
    Protocol {
        path: cm_types::Path,
        expected_res: ExpectedResult,
    },
    Service {
        path: cm_types::Path,
        instance: ServiceInstance,
        member: String,
        expected_res: ExpectedResult,
    },
    Directory {
        path: cm_types::Path,
        file: PathBuf,
        expected_res: ExpectedResult,
    },
    Storage {
        path: cm_types::Path,
        // The relative moniker from the storage declaration to the use declaration. Only
        // used if `expected_res` is Ok.
        storage_relation: Option<InstancedMoniker>,
        // The backing directory for this storage is in component manager's namespace, not the
        // test's isolated test directory.
        from_cm_namespace: bool,
        storage_subdir: Option<String>,
        expected_res: ExpectedResult,
    },
    StorageAdmin {
        // The relative moniker from the storage declaration to the use declaration.
        storage_relation: InstancedMoniker,
        // The backing directory for this storage is in component manager's namespace, not the
        // test's isolated test directory.
        from_cm_namespace: bool,

        storage_subdir: Option<String>,
        expected_res: ExpectedResult,
    },
    EventStream {
        path: cm_types::Path,
        scope: Vec<ComponentEventRoute>,
        name: Name,
        expected_res: ExpectedResult,
    },
}

impl CheckUse {
    pub fn default_directory(expected_res: ExpectedResult) -> Self {
        Self::Directory {
            path: default_directory_capability(),
            file: PathBuf::from("hippo"),
            expected_res,
        }
    }
}

// This function should reproduce the logic of `crate::storage::generate_storage_path`.
pub fn generate_storage_path(
    subdir: Option<String>,
    moniker: &InstancedMoniker,
    instance_id: Option<&ComponentInstanceId>,
) -> PathBuf {
    if let Some(id) = instance_id {
        return id.into();
    }
    let mut path = moniker.path().iter();
    let mut dir_path = vec![];
    if let Some(subdir) = subdir {
        dir_path.push(subdir);
    }
    if let Some(p) = path.next() {
        dir_path.push(p.to_string());
    }
    while let Some(p) = path.next() {
        dir_path.push("children".to_string());
        dir_path.push(p.to_string());
    }

    // Storage capabilities used to have a hardcoded set of types, which would be appended
    // here. To maintain compatibility with the old paths (and thus not lose data when this was
    // migrated) we append "data" here. This works because this is the only type of storage
    // that was actually used in the wild.
    //
    // This is only temporary, until the storage instance id migration changes this layout.
    dir_path.push("data".to_string());
    dir_path.into_iter().collect()
}

/// A `RoutingTestModel` attempts to use capabilities from instances in a component model
/// and checks the result of the attempt against an expectation.
#[async_trait]
pub trait RoutingTestModel {
    type C: ComponentInstanceInterface + std::fmt::Debug + 'static;

    /// Checks a `use` declaration at `moniker` by trying to use `capability`.
    async fn check_use(&self, moniker: Moniker, check: CheckUse);

    /// Checks using a capability from a component's exposed directory.
    async fn check_use_exposed_dir(&self, moniker: Moniker, check: CheckUse);

    /// Looks up a component instance by its absolute moniker.
    async fn look_up_instance(&self, moniker: &Moniker) -> Result<Arc<Self::C>, anyhow::Error>;

    /// Checks that a use declaration of `path` at `moniker` can be opened with
    /// Fuchsia file operations.
    async fn check_open_file(&self, moniker: Moniker, path: cm_types::Path);

    /// Create a file with the given contents in the test dir, along with any subdirectories
    /// required.
    async fn create_static_file(&self, path: &Path, contents: &str) -> Result<(), anyhow::Error>;

    /// Installs a new directory at `path` in the test's namespace.
    fn install_namespace_directory(&self, path: &str);

    /// Creates a subdirectory in the outgoing dir's /data directory.
    fn add_subdir_to_data_directory(&self, subdir: &str);

    /// Asserts that the subdir given by `path` within the test directory contains exactly the
    /// filenames in `expected`.
    async fn check_test_subdir_contents(&self, path: &str, expected: Vec<String>);

    /// Asserts that the directory at absolute `path` contains exactly the filenames in `expected`.
    async fn check_namespace_subdir_contents(&self, path: &str, expected: Vec<String>);

    /// Asserts that the subdir given by `path` within the test directory contains a file named `expected`.
    async fn check_test_subdir_contains(&self, path: &str, expected: String);

    /// Asserts that the tree in the test directory under `path` contains a file named `expected`.
    async fn check_test_dir_tree_contains(&self, expected: String);
}

/// Builds an implementation of `RoutingTestModel` from a set of `ComponentDecl`s.
#[async_trait]
pub trait RoutingTestModelBuilder {
    type Model: RoutingTestModel;

    /// Create a new builder. Both string arguments refer to component names, not URLs,
    /// ex: "a", not "test:///a" or "test:///a_resolved".
    fn new(root_component: &str, components: Vec<(&'static str, ComponentDecl)>) -> Self;

    /// Set the capabilities that should be available from the top instance's namespace.
    fn set_namespace_capabilities(&mut self, caps: Vec<CapabilityDecl>);

    /// Set the capabilities that should be available as built-in capabilities.
    fn set_builtin_capabilities(&mut self, caps: Vec<CapabilityDecl>);

    /// Register a mock `runner` in the built-in environment.
    fn register_mock_builtin_runner(&mut self, runner: &str);

    /// Add a custom capability security policy to restrict routing of certain caps.
    fn add_capability_policy(
        &mut self,
        key: CapabilityAllowlistKey,
        allowlist: HashSet<AllowlistEntry>,
    );

    /// Add a custom debug capability security policy to restrict routing of certain caps.
    fn add_debug_capability_policy(
        &mut self,
        key: DebugCapabilityKey,
        allowlist: HashSet<DebugCapabilityAllowlistEntry>,
    );

    /// Sets the path to the component ID index for the test model.
    fn set_component_id_index_path(&mut self, index_path: String);

    async fn build(self) -> Self::Model;
}

/// The CommonRoutingTests are run under multiple contexts, e.g. both on Fuchsia under
/// component_manager and on the build host under cm_fidl_analyzer. This macro helps ensure that all
/// tests are run in each context.
#[macro_export]
macro_rules! instantiate_common_routing_tests {
    ($builder_impl:path) => {
        // New CommonRoutingTest tests must be added to this list to run.
        instantiate_common_routing_tests! {
            $builder_impl,
            test_use_from_parent,
            test_use_from_child,
            test_use_from_self,
            test_use_from_grandchild,
            test_use_from_grandparent,
            test_use_from_sibling_no_root,
            test_use_from_sibling_root,
            test_use_from_niece,
            test_use_kitchen_sink,
            test_use_from_component_manager_namespace,
            test_offer_from_component_manager_namespace,
            test_use_not_offered,
            test_use_offer_source_not_exposed,
            test_use_offer_source_not_offered,
            test_use_from_expose,
            test_route_protocol_from_expose,
            test_use_from_expose_to_framework,
            test_offer_from_non_executable,
            test_route_aggregate_protocol_fails,
            test_route_aggregate_service,
            test_route_aggregate_service_without_filter_fails,
            test_route_aggregate_service_with_conflicting_filter_fails,
            test_use_directory_with_subdir_from_grandparent,
            test_use_directory_with_subdir_from_sibling,
            test_expose_directory_with_subdir,
            test_expose_from_self_and_child,
            test_use_not_exposed,
            test_use_protocol_denied_by_capability_policy,
            test_use_directory_with_alias_denied_by_capability_policy,
            test_use_protocol_partial_chain_allowed_by_capability_policy,
            test_use_protocol_component_provided_capability_policy,
            test_use_from_component_manager_namespace_denied_by_policy,
            test_use_protocol_component_provided_debug_capability_policy_at_root_from_self,
            test_use_protocol_component_provided_debug_capability_policy_from_self,
            test_use_protocol_component_provided_debug_capability_policy_from_child,
            test_use_protocol_component_provided_debug_capability_policy_from_grandchild,
            test_use_protocol_component_provided_wildcard_debug_capability_policy,
            test_event_stream_aliasing,
            test_use_event_stream_from_above_root,
            test_use_event_stream_from_above_root_and_downscoped,
            test_can_offer_capability_requested_event,
            test_route_service_from_parent,
            test_route_service_from_child,
            test_route_service_from_sibling,
            test_route_filtered_service_from_sibling,
            test_route_renamed_service_instance_from_sibling,
            test_use_builtin_from_grandparent,
            test_invalid_use_from_component_manager,
            test_invalid_offer_from_component_manager,
            test_route_runner_from_parent_environment,
            test_route_runner_from_grandparent_environment,
            test_route_runner_from_sibling_environment,
            test_route_runner_from_inherited_environment,
            test_route_runner_from_environment_not_found,
            test_route_builtin_runner,
            test_route_builtin_runner_from_root_env,
            test_route_builtin_runner_not_found,
            test_route_builtin_runner_from_root_env_registration_not_found,
        }
    };
    ($builder_impl:path, $test:ident, $($remaining:ident),+ $(,)?) => {
        instantiate_common_routing_tests! { $builder_impl, $test }
        instantiate_common_routing_tests! { $builder_impl, $($remaining),+ }
    };
    ($builder_impl:path, $test:ident) => {
        // TODO(fxbug.dev/77647): #[fuchsia::test] did not work inside a declarative macro, so this
        // falls back on fuchsia_async and manual logging initialization for now.
        #[fuchsia_async::run_singlethreaded(test)]
        async fn $test() {
            fuchsia::init_logging_for_component_with_executor(
                || {}, &[], fuchsia::Interest::default())();

            $crate::CommonRoutingTest::<$builder_impl>::new().$test().await
        }
    };
}

pub struct CommonRoutingTest<T: RoutingTestModelBuilder> {
    builder: PhantomData<T>,
}
impl<T: RoutingTestModelBuilder> CommonRoutingTest<T> {
    pub fn new() -> Self {
        Self { builder: PhantomData }
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: offers directory /data/foo from self as /data/bar
    /// a: offers service /svc/foo from self as /svc/bar
    /// a: offers service /svc/file from self as /svc/device
    /// b: uses directory /data/bar as /data/hippo
    /// b: uses service /svc/bar as /svc/hippo
    /// b: uses service /svc/device
    ///
    /// The test related to `/svc/file` is used to verify that services that require
    /// extended flags, like `OPEN_FLAG_DESCRIBE`, work correctly. This often
    /// happens for fuchsia.hardware protocols that compose fuchsia.io protocols,
    /// and expect that `fdio_open` should operate correctly.
    pub async fn test_use_from_parent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .protocol(ProtocolDeclBuilder::new("file").path("/svc/file").build())
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "file".parse().unwrap(),
                        target_name: "device".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "bar_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "bar_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "device".parse().unwrap(),
                        target_path: "/svc/device".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model.check_open_file(vec!["b"].try_into().unwrap(), "/svc/device".parse().unwrap()).await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: uses directory /data/bar from #b as /data/hippo
    /// a: uses service /svc/bar from #b as /svc/hippo
    /// b: exposes directory /data/foo from self as /data/bar
    /// b: exposes service /svc/foo from self as /svc/bar
    pub async fn test_use_from_child(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Child("b".to_string()),
                        source_name: "bar_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Child("b".to_string()),
                        source_name: "bar_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model.check_use(Moniker::root(), CheckUse::default_directory(ExpectedResult::Ok)).await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    /// a: uses protocol /svc/hippo from self
    pub async fn test_use_from_self(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("hippo").build())
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Self_,
                    source_name: "hippo".parse().unwrap(),
                    target_path: "/svc/hippo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        )];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
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
    /// a: uses /data/baz from #b as /data/hippo
    /// a: uses /svc/baz from #b as /svc/hippo
    /// b: exposes directory /data/bar from #c as /data/baz
    /// b: exposes service /svc/bar from #c as /svc/baz
    /// c: exposes directory /data/foo from self as /data/bar
    /// c: exposes service /svc/foo from self as /svc/bar
    pub async fn test_use_from_grandchild(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Child("b".to_string()),
                        source_name: "baz_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Child("b".to_string()),
                        source_name: "baz_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Child("c".to_string()),
                        source_name: "bar_data".parse().unwrap(),
                        target_name: "baz_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Child("c".to_string()),
                        source_name: "bar_svc".parse().unwrap(),
                        target_name: "baz_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model.check_use(Moniker::root(), CheckUse::default_directory(ExpectedResult::Ok)).await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
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
    /// a: offers directory /data/foo from self as /data/bar
    /// a: offers service /svc/foo from self as /svc/bar
    /// b: offers directory /data/bar from realm as /data/baz
    /// b: offers service /svc/bar from realm as /svc/baz
    /// c: uses /data/baz as /data/hippo
    /// c: uses /svc/baz as /svc/hippo
    pub async fn test_use_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Parent,
                        source_name: "bar_data".parse().unwrap(),
                        target_name: "baz_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "bar_svc".parse().unwrap(),
                        target_name: "baz_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "baz_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "baz_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
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
    /// a: offers service /svc/builtin.Echo from realm
    /// b: offers service /svc/builtin.Echo from realm
    /// c: uses /svc/builtin.Echo as /svc/hippo
    pub async fn test_use_builtin_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "builtin.Echo".parse().unwrap(),
                        target_name: "builtin.Echo".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "builtin.Echo".parse().unwrap(),
                        target_name: "builtin.Echo".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "builtin.Echo".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Protocol(ProtocolDecl {
            name: "builtin.Echo".parse().unwrap(),
            source_path: None,
        })]);
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///     a
    ///    /
    ///   b
    ///  / \
    /// d   c
    ///
    /// d: exposes directory /data/foo from self as /data/bar
    /// b: offers directory /data/bar from d as /data/foobar to c
    /// c: uses /data/foobar as /data/hippo
    pub async fn test_use_from_sibling_no_root(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("d".to_string()),
                        source_name: "bar_data".parse().unwrap(),
                        target_name: "foobar_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("d".to_string()),
                        source_name: "bar_svc".parse().unwrap(),
                        target_name: "foobar_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "foobar_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "foobar_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// b: exposes directory /data/foo from self as /data/bar
    /// a: offers directory /data/bar from b as /data/baz to c
    /// c: uses /data/baz as /data/hippo
    pub async fn test_use_from_sibling_root(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "bar_data".parse().unwrap(),
                        target_name: "baz_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "bar_svc".parse().unwrap(),
                        target_name: "baz_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "baz_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "baz_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///     a
    ///    / \
    ///   b   c
    ///  /
    /// d
    ///
    /// d: exposes directory /data/foo from self as /data/bar
    /// b: exposes directory /data/bar from d as /data/baz
    /// a: offers directory /data/baz from b as /data/foobar to c
    /// c: uses /data/foobar as /data/hippo
    pub async fn test_use_from_niece(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "baz_data".parse().unwrap(),
                        target_name: "foobar_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "baz_svc".parse().unwrap(),
                        target_name: "foobar_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Child("d".to_string()),
                        source_name: "bar_data".parse().unwrap(),
                        target_name: "baz_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Child("d".to_string()),
                        source_name: "bar_svc".parse().unwrap(),
                        target_name: "baz_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "foobar_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "foobar_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///      a
    ///     / \
    ///    /   \
    ///   b     c
    ///  / \   / \
    /// d   e f   g
    ///            \
    ///             h
    ///
    /// a,d,h: hosts /svc/foo and /data/foo
    /// e: uses /svc/foo as /svc/hippo from a, uses /data/foo as /data/hippo from d
    /// f: uses /data/foo from d as /data/hippo, uses /svc/foo from h as /svc/hippo
    pub async fn test_use_kitchen_sink(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "foo_from_a_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "foo_from_d_data".parse().unwrap(),
                        target_name: "foo_from_d_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new_empty_component()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("d".to_string()),
                        source_name: "foo_from_d_data".parse().unwrap(),
                        target_name: "foo_from_d_data".parse().unwrap(),
                        target: OfferTarget::static_child("e".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "foo_from_a_svc".parse().unwrap(),
                        target_name: "foo_from_a_svc".parse().unwrap(),
                        target: OfferTarget::static_child("e".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Child("d".to_string()),
                        source_name: "foo_from_d_data".parse().unwrap(),
                        target_name: "foo_from_d_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("d")
                    .add_lazy_child("e")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new_empty_component()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Parent,
                        source_name: "foo_from_d_data".parse().unwrap(),
                        target_name: "foo_from_d_data".parse().unwrap(),
                        target: OfferTarget::static_child("f".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("g".to_string()),
                        source_name: "foo_from_h_svc".parse().unwrap(),
                        target_name: "foo_from_h_svc".parse().unwrap(),
                        target: OfferTarget::static_child("f".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("f")
                    .add_lazy_child("g")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "foo_from_d_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "foo_from_d_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "foo_from_a_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "f",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "foo_from_d_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "foo_from_h_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "g",
                ComponentDeclBuilder::new_empty_component()
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Child("h".to_string()),
                        source_name: "foo_from_h_svc".parse().unwrap(),
                        target_name: "foo_from_h_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("h")
                    .build(),
            ),
            (
                "h",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "foo_from_h_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "e"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b", "e"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use(
                vec!["c", "f"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["c", "f"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///
    /// a: uses directory /use_from_cm_namespace/data/foo as foo_data
    /// a: uses service /use_from_cm_namespace/svc/foo as foo_svc
    pub async fn test_use_from_component_manager_namespace(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    source: UseSource::Parent,
                    source_name: "foo_data".parse().unwrap(),
                    target_path: "/data/hippo".parse().unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Parent,
                    source_name: "foo_svc".parse().unwrap(),
                    target_path: "/svc/hippo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        )];
        let namespace_capabilities = vec![
            CapabilityDecl::Directory(
                DirectoryDeclBuilder::new("foo_data")
                    .path("/use_from_cm_namespace/data/foo")
                    .build(),
            ),
            CapabilityDecl::Protocol(
                ProtocolDeclBuilder::new("foo_svc").path("/use_from_cm_namespace/svc/foo").build(),
            ),
        ];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        let model = builder.build().await;

        model.install_namespace_directory("/use_from_cm_namespace");
        model.check_use(Moniker::root(), CheckUse::default_directory(ExpectedResult::Ok)).await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///    \
    ///     b
    ///
    /// a: offers directory /offer_from_cm_namespace/data/foo from realm as bar_data
    /// a: offers service /offer_from_cm_namespace/svc/foo from realm as bar_svc
    /// b: uses directory bar_data as /data/hippo
    /// b: uses service bar_svc as /svc/hippo
    pub async fn test_offer_from_component_manager_namespace(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Parent,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "bar_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "bar_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let namespace_capabilities = vec![
            CapabilityDecl::Directory(
                DirectoryDeclBuilder::new("foo_data")
                    .path("/offer_from_cm_namespace/data/foo")
                    .build(),
            ),
            CapabilityDecl::Protocol(
                ProtocolDeclBuilder::new("foo_svc")
                    .path("/offer_from_cm_namespace/svc/foo")
                    .build(),
            ),
        ];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        let model = builder.build().await;

        model.install_namespace_directory("/offer_from_cm_namespace");
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses directory /data/hippo as /data/hippo, but it's not in its realm
    /// b: uses service /svc/hippo as /svc/hippo, but it's not in its realm
    pub async fn test_use_not_offered(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new_empty_component().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offers directory /data/hippo from b as /data/hippo, but it's not exposed by b
    /// a: offers service /svc/hippo from b as /svc/hippo, but it's not exposed by b
    /// c: uses directory /data/hippo as /data/hippo
    /// c: uses service /svc/hippo as /svc/hippo
    pub async fn test_use_offer_source_not_exposed(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source_name: "hippo_data".parse().unwrap(),
                        source: OfferSource::static_child("b".to_string()),
                        target_name: "hippo_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source_name: "hippo_svc".parse().unwrap(),
                        source: OfferSource::static_child("b".to_string()),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            ("b", component_decl_with_test_runner()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
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
    /// b: offers directory /data/hippo from its realm as /data/hippo, but it's not offered by a
    /// b: offers service /svc/hippo from its realm as /svc/hippo, but it's not offfered by a
    /// c: uses directory /data/hippo as /data/hippo
    /// c: uses service /svc/hippo as /svc/hippo
    pub async fn test_use_offer_source_not_offered(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new_empty_component()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source_name: "hippo_data".parse().unwrap(),
                        source: OfferSource::Parent,
                        target_name: "hippo_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source_name: "hippo_svc".parse().unwrap(),
                        source: OfferSource::Parent,
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let test = T::new("a", components).build().await;
        test.check_use(
            vec!["b", "c"].try_into().unwrap(),
            CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
        )
        .await;
        test.check_use(
            vec!["b", "c"].try_into().unwrap(),
            CheckUse::Protocol {
                path: default_service_capability(),
                expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
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
    /// b: uses directory /data/hippo as /data/hippo, but it's exposed to it, not offered
    /// b: uses service /svc/hippo as /svc/hippo, but it's exposed to it, not offered
    /// c: exposes /data/hippo
    /// c: exposes /svc/hippo
    pub async fn test_use_from_expose(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new_empty_component().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("hippo_data").build())
                    .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "hippo_data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "hippo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source_name: "hippo_svc".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "hippo_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    /// a
    ///  \
    ///   b
    ///
    /// a: exposes "foo" to parent from child
    /// b: exposes "foo" to parent from self
    pub async fn test_route_protocol_from_expose(&self) {
        let expose_decl = ExposeProtocolDecl {
            source: ExposeSource::Child("b".parse().unwrap()),
            source_name: "foo".parse().unwrap(),
            target_name: "foo".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let expected_protocol_decl = ProtocolDecl {
            name: "foo".parse().unwrap(),
            source_path: Some("/svc/foo".parse().unwrap()),
        };

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Protocol(expose_decl.clone()))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .protocol(expected_protocol_decl.clone())
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let root_instance = model.look_up_instance(&Moniker::root()).await.expect("root instance");
        let expected_source_moniker = Moniker::parse_str("/b").unwrap();

        assert_matches!(
        route_capability(RouteRequest::ExposeProtocol(expose_decl), &root_instance, &mut NoopRouteMapper).await,
            Ok(RouteSource {
                source: CapabilitySource::<
                    <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C
                    >::Component {
                        capability: ComponentCapability::Protocol(protocol_decl),
                        component,
                    },
                relative_path,
            }) if protocol_decl == expected_protocol_decl && component.moniker == expected_source_moniker && relative_path == PathBuf::new()
        );
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// b: exposes directory /data/foo from self as /data/bar to framework (NOT realm)
    /// a: offers directory /data/bar from b as /data/baz to c, but it is not exposed via realm
    /// c: uses /data/baz as /data/hippo
    pub async fn test_use_from_expose_to_framework(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "bar_data".parse().unwrap(),
                        target_name: "baz_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "bar_svc".parse().unwrap(),
                        target_name: "baz_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: ExposeTarget::Framework,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "bar_svc".parse().unwrap(),
                        target: ExposeTarget::Framework,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "baz_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "baz_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: offers directory /data/hippo to b, but a is not executable
    /// a: offers service /svc/hippo to b, but a is not executable
    /// b: uses directory /data/hippo as /data/hippo, but it's not in its realm
    /// b: uses service /svc/hippo as /svc/hippo, but it's not in its realm
    pub async fn test_offer_from_non_executable(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .directory(DirectoryDeclBuilder::new("hippo_data").build())
                    .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source_name: "hippo_data".parse().unwrap(),
                        source: OfferSource::Self_,
                        target_name: "hippo_data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source_name: "hippo_svc".parse().unwrap(),
                        source: OfferSource::Self_,
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   a
    /// / | \
    /// b c d
    ///
    /// a: offers "foo" from both b and c to d
    /// b: exposes "foo" to parent from self
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate protocol should fail
    pub async fn test_route_aggregate_protocol_fails(&self) {
        let expected_protocol_decl = ProtocolDecl {
            name: "foo".parse().unwrap(),
            source_path: Some("/svc/foo".parse().unwrap()),
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("b".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .protocol(expected_protocol_decl.clone())
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .protocol(expected_protocol_decl.clone())
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "foo".parse().unwrap(),
                        target_path: "/svc/foo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;

        let d_component =
            model.look_up_instance(&vec!["d"].try_into().unwrap()).await.expect("b instance");
        assert_matches!(
            route_capability(
                RouteRequest::UseProtocol(UseProtocolDecl {
                    source: UseSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    target_path: "/svc/foo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }),
                &d_component,
                &mut NoopRouteMapper
            )
            .await,
            Err(RoutingError::UnsupportedRouteSource { source_type: _ })
        );
    }

    ///   a
    /// / | \
    /// b c d
    ///
    /// a: offers "foo" from both b and c to d
    /// b: exposes "foo" to parent from self
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate service with non-conflicting filters should succeed.
    pub async fn test_route_aggregate_service(&self) {
        let expected_service_decl = ServiceDecl {
            name: "foo".parse().unwrap(),
            source_path: Some("/svc/foo".parse().unwrap()),
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("b".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        source_instance_filter: Some(vec![
                            "instance_0".to_string(),
                            "instance_1".to_string(),
                        ]),
                        renamed_instances: None,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        source_instance_filter: Some(vec![
                            "instance_2".to_string(),
                            "instance_3".to_string(),
                        ]),
                        renamed_instances: None,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(expected_service_decl.clone())
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(expected_service_decl.clone())
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Service(UseServiceDecl {
                        source: UseSource::Parent,
                        source_name: "foo".parse().unwrap(),
                        target_path: "/svc/foo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;

        let d_component =
            model.look_up_instance(&vec!["d"].try_into().unwrap()).await.expect("b instance");

        let source = route_capability(
            RouteRequest::UseService(UseServiceDecl {
                source: UseSource::Parent,
                source_name: "foo".parse().unwrap(),
                target_path: "/svc/foo".parse().unwrap(),
                dependency_type: DependencyType::Strong,
                availability: Availability::Required,
            }),
            &d_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::OfferAggregate {
                        capability: AggregateCapability::Service(name),
                        ..
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "foo");
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    /// / | \
    /// b c d
    ///
    /// a: offers "foo" from both b and c to d
    /// b: exposes "foo" to parent from self
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate service without specifying a source_instance_filter should fail.
    pub async fn test_route_aggregate_service_without_filter_fails(&self) {
        let expected_service_decl = ServiceDecl {
            name: "foo".parse().unwrap(),
            source_path: Some("/svc/foo".parse().unwrap()),
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("b".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        source_instance_filter: None,
                        renamed_instances: None,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        source_instance_filter: None,
                        renamed_instances: None,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(expected_service_decl.clone())
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(expected_service_decl.clone())
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Service(UseServiceDecl {
                        source: UseSource::Parent,
                        source_name: "foo".parse().unwrap(),
                        target_path: "/svc/foo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;

        let d_component =
            model.look_up_instance(&vec!["d"].try_into().unwrap()).await.expect("b instance");
        assert_matches!(
            route_capability(
                RouteRequest::UseService(UseServiceDecl {
                    source: UseSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    target_path: "/svc/foo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }),
                &d_component,
                &mut NoopRouteMapper
            )
            .await,
            Err(RoutingError::UnsupportedRouteSource { source_type: _ })
        );
    }

    ///   a
    /// / | \
    /// b c d
    ///
    /// a: offers "foo" from both b and c to d
    /// b: exposes "foo" to parent from self
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate service with conflicting source_instance_filters should fail.
    pub async fn test_route_aggregate_service_with_conflicting_filter_fails(&self) {
        let expected_service_decl = ServiceDecl {
            name: "foo".parse().unwrap(),
            source_path: Some("/svc/foo".parse().unwrap()),
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("b".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        source_instance_filter: Some(vec![
                            "default".to_string(),
                            "other_a".to_string(),
                        ]),
                        renamed_instances: None,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        source_instance_filter: Some(vec![
                            "default".to_string(),
                            "other_b".to_string(),
                        ]),
                        renamed_instances: None,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(expected_service_decl.clone())
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(expected_service_decl.clone())
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Service(UseServiceDecl {
                        source: UseSource::Parent,
                        source_name: "foo".parse().unwrap(),
                        target_path: "/svc/foo".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;

        let d_component =
            model.look_up_instance(&vec!["d"].try_into().unwrap()).await.expect("b instance");
        assert_matches!(
            route_capability(
                RouteRequest::UseService(UseServiceDecl {
                    source: UseSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    target_path: "/svc/foo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }),
                &d_component,
                &mut NoopRouteMapper
            )
            .await,
            Err(RoutingError::UnsupportedRouteSource { source_type: _ })
        );
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data/foo from self with subdir 's1/s2'
    /// b: offers directory /data/foo from realm with subdir 's3'
    /// c: uses /data/foo as /data/hippo
    pub async fn test_use_directory_with_subdir_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "foo_data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: Some(PathBuf::from("s1/s2")),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Parent,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "foo_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: Some(PathBuf::from("s3")),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "foo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: Some(PathBuf::from("s4")),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .create_static_file(Path::new("foo/s1/s2/s3/s4/inner"), "hello")
            .await
            .expect("failed to create file");
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: default_directory_capability(),
                    file: PathBuf::from("inner"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    ///
    /// b: exposes directory /data/foo from self with subdir 's1/s2'
    /// a: offers directory /data/foo from `b` to `c` with subdir 's3'
    /// c: uses /data/foo as /data/hippo
    pub async fn test_use_directory_with_subdir_from_sibling(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::static_child("b".to_string()),
                        source_name: "foo_data".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        target_name: "foo_data".parse().unwrap(),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: Some(PathBuf::from("s3")),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        target_name: "foo_data".parse().unwrap(),
                        rights: Some(fio::R_STAR_DIR),
                        subdir: Some(PathBuf::from("s1/s2")),
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "foo_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .create_static_file(Path::new("foo/s1/s2/s3/inner"), "hello")
            .await
            .expect("failed to create file");
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: default_directory_capability(),
                    file: PathBuf::from("inner"),
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
    /// c: exposes /data/foo from self
    /// b: exposes /data/foo from `c` with subdir `s1/s2`
    /// a: exposes /data/foo from `b` with subdir `s3` as /data/hippo
    /// use /data/hippo from a's exposed dir
    pub async fn test_expose_directory_with_subdir(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Child("b".to_string()),
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "hippo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: None,
                        subdir: Some(PathBuf::from("s3")),
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Child("c".to_string()),
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "foo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: None,
                        subdir: Some(PathBuf::from("s1/s2")),
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "foo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .create_static_file(Path::new("foo/s1/s2/s3/inner"), "hello")
            .await
            .expect("failed to create file");
        model
            .check_use_exposed_dir(
                Moniker::root(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("inner"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    pub async fn test_expose_from_self_and_child(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Child("c".to_string()),
                        source_name: "hippo_data".parse().unwrap(),
                        target_name: "hippo_bar_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Child("c".to_string()),
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_bar_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "hippo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_bar_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo_bar_svc".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo_svc".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    pub async fn test_use_not_exposed(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_lazy_child("c").build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "hippo_data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::R_STAR_DIR),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        // Capability is only exposed from "c", so it only be usable from there.

        // When trying to open a capability that's not exposed to realm, there's no node for it in the
        // exposed dir, so no routing takes place.
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo_svc".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo_svc".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   (cm)
    ///    |
    ///    a
    ///
    /// a: uses an invalid service from the component manager.
    pub async fn test_invalid_use_from_component_manager(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Parent,
                    source_name: "invalid".parse().unwrap(),
                    target_path: "/svc/valid".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        )];

        // Try and use the service. We expect a failure.
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: "/svc/valid".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    ///   (cm)
    ///    |
    ///    a
    ///    |
    ///    b
    ///
    /// a: offers an invalid service from the component manager to "b".
    /// b: attempts to use the service
    pub async fn test_invalid_offer_from_component_manager(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source_name: "invalid".parse().unwrap(),
                        source: OfferSource::Parent,
                        target_name: "valid".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "valid".parse().unwrap(),
                        target_path: "/svc/valid".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        // Try and use the service. We expect a failure.
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/svc/valid".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
                },
            )
            .await;
    }

    /*
    TODO(https://fxbug.dev/107902): Allow exposing from parent.

    /// Tests exposing an event_stream from a child through its parent down to another
    /// unrelated child.
    ///        a
    ///         \
    ///          b
    ///          /\
    ///          c f
    ///          /\
    ///          d e
    /// c exposes started with a scope of e (but not d)
    /// to b, which then offers that to f.
    pub async fn test_expose_event_stream_with_scope(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Child(ChildRef {
                            name: "c".to_string(),
                            collection: None,
                        }),
                        source_name: "started".parse().unwrap(),
                        scope: None,
                        filter: None,
                        target: OfferTarget::Child(ChildRef {
                            name: "f".to_string(),
                            collection: None,
                        }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .add_lazy_child("f")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::EventStream(ExposeEventStreamDecl {
                        source: ExposeSource::Framework,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![EventScope::Child(ChildRef {
                            name: "e".to_string(),
                            collection: None,
                        })]),
                        target: ExposeTarget::Parent,
                        target_name: "started".parse().unwrap(),
                    }))
                    .add_lazy_child("d")
                    .add_lazy_child("e")
                    .build(),
            ),
            ("d", ComponentDeclBuilder::new().build()),
            ("e", ComponentDeclBuilder::new().build()),
            (
                "f",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
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
            .check_use(
                vec!["b", "f"].into(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![
                        ComponentEventRoute {
                            component: "c".to_string(),
                            scope: Some(vec!["e".to_string()]),
                        },
                        ComponentEventRoute { component: "b".to_string(), scope: None },
                    ],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }*/

    /// Tests event stream aliasing (scoping rules are applied correctly)
    ///        root
    ///        |
    ///        a
    ///       /|\
    ///      b c d
    /// A offers started to b with scope b, A offers started to c with scope d,
    /// A offers started to d with scope c.
    pub async fn test_event_stream_aliasing(&self) {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![EventScope::Child(ChildRef {
                            name: "b".into(),
                            collection: None,
                        })]),
                        filter: None,
                        target: OfferTarget::Child(ChildRef { name: "b".into(), collection: None }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![EventScope::Child(ChildRef {
                            name: "c".into(),
                            collection: None,
                        })]),
                        filter: None,
                        target: OfferTarget::Child(ChildRef { name: "d".into(), collection: None }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![EventScope::Child(ChildRef {
                            name: "d".into(),
                            collection: None,
                        })]),
                        filter: None,
                        target: OfferTarget::Child(ChildRef { name: "c".into(), collection: None }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
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
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["b".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["d".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;

        model
            .check_use(
                vec!["d"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["c".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses framework events "started", and "capability_requested"
    pub async fn test_use_event_stream_from_above_root(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::EventStream(UseEventStreamDecl {
                    source: UseSource::Parent,
                    source_name: "started".parse().unwrap(),
                    target_path: "/event/stream".parse().unwrap(),
                    scope: None,
                    filter: None,
                    availability: Availability::Required,
                }))
                .build(),
        )];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "started".parse().unwrap(),
        })]);

        let model = builder.build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///   /\
    ///  b  c
    ///    / \
    ///   d   e
    /// c: uses framework events "started", and "capability_requested",
    /// scoped to b and c.
    /// d receives started which is scoped to b, c, and e.
    pub async fn test_use_event_stream_from_above_root_and_downscoped(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![
                            EventScope::Child(ChildRef { name: "b".into(), collection: None }),
                            EventScope::Child(ChildRef { name: "c".into(), collection: None }),
                        ]),
                        filter: None,
                        target: OfferTarget::Child(ChildRef { name: "b".into(), collection: None }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![
                            EventScope::Child(ChildRef { name: "b".into(), collection: None }),
                            EventScope::Child(ChildRef { name: "c".into(), collection: None }),
                        ]),
                        filter: None,
                        target: OfferTarget::Child(ChildRef { name: "c".into(), collection: None }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "started".parse().unwrap(),
                        scope: Some(vec![EventScope::Child(ChildRef {
                            name: "e".into(),
                            collection: None,
                        })]),
                        filter: None,
                        target: OfferTarget::Child(ChildRef { name: "d".into(), collection: None }),
                        target_name: "started".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("d")
                    .add_lazy_child("e")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "started".parse().unwrap(),
                        target_path: "/event/stream".parse().unwrap(),
                        scope: None,
                        filter: None,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            ("e", ComponentDeclBuilder::new().build()),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "started".parse().unwrap(),
        })]);

        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["b".to_string(), "c".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["b".to_string(), "c".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
        model
            .check_use(
                vec!["c", "d"].try_into().unwrap(), // Should get e's event from parent
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![
                        ComponentEventRoute {
                            component: "/".to_string(),
                            scope: Some(vec!["b".to_string(), "c".to_string()]),
                        },
                        ComponentEventRoute {
                            component: "c".to_string(),
                            scope: Some(vec!["e".to_string()]),
                        },
                    ],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a; attempts to offer event "capability_requested" to b.
    pub async fn test_can_offer_capability_requested_event(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                        source: OfferSource::Parent,
                        source_name: "capability_requested".parse().unwrap(),
                        target_name: "capability_requested_on_a".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        filter: None,
                        scope: None,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::EventStream(UseEventStreamDecl {
                        source: UseSource::Parent,
                        source_name: "capability_requested_on_a".parse().unwrap(),
                        target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                        filter: None,
                        scope: None,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "capability_requested".parse().unwrap(),
        })]);
        let model = builder.build().await;

        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                    scope: vec![ComponentEventRoute { component: "/".to_string(), scope: None }],
                    name: "capability_requested_on_a".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses service /svc/hippo as /svc/hippo.
    /// a: provides b with the service but policy prevents it.
    pub async fn test_use_protocol_denied_by_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "hippo_svc".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            HashSet::new(),
        );

        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses directory /data/foo as /data/bar.
    /// a: provides b with the directory but policy prevents it.
    pub async fn test_use_directory_with_alias_denied_by_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDeclBuilder::new("foo_data").build())
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "foo_data".parse().unwrap(),
                        target_name: "bar_data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::R_STAR_DIR),
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
                    .use_(UseDecl::Directory(UseDirectoryDecl {
                        source: UseSource::Parent,
                        source_name: "bar_data".parse().unwrap(),
                        target_path: "/data/hippo".parse().unwrap(),
                        rights: fio::R_STAR_DIR,
                        subdir: None,
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "foo_data".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Directory,
            },
            HashSet::new(),
        );
        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::ACCESS_DENIED)),
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    /// c: uses service /svc/hippo as /svc/hippo.
    /// b: uses service /svc/hippo as /svc/hippo.
    /// a: provides b with the service policy allows it.
    /// b: provides c with the service policy does not allow it.
    pub async fn test_use_protocol_partial_chain_allowed_by_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(AllowlistEntryBuilder::new().exact("b").build());

        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "hippo_svc".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///    /  \
    ///   c    d
    /// b: provides d with the service policy allows denies it.
    /// b: provides c with the service policy allows it.
    /// c: uses service /svc/hippo as /svc/hippo.
    /// Tests component provided caps in the middle of a path
    pub async fn test_use_protocol_component_provided_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_name: "hippo_svc".parse().unwrap(),
                        target: OfferTarget::static_child("d".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("c")
                    .add_lazy_child("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Parent,
                        source_name: "hippo_svc".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(AllowlistEntryBuilder::new().exact("b").build());
        allowlist.insert(AllowlistEntryBuilder::new().exact("b").exact("c").build());

        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "hippo_svc".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "d"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///
    /// a: uses service /use_from_cm_namespace/svc/foo as foo_svc
    pub async fn test_use_from_component_manager_namespace_denied_by_policy(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "foo_svc".parse().unwrap(),
                    target_path: "/svc/hippo".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        )];
        let namespace_capabilities = vec![CapabilityDecl::Protocol(
            ProtocolDeclBuilder::new("foo_svc").path("/use_from_cm_namespace/svc/foo").build(),
        )];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentManager,
                source_name: "foo_svc".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            HashSet::new(),
        );
        let model = builder.build().await;

        model.install_namespace_directory("/use_from_cm_namespace");
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///    /  \
    ///   c    d
    /// b: provides d with the service policy allows denies it.
    /// b: provides c with the service policy allows it.
    /// c: uses service svc_allowed as /svc/hippo.
    /// a: offers services using environment.
    /// Tests component provided caps in the middle of a path
    pub async fn test_use_protocol_component_provided_debug_capability_policy_at_root_from_self(
        &self,
    ) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("svc_allowed").build())
                    .protocol(ProtocolDeclBuilder::new("svc_not_allowed").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env_a")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_allowed".parse().unwrap(),
                                    target_name: "svc_allowed".parse().unwrap(),
                                    source: RegistrationSource::Self_,
                                },
                            ))
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_not_allowed".parse().unwrap(),
                                    target_name: "svc_not_allowed".parse().unwrap(),
                                    source: RegistrationSource::Self_,
                                },
                            )),
                    )
                    .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env_a"))
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("c"))
                    .add_child(ChildDeclBuilder::new_lazy_child("d"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(DebugCapabilityAllowlistEntry::new(
            AllowlistEntryBuilder::build_exact_from_moniker(&Moniker::root()),
            AllowlistEntryBuilder::build_exact_from_moniker(&Moniker::root()),
        ));

        let mut builder = T::new("a", components);
        builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "svc_allowed".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "env_a".to_string(),
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "d"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///    /  \
    ///   c    d
    /// b: provides d with the service policy allows denies it.
    /// b: provides c with the service policy allows it.
    /// c: uses service svc_allowed as /svc/hippo.
    /// b: offers services using environment.
    /// Tests component provided debug caps in the middle of a path
    pub async fn test_use_protocol_component_provided_debug_capability_policy_from_self(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("b"))
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("svc_allowed").build())
                    .protocol(ProtocolDeclBuilder::new("svc_not_allowed").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env_b")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_allowed".parse().unwrap(),
                                    target_name: "svc_allowed".parse().unwrap(),
                                    source: RegistrationSource::Self_,
                                },
                            ))
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_not_allowed".parse().unwrap(),
                                    target_name: "svc_not_allowed".parse().unwrap(),
                                    source: RegistrationSource::Self_,
                                },
                            )),
                    )
                    .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env_b"))
                    .add_child(ChildDeclBuilder::new_lazy_child("d").environment("env_b"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(DebugCapabilityAllowlistEntry::new(
            AllowlistEntryBuilder::new().exact("b").build(),
            AllowlistEntryBuilder::new().exact("b").build(),
        ));

        let mut builder = T::new("a", components);
        builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "svc_allowed".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "env_b".to_string(),
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "d"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///   / \ \
    ///  c  d  e
    /// b: provides e with the service policy which denies it.
    /// b: provides c with the service policy which allows it.
    /// c: uses service svc_allowed as /svc/hippo.
    /// b: offers services using environment.
    /// d: exposes the service to b
    /// Tests component provided debug caps in the middle of a path
    pub async fn test_use_protocol_component_provided_debug_capability_policy_from_child(&self) {
        let expose_decl_svc_allowed = ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "svc_allowed".parse().unwrap(),
            target_name: "svc_allowed".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let expose_decl_svc_not_allowed = ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "svc_not_allowed".parse().unwrap(),
            target_name: "svc_not_allowed".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env_b")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_allowed".parse().unwrap(),
                                    target_name: "svc_allowed".parse().unwrap(),
                                    source: RegistrationSource::Child("d".to_string()),
                                },
                            ))
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_not_allowed".parse().unwrap(),
                                    target_name: "svc_not_allowed".parse().unwrap(),
                                    source: RegistrationSource::Child("d".to_string()),
                                },
                            )),
                    )
                    .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env_b"))
                    .add_child(ChildDeclBuilder::new_lazy_child("d"))
                    .add_child(ChildDeclBuilder::new_lazy_child("e").environment("env_b"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("svc_allowed").build())
                    .expose(cm_rust::ExposeDecl::Protocol(expose_decl_svc_allowed))
                    .protocol(ProtocolDeclBuilder::new("svc_not_allowed").build())
                    .expose(cm_rust::ExposeDecl::Protocol(expose_decl_svc_not_allowed))
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(DebugCapabilityAllowlistEntry::new(
            AllowlistEntryBuilder::new().exact("b").exact("d").build(),
            AllowlistEntryBuilder::new().exact("b").build(),
        ));

        let mut builder = T::new("a", components);
        builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "svc_allowed".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "env_b".to_string(),
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "e"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///    a
    ///     \
    ///      b
    ///     / \
    ///    c   d
    ///         \
    ///          e
    /// b: defines an environment with a debug protocols exposed by d, 1 allowed and 1 not allowed
    /// b: provides c with the environment
    /// c: uses service svc_allowed as /svc/hippo.
    /// c: uses service svc_not_allowed as /svc/hippo_not_allowed
    /// d: exposes the service to b
    /// e: exposes the service to d
    /// Tests component provided debug caps in the middle of a path
    pub async fn test_use_protocol_component_provided_debug_capability_policy_from_grandchild(
        &self,
    ) {
        let expose_decl_svc_allowed = ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "svc_allowed".parse().unwrap(),
            target_name: "svc_allowed".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let expose_decl_svc_not_allowed = ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "svc_not_allowed".parse().unwrap(),
            target_name: "svc_not_allowed".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env_b")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_allowed".parse().unwrap(),
                                    target_name: "svc_allowed".parse().unwrap(),
                                    source: RegistrationSource::Child("d".to_string()),
                                },
                            ))
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_not_allowed".parse().unwrap(),
                                    target_name: "svc_not_allowed".parse().unwrap(),
                                    source: RegistrationSource::Child("d".to_string()),
                                },
                            )),
                    )
                    .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env_b"))
                    .add_child(ChildDeclBuilder::new_lazy_child("d"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_path: "/svc/hippo_not_allowed".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .expose(cm_rust::ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Child("e".parse().unwrap()),
                        source_name: "svc_allowed".parse().unwrap(),
                        target_name: "svc_allowed".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(cm_rust::ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Child("e".parse().unwrap()),
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_name: "svc_not_allowed".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_lazy_child("e")
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("svc_allowed").build())
                    .expose(cm_rust::ExposeDecl::Protocol(expose_decl_svc_allowed))
                    .protocol(ProtocolDeclBuilder::new("svc_not_allowed").build())
                    .expose(cm_rust::ExposeDecl::Protocol(expose_decl_svc_not_allowed))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(DebugCapabilityAllowlistEntry::new(
            AllowlistEntryBuilder::new().exact("b").exact("d").exact("e").build(),
            AllowlistEntryBuilder::new().exact("b").build(),
        ));

        let mut builder = T::new("a", components);
        builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "svc_allowed".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "env_b".to_string(),
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/svc/hippo_not_allowed".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c   d
    ///   \
    ///    e
    /// c: defines an environment with debug protocols exposed by d, 1 allowed and 1 not allowed
    /// b: offers protocols from d to c
    /// e: uses service svc_allowed as /svc/hippo.
    /// e: uses service svc_not_allowed as /svc/hippo_not_allowed
    /// Tests component provided debug caps given wildcard selectors
    pub async fn test_use_protocol_component_provided_wildcard_debug_capability_policy(&self) {
        let expose_decl_svc_allowed = ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "svc_allowed".parse().unwrap(),
            target_name: "svc_allowed".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let expose_decl_svc_not_allowed = ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "svc_not_allowed".parse().unwrap(),
            target_name: "svc_not_allowed".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        };
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("c"))
                    .add_child(ChildDeclBuilder::new_lazy_child("d"))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("d".parse().unwrap()),
                        source_name: "svc_allowed".parse().unwrap(),
                        target_name: "svc_allowed".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("d".parse().unwrap()),
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_name: "svc_not_allowed".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env_c")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_allowed".parse().unwrap(),
                                    target_name: "svc_allowed".parse().unwrap(),
                                    source: RegistrationSource::Parent,
                                },
                            ))
                            .add_debug_registration(cm_rust::DebugRegistration::Protocol(
                                cm_rust::DebugProtocolRegistration {
                                    source_name: "svc_not_allowed".parse().unwrap(),
                                    target_name: "svc_not_allowed".parse().unwrap(),
                                    source: RegistrationSource::Parent,
                                },
                            )),
                    )
                    .add_child(ChildDeclBuilder::new_lazy_child("e").environment("env_c"))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .protocol(ProtocolDeclBuilder::new("svc_allowed").build())
                    .expose(cm_rust::ExposeDecl::Protocol(expose_decl_svc_allowed))
                    .protocol(ProtocolDeclBuilder::new("svc_not_allowed").build())
                    .expose(cm_rust::ExposeDecl::Protocol(expose_decl_svc_not_allowed))
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_not_allowed".parse().unwrap(),
                        target_path: "/svc/hippo_not_allowed".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        dependency_type: DependencyType::Strong,
                        source: UseSource::Debug,
                        source_name: "svc_allowed".parse().unwrap(),
                        target_path: "/svc/hippo".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .build(),
            ),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(DebugCapabilityAllowlistEntry::new(
            AllowlistEntryBuilder::new().exact("b").any_descendant(),
            AllowlistEntryBuilder::new().exact("b").any_descendant(),
        ));

        let mut builder = T::new("a", components);
        builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "svc_allowed".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "env_c".to_string(),
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c", "e"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/svc/hippo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "c", "e"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/svc/hippo_not_allowed".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///  /
    /// b
    ///
    /// a: offer to b from self
    /// b: use from parent
    pub async fn test_route_service_from_parent(&self) {
        let use_decl = UseServiceDecl {
            dependency_type: DependencyType::Strong,
            source: UseSource::Parent,
            source_name: "foo".parse().unwrap(),
            target_path: "/foo".parse().unwrap(),
            availability: Availability::Required,
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        source_instance_filter: None,
                        renamed_instances: None,
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        availability: Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "foo".parse().unwrap(),
                        source_path: Some("/svc/foo".parse().unwrap()),
                    })
                    .add_lazy_child("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("root instance");
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  /
    /// b
    ///
    /// a: use from #b
    /// b: expose to parent from self
    pub async fn test_route_service_from_child(&self) {
        let use_decl = UseServiceDecl {
            dependency_type: DependencyType::Strong,
            source: UseSource::Child("b".to_string()),
            source_name: "foo".parse().unwrap(),
            target_path: "/foo".parse().unwrap(),
            availability: Availability::Required,
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .use_(use_decl.clone().into())
                    .add_lazy_child("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .service(ServiceDecl {
                        name: "foo".parse().unwrap(),
                        source_path: Some("/svc/foo".parse().unwrap()),
                    })
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("root instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &b_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b from child c
    /// b: use from parent
    /// c: expose from self
    pub async fn test_route_service_from_sibling(&self) {
        let use_decl = UseServiceDecl {
            dependency_type: DependencyType::Strong,
            source: UseSource::Parent,
            source_name: "foo".parse().unwrap(),
            target_path: "/foo".parse().unwrap(),
            availability: Availability::Required,
        };
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        source_instance_filter: None,
                        renamed_instances: None,
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "foo".parse().unwrap(),
                        source_path: Some("/svc/foo".parse().unwrap()),
                    })
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let c_component =
            model.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");

        // Verify this source comes from `c`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &c_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b with service instance filter set from child c
    /// b: use from parent
    /// c: expose from self
    pub async fn test_route_filtered_service_from_sibling(&self) {
        let use_decl = UseServiceDecl {
            dependency_type: DependencyType::Strong,
            source: UseSource::Parent,
            source_name: "foo".parse().unwrap(),
            target_path: "/foo".parse().unwrap(),
            availability: Availability::Required,
        };
        let source_instance_filter = Some(vec!["service_instance_0".to_string()]);
        let renamed_instances = Some(vec![]);
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        source_instance_filter: source_instance_filter,
                        renamed_instances: renamed_instances,
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "foo".parse().unwrap(),
                        source_path: Some("/svc/foo".parse().unwrap()),
                    })
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let c_component =
            model.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");

        // Verify this source comes from `c`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::FilteredService {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                        source_instance_filter,
                        instance_name_source_to_target,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert_eq!(source_instance_filter, vec!["service_instance_0".to_string()]);
                assert_eq!(instance_name_source_to_target, HashMap::new());

                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &c_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b with a service instance renamed from child c
    /// b: use from parent
    /// c: expose from self
    pub async fn test_route_renamed_service_instance_from_sibling(&self) {
        let use_decl = UseServiceDecl {
            dependency_type: DependencyType::Strong,
            source: UseSource::Parent,
            source_name: "foo".parse().unwrap(),
            target_path: "/foo".parse().unwrap(),
            availability: Availability::Required,
        };
        let source_instance_filter = Some(vec![]);
        let renamed_instances = Some(vec![NameMapping {
            source_name: "instance_0".to_string(),
            target_name: "renamed_instance_0".to_string(),
        }]);
        let expected_rename_map = {
            let mut m = HashMap::new();
            m.insert("instance_0".to_string(), vec!["renamed_instance_0".to_string()]);
            m
        };

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Service(OfferServiceDecl {
                        source: OfferSource::static_child("c".parse().unwrap()),
                        source_name: "foo".parse().unwrap(),
                        source_instance_filter: source_instance_filter,
                        renamed_instances: renamed_instances,
                        target_name: "foo".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        availability: Availability::Required,
                    }))
                    .add_lazy_child("b")
                    .add_lazy_child("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "foo".parse().unwrap(),
                        target_name: "foo".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "foo".parse().unwrap(),
                        source_path: Some("/svc/foo".parse().unwrap()),
                    })
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let c_component =
            model.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");

        // Verify this source comes from `c`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::FilteredService {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                        source_instance_filter,
                        instance_name_source_to_target,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert_eq!(source_instance_filter, Vec::<String>::new());
                assert_eq!(instance_name_source_to_target, expected_rename_map);

                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &c_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service "/svc/runner" from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: uses runner "hobbit".
    pub async fn test_route_runner_from_parent_environment(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "elf".parse().unwrap(),
                                source: RegistrationSource::Self_,
                                target_name: "hobbit".parse().unwrap(),
                            })
                            .build(),
                    )
                    .runner(RunnerDecl {
                        name: "elf".parse().unwrap(),
                        source_path: Some("/svc/runner".parse().unwrap()),
                    })
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/runner".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: declares runner "elf" as service "/svc/runner" from self.
    /// a: offers runner "elf" from self to "b" as "dwarf".
    /// b: registers runner "dwarf" from realm in environment as "hobbit".
    /// c: uses runner "hobbit".
    pub async fn test_route_runner_from_grandparent_environment(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_lazy_child("b")
                    .offer(OfferDecl::Runner(OfferRunnerDecl {
                        source: OfferSource::Self_,
                        source_name: "elf".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        target_name: "dwarf".parse().unwrap(),
                    }))
                    .runner(RunnerDecl {
                        name: "elf".parse().unwrap(),
                        source_path: Some("/svc/runner".parse().unwrap()),
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "dwarf".parse().unwrap(),
                                source: RegistrationSource::Parent,
                                target_name: "hobbit".parse().unwrap(),
                            })
                            .build(),
                    )
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let c_component =
            model.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &c_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/runner".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: registers runner "dwarf" from "b" in environment as "hobbit".
    /// b: exposes runner "elf" as service "/svc/runner" from self as "dwarf".
    /// c: uses runner "hobbit".
    pub async fn test_route_runner_from_sibling_environment(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_lazy_child("b")
                    .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "dwarf".parse().unwrap(),
                                source: RegistrationSource::Child("b".parse().unwrap()),
                                target_name: "hobbit".parse().unwrap(),
                            })
                            .build(),
                    )
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Runner(ExposeRunnerDecl {
                        source: ExposeSource::Self_,
                        source_name: "elf".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        target_name: "dwarf".parse().unwrap(),
                    }))
                    .runner(RunnerDecl {
                        name: "elf".parse().unwrap(),
                        source_path: Some("/svc/runner".parse().unwrap()),
                    })
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let c_component =
            model.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &c_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `b`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/runner".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &b_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: declares runner "elf" as service "/svc/runner" from self.
    /// a: registers runner "elf" from realm in environment as "hobbit".
    /// b: creates environment extending from realm.
    /// c: uses runner "hobbit".
    pub async fn test_route_runner_from_inherited_environment(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "elf".parse().unwrap(),
                                source: RegistrationSource::Self_,
                                target_name: "hobbit".parse().unwrap(),
                            })
                            .build(),
                    )
                    .runner(RunnerDecl {
                        name: "elf".parse().unwrap(),
                        source_path: Some("/svc/runner".parse().unwrap()),
                    })
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .build(),
                    )
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let c_component =
            model.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &c_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/runner".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service "/svc/runner" from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: uses runner "hobbit". Fails because "hobbit" was not in environment.
    pub async fn test_route_runner_from_environment_not_found(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "elf".parse().unwrap(),
                                source: RegistrationSource::Self_,
                                target_name: "dwarf".parse().unwrap(),
                            })
                            .build(),
                    )
                    .runner(RunnerDecl {
                        name: "elf".parse().unwrap(),
                        source_path: Some("/svc/runner".parse().unwrap()),
                    })
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_result = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await;

        assert_matches!(
            route_result,
            Err(RoutingError::UseFromEnvironmentNotFound {
                    moniker,
                    capability_type,
                    capability_name,
                }
            )
                if moniker == *b_component.moniker() &&
                capability_type == "runner" &&
                capability_name == "hobbit"
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: registers built-in runner "elf" from realm in environment as "hobbit".
    /// b: uses runner "hobbit".
    pub async fn test_route_builtin_runner(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "elf".parse().unwrap(),
                                source: RegistrationSource::Parent,
                                target_name: "hobbit".parse().unwrap(),
                            })
                            .build(),
                    )
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
            name: "elf".parse().unwrap(),
            source_path: None,
        })]);
        builder.register_mock_builtin_runner("elf");
        let model = builder.build().await;

        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this is a built-in source.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Builtin {
                        capability: InternalCapability::Runner(name),
                        ..
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "elf");
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///
    /// a: uses built-in runner "elf" from the root environment.
    pub async fn test_route_builtin_runner_from_root_env(&self) {
        let components = vec![("a", ComponentDeclBuilder::new().build())];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
            name: "elf".parse().unwrap(),
            source_path: None,
        })]);
        builder.register_mock_builtin_runner("elf");
        let model = builder.build().await;

        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let source = route_capability(
            RouteRequest::Runner("elf".parse().unwrap()),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this is a built-in source.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Builtin {
                        capability: InternalCapability::Runner(name),
                        ..
                    },
                relative_path,
            } if relative_path == PathBuf::new() => {
                assert_eq!(name, "elf");
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: registers built-in runner "elf" from realm in environment as "hobbit". The ELF runner is
    ///    registered in the root environment, but not declared as a built-in capability.
    /// b: uses runner "hobbit"; should fail.
    pub async fn test_route_builtin_runner_not_found(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .name("env")
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .add_runner(RunnerRegistration {
                                source_name: "elf".parse().unwrap(),
                                source: RegistrationSource::Parent,
                                target_name: "hobbit".parse().unwrap(),
                            })
                            .build(),
                    )
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
        ];

        let mut builder = T::new("a", components);
        builder.register_mock_builtin_runner("elf");

        let model = builder.build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_result = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await;

        assert_matches!(
            route_result,
            Err(RoutingError::RegisterFromComponentManagerNotFound {
                    capability_id,
                }
            )
                if capability_id == "elf".to_string()
        );
    }

    ///   a
    ///
    /// a: Attempts to use unregistered runner "hobbit" from the root environment.
    ///    The runner is provided as a built-in capability, but not registered in
    ///    the root environment.
    pub async fn test_route_builtin_runner_from_root_env_registration_not_found(&self) {
        let components = vec![("a", ComponentDeclBuilder::new().build())];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
            name: "hobbit".parse().unwrap(),
            source_path: None,
        })]);
        let model = builder.build().await;

        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let route_result = route_capability(
            RouteRequest::Runner("hobbit".parse().unwrap()),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await;

        assert_matches!(
            route_result,
            Err(RoutingError::UseFromEnvironmentNotFound {
                    moniker,
                    capability_type,
                    capability_name,
                }
            )
                if moniker == *a_component.moniker()
                && capability_type == "runner".to_string()
                && capability_name == "hobbit"
        );
    }
}
