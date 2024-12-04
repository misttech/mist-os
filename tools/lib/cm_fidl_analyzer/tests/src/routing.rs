// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use assert_matches::assert_matches;
use async_trait::async_trait;
use camino::Utf8PathBuf;
use cm_config::{
    AllowlistEntry, CapabilityAllowlistKey, DebugCapabilityAllowlistEntry, DebugCapabilityKey,
    RuntimeConfig, SecurityPolicy,
};
use cm_fidl_analyzer::component_instance::ComponentInstanceForAnalyzer;
use cm_fidl_analyzer::component_model::{
    AnalyzerModelError, ComponentModelForAnalyzer, DynamicConfig, ModelBuilderForAnalyzer,
};
use cm_fidl_analyzer::environment::{BOOT_RESOLVER_NAME, BOOT_SCHEME};
use cm_fidl_analyzer::route::{TargetDecl, VerifyRouteResult};
use cm_rust::*;
use cm_rust_testing::*;
use cm_types::Url;
use fidl::prelude::*;
use moniker::Moniker;
use routing::capability_source::{
    BuiltinSource, CapabilitySource, ComponentCapability, ComponentSource, FrameworkSource,
    InternalCapability, NamespaceSource,
};
use routing::component_instance::ComponentInstanceInterface;
use routing::environment::RunnerRegistry;
use routing::error::RoutingError;
use routing::mapper::RouteSegment;
use routing::RegistrationDecl;
use routing_test_helpers::{
    CheckUse, ComponentEventRoute, ExpectedResult, RoutingTestModel, RoutingTestModelBuilder,
    ServiceInstance,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use {
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_internal as component_internal,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys, zx_status,
};

const TEST_URL_PREFIX: &str = "test:///";
// Placeholder for when a component resolves to itself, and its name is unknown as a result.
const USE_TARGET_PLACEHOLDER_NAME: &str = "target";

fn make_test_url(component_name: &str) -> String {
    format!("{}{}", TEST_URL_PREFIX, component_name)
}

pub struct RoutingTestForAnalyzer {
    pub model: Arc<ComponentModelForAnalyzer>,
}

pub struct RoutingTestBuilderForAnalyzer {
    root_url: Url,
    decls_by_url: HashMap<Url, (ComponentDecl, Option<config_encoder::ConfigFields>)>,
    namespace_capabilities: Vec<CapabilityDecl>,
    builtin_capabilities: Vec<CapabilityDecl>,
    builtin_runner_registrations: Vec<RunnerRegistration>,
    capability_policy: HashMap<CapabilityAllowlistKey, HashSet<AllowlistEntry>>,
    debug_capability_policy: HashMap<DebugCapabilityKey, HashSet<DebugCapabilityAllowlistEntry>>,
    component_id_index_path: Option<Utf8PathBuf>,
    builtin_boot_resolver: component_internal::BuiltinBootResolver,
    dynamic_config: DynamicConfig,
}

impl RoutingTestBuilderForAnalyzer {
    fn set_builtin_boot_resolver(&mut self, resolver: component_internal::BuiltinBootResolver) {
        self.builtin_boot_resolver = resolver;
    }

    // Creates a new builder with the specified map of component URLs to `ComponentDecl`s, rather
    // than using the default test URL scheme.
    fn new_with_custom_urls(root_url: String, components: Vec<(String, ComponentDecl)>) -> Self {
        let root_url = Url::new(root_url).expect("failed to parse root component url");
        let decls_by_url =
            HashMap::from_iter(components.into_iter().map(|(url_string, component_decl)| {
                (Url::new(&url_string).unwrap(), (component_decl, None))
            }));
        Self {
            root_url,
            decls_by_url,
            namespace_capabilities: Vec::new(),
            builtin_capabilities: Vec::new(),
            builtin_runner_registrations: Vec::new(),
            capability_policy: HashMap::new(),
            debug_capability_policy: HashMap::new(),
            component_id_index_path: None,
            builtin_boot_resolver: component_internal::BuiltinBootResolver::None,
            dynamic_config: DynamicConfig::default(),
        }
    }

    fn set_dynamic_config(&mut self, config: DynamicConfig) {
        self.dynamic_config = config;
    }
}

#[async_trait]
impl RoutingTestModelBuilder for RoutingTestBuilderForAnalyzer {
    type Model = RoutingTestForAnalyzer;

    // Creates a new builder with the specified components. Components are specified by name;
    // the method assigns each component a test URL.
    fn new(root_component: &str, components: Vec<(&'static str, ComponentDecl)>) -> Self {
        let root_url =
            Url::new(make_test_url(root_component)).expect("failed to parse root component url");
        let decls_by_url = HashMap::from_iter(components.into_iter().map(|(name, decl)| {
            (Url::new(&format!("{}{}", TEST_URL_PREFIX, name)).unwrap(), (decl, None))
        }));
        Self {
            root_url,
            decls_by_url,
            namespace_capabilities: Vec::new(),
            builtin_capabilities: Vec::new(),
            builtin_runner_registrations: Vec::new(),
            capability_policy: HashMap::new(),
            debug_capability_policy: HashMap::new(),
            component_id_index_path: None,
            builtin_boot_resolver: component_internal::BuiltinBootResolver::None,
            dynamic_config: DynamicConfig::default(),
        }
    }

    fn set_namespace_capabilities(&mut self, caps: Vec<CapabilityDecl>) {
        self.namespace_capabilities = caps;
    }

    fn set_builtin_capabilities(&mut self, caps: Vec<CapabilityDecl>) {
        self.builtin_capabilities = caps;
    }

    fn register_mock_builtin_runner(&mut self, runner: &str) {
        let runner_name: cm_types::Name = runner.parse().unwrap();
        self.builtin_runner_registrations.push(RunnerRegistration {
            source_name: runner_name.clone(),
            target_name: runner_name,
            source: RegistrationSource::Self_,
        });
    }

    /// Add a custom capability security policy to restrict routing of certain caps.
    fn add_capability_policy(
        &mut self,
        key: CapabilityAllowlistKey,
        allowlist: HashSet<AllowlistEntry>,
    ) {
        self.capability_policy.insert(key, allowlist);
    }

    /// Add a custom debug capability security policy to restrict routing of certain caps.
    fn add_debug_capability_policy(
        &mut self,
        key: DebugCapabilityKey,
        allowlist: HashSet<DebugCapabilityAllowlistEntry>,
    ) {
        self.debug_capability_policy.insert(key, allowlist);
    }

    fn set_component_id_index_path(&mut self, index_path: Utf8PathBuf) {
        self.component_id_index_path = Some(index_path);
    }

    async fn build(self) -> RoutingTestForAnalyzer {
        let mut config = RuntimeConfig::default();
        config.root_component_url = Some(self.root_url.clone());
        config.namespace_capabilities = self.namespace_capabilities;
        config.builtin_capabilities = self.builtin_capabilities;

        config.security_policy = Arc::new(SecurityPolicy {
            capability_policy: self.capability_policy,
            debug_capability_policy: self.debug_capability_policy,
            ..Default::default()
        });

        config.component_id_index_path = self.component_id_index_path;
        let component_id_index = match config.component_id_index_path {
            Some(ref index_path) => component_id_index::Index::from_fidl_file(index_path)
                .unwrap_or_else(|e| {
                    panic!("failed to create component ID index with path {}: {:?}", index_path, e)
                }),
            None => Default::default(),
        };
        config.builtin_boot_resolver = self.builtin_boot_resolver;

        let build_model_result = ModelBuilderForAnalyzer::new(self.root_url)
            .build_with_dynamic_config(
                self.dynamic_config,
                self.decls_by_url,
                Arc::new(config),
                Arc::new(component_id_index),
                RunnerRegistry::from_decl(&self.builtin_runner_registrations),
            );
        let model = build_model_result.model.expect("failed to build ComponentModelForAnalyzer");
        RoutingTestForAnalyzer { model }
    }
}

#[derive(Debug, Error)]
pub enum TestModelError {
    #[error("matching use decl not found")]
    UseDeclNotFound,
    #[error("matching expose decl not found")]
    ExposeDeclNotFound,
}

impl TestModelError {
    pub fn as_zx_status(&self) -> zx_status::Status {
        match self {
            Self::UseDeclNotFound | Self::ExposeDeclNotFound => zx_status::Status::NOT_FOUND,
        }
    }
}

impl RoutingTestForAnalyzer {
    fn assert_event_stream_scope(
        &self,
        use_decl: &UseEventStreamDecl,
        scope: &Vec<ComponentEventRoute>,
        target: &Arc<ComponentInstanceForAnalyzer>,
    ) {
        // Perform secondary routing to find scope
        let (result, mut segments) =
            ComponentModelForAnalyzer::route_event_stream_sync(use_decl.clone(), &target);
        result.expect("Expected event_stream routing to succeed.");

        let mut route = use_decl
            .scope
            .as_ref()
            .map(|scope| {
                let route = ComponentEventRoute {
                    component: USE_TARGET_PLACEHOLDER_NAME.to_string(),
                    scope: Some(
                        scope
                            .iter()
                            .map(|s| match s {
                                cm_rust::EventScope::Child(child) => child.name.to_string(),
                                cm_rust::EventScope::Collection(collection) => {
                                    collection.to_string()
                                }
                            })
                            .collect(),
                    ),
                };
                vec![route]
            })
            .unwrap_or_default();
        segments.reverse();
        // Generate a unified route from the component topology
        generate_unified_route(&segments, &mut route);
        assert_eq!(scope, &route);
    }

    fn find_matching_use(
        &self,
        check: CheckUse,
        decl: &ComponentDecl,
    ) -> (Result<UseDecl, TestModelError>, ExpectedResult) {
        match check {
            CheckUse::Directory { path, expected_res, .. } => (
                decl.uses
                    .iter()
                    .find_map(|u| match u {
                        UseDecl::Directory(d) if d.target_path == path => Some(u.clone()),
                        _ => None,
                    })
                    .ok_or(TestModelError::UseDeclNotFound),
                expected_res,
            ),
            CheckUse::Protocol { path, expected_res, .. } => (
                decl.uses
                    .iter()
                    .find_map(|u| match u {
                        UseDecl::Protocol(d) if d.target_path == path => Some(u.clone()),
                        _ => None,
                    })
                    .ok_or(TestModelError::UseDeclNotFound),
                expected_res,
            ),
            CheckUse::Service { path, expected_res, .. } => (
                decl.uses
                    .iter()
                    .find_map(|u| match u {
                        UseDecl::Service(d) if d.target_path == path => Some(u.clone()),
                        _ => None,
                    })
                    .ok_or(TestModelError::UseDeclNotFound),
                expected_res,
            ),
            CheckUse::Storage { path, expected_res, .. } => (
                decl.uses
                    .iter()
                    .find_map(|u| match u {
                        UseDecl::Storage(d) if d.target_path == path => Some(u.clone()),
                        _ => None,
                    })
                    .ok_or(TestModelError::UseDeclNotFound),
                expected_res,
            ),
            CheckUse::StorageAdmin { expected_res, .. } => (
                decl.uses
                    .iter()
                    .find_map(|u| match u {
                        UseDecl::Protocol(d)
                            if d.source_name.to_string()
                                == fsys::StorageAdminMarker::PROTOCOL_NAME =>
                        {
                            Some(u.clone())
                        }
                        _ => None,
                    })
                    .ok_or(TestModelError::UseDeclNotFound),
                expected_res,
            ),
            CheckUse::EventStream { path, scope: _, name, expected_res } => (
                decl.uses
                    .iter()
                    .find_map(|u| match u {
                        UseDecl::EventStream(d)
                            if d.source_name.to_string() == name.to_string()
                                && path == d.target_path =>
                        {
                            Some(u.clone())
                        }
                        _ => None,
                    })
                    .ok_or(TestModelError::UseDeclNotFound),
                expected_res,
            ),
        }
    }

    fn find_matching_expose(
        &self,
        check: CheckUse,
        decl: &ComponentDecl,
    ) -> (Result<ExposeDecl, TestModelError>, ExpectedResult) {
        match check {
            CheckUse::Directory { path, expected_res, .. }
            | CheckUse::Protocol { path, expected_res, .. }
            | CheckUse::EventStream { path, expected_res, .. }
            | CheckUse::Service { path, expected_res, .. } => (
                decl.exposes
                    .iter()
                    .find(|&e| e.target_name() == path.basename())
                    .cloned()
                    .ok_or(TestModelError::ExposeDeclNotFound),
                expected_res,
            ),
            CheckUse::Storage { .. } | CheckUse::StorageAdmin { .. } => {
                panic!("attempted to use from expose for unsupported capability type")
            }
        }
    }
}

fn segment_to_component_event_route(segment: &RouteSegment) -> Option<ComponentEventRoute> {
    let (moniker, offer) = match segment {
        RouteSegment::OfferBy { moniker, capability } => (moniker, capability),
        _ => return None,
    };
    let event_stream_offer = match offer {
        OfferDecl::EventStream(o) => o,
        _ => return None,
    };
    let scopes = match &event_stream_offer.scope {
        Some(scope) => Some(
            scope
                .iter()
                .map(|s| match s {
                    cm_rust::EventScope::Child(child) => child.name.to_string(),
                    cm_rust::EventScope::Collection(collection) => collection.to_string(),
                })
                .collect(),
        ),
        None => None,
    };
    let moniker_name = match moniker.leaf() {
        Some(l) => l.name().to_string(),
        None => "/".to_string(),
    };
    Some(ComponentEventRoute { component: moniker_name, scope: scopes })
}

fn generate_unified_route(segments: &Vec<RouteSegment>, routes: &mut Vec<ComponentEventRoute>) {
    for segment in segments {
        if let Some(route) = segment_to_component_event_route(segment) {
            routes.push(route)
        }
    }
}

#[async_trait]
impl RoutingTestModel for RoutingTestForAnalyzer {
    type C = ComponentInstanceForAnalyzer;

    async fn check_use(&self, moniker: Moniker, check: CheckUse) {
        let target = self.model.get_instance(&moniker).expect("target instance not found");
        let scope =
            if let CheckUse::EventStream { path: _, ref scope, name: _, expected_res: _ } = check {
                Some(scope.clone())
            } else {
                None
            };

        let (find_decl, expected) = self.find_matching_use(check, target.decl_for_testing());

        // If `find_decl` is not OK, check that `expected` has a matching error.
        // Otherwise, route the capability and compare the result to `expected`.
        match &find_decl {
            Err(err) => {
                match expected {
                    ExpectedResult::Ok => panic!("expected UseDecl was not found: {}", err),
                    ExpectedResult::Err(status) => {
                        assert_eq!(err.as_zx_status(), status);
                    }
                    ExpectedResult::ErrWithNoEpitaph => {}
                };
                return;
            }
            Ok(use_decl) => {
                for result in self.model.check_use_capability(use_decl, &target).await.iter() {
                    match result.error {
                        Some(ref err) => match expected {
                            ExpectedResult::Ok => {
                                panic!("routing failed, expected success: {:?}", err)
                            }
                            ExpectedResult::Err(status) => {
                                assert_eq!(err.as_zx_status(), status);
                            }
                            ExpectedResult::ErrWithNoEpitaph => {}
                        },
                        None => match expected {
                            ExpectedResult::Ok => {
                                if let UseDecl::EventStream(use_decl) = use_decl {
                                    self.assert_event_stream_scope(
                                        use_decl,
                                        scope
                                            .as_ref()
                                            .expect("scope should be non-null for event streams"),
                                        &target,
                                    );
                                }
                            }
                            _ => panic!("capability use succeeded, expected failure"),
                        },
                    }
                }
            }
        }
    }

    async fn check_use_exposed_dir(&self, moniker: Moniker, check: CheckUse) {
        let target = self.model.get_instance(&moniker).expect("target instance not found");

        let (find_decl, expected) = self.find_matching_expose(check, target.decl_for_testing());

        // If `find_decl` is not OK, check that `expected` has a matching error.
        // Otherwise, route the capability and compare the result to `expected`.
        match &find_decl {
            Err(err) => {
                match expected {
                    ExpectedResult::Ok => panic!("expected ExposeDecl was not found: {}", err),
                    ExpectedResult::Err(status) => {
                        assert_eq!(err.as_zx_status(), status);
                    }
                    _ => unimplemented![],
                };
                return;
            }
            Ok(expose_decl) => {
                match self
                    .model
                    .check_use_exposed_capability(expose_decl, &target)
                    .await
                    .expect("expected result for exposed directory")
                    .error
                {
                    Some(err) => match expected {
                        ExpectedResult::Ok => {
                            panic!("expected success, but routing failed with error {:?}", err)
                        }
                        ExpectedResult::Err(status) => {
                            assert_eq!(err.as_zx_status(), status);
                        }
                        _ => unimplemented![],
                    },
                    None => match expected {
                        ExpectedResult::Ok => {}
                        _ => panic!("capability use succeeded, expected failure"),
                    },
                }
            }
        }
    }

    async fn look_up_instance(
        &self,
        moniker: &Moniker,
    ) -> Result<Arc<ComponentInstanceForAnalyzer>, anyhow::Error> {
        self.model.get_instance(&moniker).map_err(|err| anyhow!(err))
    }

    // File and directory operations
    //
    // All file and directory operations are no-ops for the static model.
    #[allow(unused_variables)]
    async fn check_open_node(&self, moniker: Moniker, path: cm_types::Path) {}

    #[allow(unused_variables)]
    async fn create_static_file(&self, path: &Path, contents: &str) -> Result<(), anyhow::Error> {
        Ok(())
    }

    #[allow(unused_variables)]
    fn install_namespace_directory(&self, path: &str) {}

    #[allow(unused_variables)]
    fn add_subdir_to_data_directory(&self, subdir: &str) {}

    #[allow(unused_variables)]
    async fn check_test_subdir_contents(&self, path: &str, expected: Vec<String>) {}

    #[allow(unused_variables)]
    async fn check_namespace_subdir_contents(&self, path: &str, expected: Vec<String>) {}

    #[allow(unused_variables)]
    async fn check_test_subdir_contains(&self, path: &str, expected: String) {}

    #[allow(unused_variables)]
    async fn check_test_dir_tree_contains(&self, expected: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::hashmap;
    use routing_test_helpers::instantiate_common_routing_tests;

    instantiate_common_routing_tests! { RoutingTestBuilderForAnalyzer }

    // Routing of service capabilities is tested as part of the common routing tests
    // generated by `instantiate_common_routing_tests`.
    //
    // In order to test additional validation beyond routing, we need much more setup for component
    // manager's routing model than we do for the static analyzer's model. The following tests
    // suffice to test the static analyzer's `check_use_capability()` method for service capabilities.

    ///   a
    ///  /
    /// b
    ///
    /// a: offer to b from self
    /// b: use from parent
    #[fuchsia::test]
    async fn check_use_service_from_parent() {
        let use_decl = UseBuilder::service().name("foo").path("/foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .target_name("foo")
                            .source(OfferSource::Self_)
                            .target(offer_target_static_child("b")),
                    )
                    .service_default("foo")
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        ];
        let model = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Service {
                    path: "/foo".parse().unwrap(),
                    instance: ServiceInstance::Named("".into()),
                    member: "".into(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await
    }

    ///   a
    ///  /
    /// b
    ///
    /// a: offer to b from self
    /// b: use from parent, but parent component is not executable
    #[fuchsia::test]
    async fn check_service_source_is_executable() {
        let use_decl = UseBuilder::service().name("foo").path("/foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .target_name("foo")
                            .source(OfferSource::Self_)
                            .target(offer_target_static_child("b")),
                    )
                    .service_default("foo")
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        ];
        let model = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Service {
                    path: "/foo".parse().unwrap(),
                    instance: ServiceInstance::Named("".into()),
                    member: "".into(),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
            )
            .await
    }

    ///   a
    ///  /
    /// b
    ///
    /// a: use from b
    /// b: expose to parent from self
    #[fuchsia::test]
    async fn check_use_service_from_child() {
        let use_decl =
            UseBuilder::service().name("foo").path("/foo").source_static_child("b").build();
        let components = vec![
            ("a", ComponentDeclBuilder::new().use_(use_decl.clone()).child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .service_default("foo")
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .build(),
            ),
        ];
        let model = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Service {
                    path: "/foo".parse().unwrap(),
                    instance: ServiceInstance::Named("".into()),
                    member: "".into(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b from child c
    /// b: use from parent
    /// c: expose from self
    #[fuchsia::test]
    async fn check_use_service_from_sibling() {
        let use_decl = UseBuilder::service().name("foo").path("/foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .target_name("foo")
                            .source(offer_source_static_child("c"))
                            .target(offer_target_static_child("b")),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .service_default("foo")
                    .build(),
            ),
        ];
        let model = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Service {
                    path: "/foo".parse().unwrap(),
                    instance: ServiceInstance::Named("".into()),
                    member: "".into(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await
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
    /// c: uses runner "hobbit" in its ProgramDecl.
    #[fuchsia::test]
    async fn check_program_runner_from_inherited_environment() {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                        source_name: "elf".parse().unwrap(),
                        source: RegistrationSource::Self_,
                        target_name: "hobbit".parse().unwrap(),
                    }))
                    .runner_default("elf")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env"))
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let c_component =
            test.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");

        assert!(test
            .model
            .check_program_runner(
                c_component.decl_for_testing().program.as_ref().expect("missing program decl"),
                &c_component
            )
            .expect("expected results of program runner check")
            .error
            .is_none());
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses framework events "started", and "capability_requested"
    #[fuchsia::test]
    pub async fn test_use_event_stream_from_above_root_2() {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                .build(),
        )];

        let mut builder = RoutingTestBuilderForAnalyzer::new("a", components);
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
    #[fuchsia::test]
    pub async fn test_use_event_stream_from_above_root_and_downscoped_2() {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target_static_child("b")
                            .scope(vec![
                                EventScope::Child(ChildRef {
                                    name: "b".parse().unwrap(),
                                    collection: None,
                                }),
                                EventScope::Child(ChildRef {
                                    name: "c".parse().unwrap(),
                                    collection: None,
                                }),
                            ]),
                    )
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target_static_child("c")
                            .scope(vec![
                                EventScope::Child(ChildRef {
                                    name: "b".parse().unwrap(),
                                    collection: None,
                                }),
                                EventScope::Child(ChildRef {
                                    name: "c".parse().unwrap(),
                                    collection: None,
                                }),
                            ]),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target_static_child("d")
                            .scope(vec![EventScope::Child(ChildRef {
                                name: "e".parse().unwrap(),
                                collection: None,
                            })]),
                    )
                    .child_default("d")
                    .child_default("e")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
            ("e", ComponentDeclBuilder::new().build()),
        ];

        let mut builder = RoutingTestBuilderForAnalyzer::new("a", components);
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

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service "/svc/runner" from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: uses runner "hobbit" in its ProgramDecl. Fails because "hobbit" was not in environment.
    #[fuchsia::test]
    async fn check_program_runner_from_environment_not_found() {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                        source_name: "elf".parse().unwrap(),
                        source: RegistrationSource::Self_,
                        target_name: "dwarf".parse().unwrap(),
                    }))
                    .runner_default("elf")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let check_result = test
            .model
            .check_program_runner(
                &b_component.decl_for_testing().program.as_ref().expect("missing program decl"),
                &b_component,
            )
            .expect("expected result of program runner check");

        assert_matches!(
            check_result.error,
            Some(AnalyzerModelError::RoutingError(
                    RoutingError::UseFromEnvironmentNotFound {
                    moniker,
                    capability_type,
                    capability_name,
            }))
                if moniker == *b_component.moniker() &&
                capability_type == "runner" &&
                capability_name == "hobbit"
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: creates environment "env" and registers resolver "base" in "env" from self.
    /// b: has environment "env" and is resolved by the "base" resolver.
    #[fuchsia::test]
    async fn check_resolver_from_extended_environment() {
        let a_url = make_test_url("a");
        let b_url = "base://b/".to_string();

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").url(&b_url).environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").resolver(
                        ResolverRegistration {
                            resolver: "base".parse().unwrap(),
                            source: RegistrationSource::Self_,
                            scheme: "base".parse().unwrap(),
                        },
                    ))
                    .resolver_default("base")
                    .build(),
            ),
            (b_url, ComponentDeclBuilder::new_empty_component().build()),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");

        let result = test.model.check_resolver(&b_component);
        assert!(result.error.is_none());
        assert_eq!(result.using_node, Moniker::parse_str("b").unwrap());
        assert_eq!(result.capability, Some("base".parse().unwrap()));
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: creates environment "b_env" and registers resolver "base" in "b_env" from self.
    /// b: inherits environment "b_env" but creates a new empty environment "c_env" for c.
    /// c: doesn't inherit the "base" resolver.
    #[fuchsia::test]
    async fn check_resolver_from_grandparent_environment_not_found() {
        let a_url = make_test_url("a");
        let b_url = make_test_url("b");
        let c_url = "base://c/".to_string();

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("b_env"))
                    .environment(EnvironmentBuilder::new().name("b_env").resolver(
                        ResolverRegistration {
                            resolver: "base".parse().unwrap(),
                            source: RegistrationSource::Self_,
                            scheme: "base".parse().unwrap(),
                        },
                    ))
                    .resolver_default("base")
                    .build(),
            ),
            (
                b_url,
                ComponentDeclBuilder::new_empty_component()
                    .child(ChildBuilder::new().name("c").url(&c_url).environment("c_env"))
                    .environment(
                        EnvironmentBuilder::new()
                            .name("c_env")
                            .extends(fdecl::EnvironmentExtends::None),
                    )
                    .build(),
            ),
            (c_url, ComponentDeclBuilder::new_empty_component().build()),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let c_component =
            test.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");

        let result = test.model.check_resolver(&c_component);

        assert_matches!(
            &result.error,
            Some(AnalyzerModelError::MissingResolverForScheme(
                    moniker,
                    resolver,
                ))
                if resolver == "base" && moniker == c_component.moniker()
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has the standard boot resolver registered in its environment, but
    ///    the resolver is not provided as a built-in capability.
    /// b: is resolved by the standard boot resolver.
    #[fuchsia::test]
    async fn check_resolver_from_builtin_environment_not_found() {
        let a_url = make_test_url("a");
        let b_url = format!("{}:///b", BOOT_SCHEME);

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").url(&format!("{}:///b", BOOT_SCHEME)))
                    .build(),
            ),
            (b_url, ComponentDeclBuilder::new().build()),
        ];

        let mut builder = RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components);
        builder.set_builtin_boot_resolver(component_internal::BuiltinBootResolver::Boot);
        let test = builder.build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");

        let result = test.model.check_resolver(&b_component);

        assert_matches!(
        &result.error,
            Some(AnalyzerModelError::RoutingError(
                    RoutingError::UseFromComponentManagerNotFound{
                        capability_id: resolver
            }))
                if resolver == BOOT_RESOLVER_NAME
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: offers protocol /svc/foo from self as /svc/bar
    /// b: uses protocol /svc/bar as /svc/hippo
    #[fuchsia::test]
    async fn route_use_from_parent() {
        let use_decl = UseBuilder::protocol().name("bar").path("/svc/hippo").build();
        let offer_decl = OfferBuilder::protocol()
            .name("foo")
            .target_name("bar")
            .source(OfferSource::Self_)
            .target_static_child("b")
            .build();
        let protocol_decl = CapabilityBuilder::protocol().name("foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(protocol_decl.clone())
                    .offer(offer_decl.clone())
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        ];
        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let root_component = test.look_up_instance(&Moniker::root()).await.expect("root instance");
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_result = test.model.check_use_capability(&use_decl, &b_component).await;
        assert_eq!(route_result.len(), 1);
        let route_result = &route_result[0];
        assert!(route_result.error.is_none());
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                capability: protocol_decl.into(),
                moniker: root_component.moniker().clone(),
            })),
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: uses protocol /svc/bar from b as /svc/hippo
    /// b: exposes protocol /svc/foo from self as /svc/bar
    #[fuchsia::test]
    async fn route_use_from_child() {
        let use_decl = UseBuilder::protocol()
            .name("bar")
            .source(UseSource::Child("b".parse().unwrap()))
            .path("/svc/hippo")
            .build();
        let expose_decl = ExposeBuilder::protocol()
            .name("foo")
            .target_name("bar")
            .source(ExposeSource::Self_)
            .build();
        let protocol_decl = CapabilityBuilder::protocol().name("foo").build();
        let components = vec![
            ("a", ComponentDeclBuilder::new().use_(use_decl.clone()).child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(protocol_decl.clone())
                    .expose(expose_decl.clone())
                    .build(),
            ),
        ];
        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let a_component = test.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_results = test.model.check_use_capability(&use_decl, &a_component).await;
        assert_eq!(route_results.len(), 1);
        let route_result = &route_results[0];
        assert_matches!(route_result.error, None);
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                capability: protocol_decl.into(),
                moniker: b_component.moniker().clone(),
            })),
        );
    }

    /// a: uses protocol /svc/foo from self under the path /svc/hippo
    #[fuchsia::test]
    async fn route_use_from_self() {
        let use_decl =
            UseBuilder::protocol().name("foo").source(UseSource::Self_).path("/svc/hippo").build();
        let protocol_decl = CapabilityBuilder::protocol().name("foo").build();
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .capability(protocol_decl.clone())
                .use_(use_decl.clone())
                .build(),
        )];

        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let a_component = test.look_up_instance(&Moniker::root()).await.expect("a instance");
        let route_results = test.model.check_use_capability(&use_decl, &a_component).await;
        assert_eq!(route_results.len(), 1);
        let route_result = &route_results[0];
        assert_matches!(route_result.error, None);
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                capability: protocol_decl.into(),
                moniker: a_component.moniker().clone(),
            })),
        );
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
    #[fuchsia::test]
    async fn map_route_use_from_niece() {
        let use_decl = UseBuilder::directory().name("foobar_data").path("/data/hippo").build();
        let a_offer_decl = OfferBuilder::directory()
            .name("baz_data")
            .target_name("foobar_data")
            .source(offer_source_static_child("b"))
            .target(offer_target_static_child("c"))
            .rights(fio::R_STAR_DIR)
            .build();
        let b_expose_decl = ExposeBuilder::directory()
            .name("bar_data")
            .target_name("baz_data")
            .source_static_child("d")
            .rights(fio::R_STAR_DIR)
            .build();
        let d_expose_decl = ExposeBuilder::directory()
            .name("foo_data")
            .target_name("bar_data")
            .source(ExposeSource::Self_)
            .rights(fio::R_STAR_DIR)
            .build();
        let directory_decl =
            CapabilityBuilder::directory().name("foo_data").path("/foo/data").build();

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(a_offer_decl.clone())
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(b_expose_decl.clone())
                    .child_default("d")
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
            (
                "d",
                ComponentDeclBuilder::new()
                    .capability(directory_decl.clone())
                    .expose(d_expose_decl.clone())
                    .build(),
            ),
        ];

        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let c_component =
            test.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let route_results = test.model.check_use_capability(&use_decl, &c_component).await;
        assert_eq!(route_results.len(), 1);
        let route_result = &route_results[0];
        assert!(route_result.error.is_none());

        assert_eq!(
            route_result.route,
            vec![
                RouteSegment::UseBy {
                    moniker: vec!["c"].try_into().unwrap(),
                    capability: use_decl
                },
                RouteSegment::OfferBy { moniker: Moniker::root(), capability: a_offer_decl },
                RouteSegment::ExposeBy {
                    moniker: vec!["b"].try_into().unwrap(),
                    capability: b_expose_decl
                },
                RouteSegment::ExposeBy {
                    moniker: vec!["b", "d"].try_into().unwrap(),
                    capability: d_expose_decl
                },
                RouteSegment::DeclareBy {
                    moniker: vec!["b", "d"].try_into().unwrap(),
                    capability: directory_decl,
                }
            ]
        )
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service "/svc/runner" from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: refers to runner "hobbit" in its `ProgramDecl`.
    #[fuchsia::test]
    async fn map_route_for_program_runner() {
        let runner_reg = RunnerRegistration {
            source_name: "elf".parse().unwrap(),
            source: RegistrationSource::Self_,
            target_name: "hobbit".parse().unwrap(),
        };
        let runner_decl = CapabilityBuilder::runner().name("elf").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").runner(runner_reg.clone()))
                    .capability(runner_decl.clone())
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_result = test
            .model
            .check_program_runner(
                &b_component
                    .decl_for_testing()
                    .program
                    .as_ref()
                    .expect("expected ProgramDecl for b"),
                &b_component,
            )
            .expect("expected result of program runner route");

        assert!(route_result.error.is_none());

        let CapabilityDecl::Runner(runner_decl) = runner_decl else {
            unreachable!();
        };
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                moniker: Moniker::root(),
                capability: runner_decl.into(),
            })),
        )
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: has storage decl with name "cache" with a source of self at path /data
    /// a: offers cache storage to b from "mystorage"
    /// b: uses cache storage as /storage
    ///
    /// We expect 2 route maps: one for the storage capability and one for the backing
    /// directory.
    #[fuchsia::test]
    async fn map_route_storage_and_dir_from_parent() {
        let directory_decl = CapabilityBuilder::directory()
            .name("data")
            .path("/data")
            .rights(fio::RW_STAR_DIR)
            .build();
        let storage_decl = CapabilityBuilder::storage()
            .name("cache")
            .backing_dir("data")
            .source(StorageDirectorySource::Self_)
            .build();
        let offer_storage_decl = OfferBuilder::storage()
            .name("cache")
            .source(OfferSource::Self_)
            .target(offer_target_static_child("b"))
            .build();
        let use_storage_decl = UseBuilder::storage().name("cache").path("/storage").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(storage_decl.clone())
                    .capability(directory_decl.clone())
                    .offer(offer_storage_decl.clone())
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_storage_decl.clone()).build()),
        ];

        let test = RoutingTestBuilderForAnalyzer::new("a", components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_results = test.model.check_use_capability(&use_storage_decl, &b_component).await;
        assert_eq!(route_results.len(), 2);

        let storage_route_result = &route_results[0];
        assert!(storage_route_result.error.is_none());

        let backing_dir_route_result = &route_results[1];
        assert!(backing_dir_route_result.error.is_none());

        assert_eq!(
            storage_route_result.route,
            vec![
                RouteSegment::UseBy {
                    moniker: vec!["b"].try_into().unwrap(),
                    capability: use_storage_decl
                },
                RouteSegment::OfferBy { moniker: Moniker::root(), capability: offer_storage_decl },
                RouteSegment::DeclareBy {
                    moniker: Moniker::root(),
                    capability: storage_decl.clone(),
                }
            ]
        );
        let CapabilityDecl::Storage(storage_decl) = storage_decl else {
            unreachable!();
        };
        assert_eq!(
            backing_dir_route_result.route,
            vec![
                RouteSegment::RegisterBy {
                    moniker: Moniker::root(),
                    capability: RegistrationDecl::Directory(storage_decl.into())
                },
                RouteSegment::DeclareBy { moniker: Moniker::root(), capability: directory_decl }
            ]
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: offers framework protocol "fuchsia.component.Realm" to b
    /// b: uses protocol "fuchsia.component.Realm"
    #[fuchsia::test]
    async fn route_use_from_framework() {
        let offer_realm_decl = OfferBuilder::protocol()
            .name("fuchsia.component.Realm")
            .source(OfferSource::Framework)
            .target(offer_target_static_child("b"))
            .build();
        let use_realm_decl = UseBuilder::protocol().name("fuchsia.component.Realm").build();

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(offer_realm_decl.clone())
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_realm_decl.clone()).build()),
        ];

        let builder = RoutingTestBuilderForAnalyzer::new("a", components);
        let test = builder.build().await;

        let a_component = test.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let realm_route_results =
            test.model.check_use_capability(&use_realm_decl, &b_component).await;
        assert_eq!(realm_route_results.len(), 1);
        let realm_route_result = &realm_route_results[0];
        assert_matches!(realm_route_result.error, None);
        assert_eq!(
            realm_route_result.source,
            Some(CapabilitySource::Framework(FrameworkSource {
                capability: InternalCapability::Protocol(
                    "fuchsia.component.Realm".parse().unwrap()
                ),
                moniker: a_component.moniker().clone(),
            })),
        );
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///    \
    ///     b
    ///
    /// a: offers protocol /offer_from_cm_namespace/svc/foo from component manager's
    ///    namespace as bar
    /// b: uses protocol bar as /svc/hippo
    #[fuchsia::test]
    async fn route_offer_from_component_manager_namespace() {
        let offer_decl = OfferBuilder::protocol()
            .name("foo")
            .target_name("bar")
            .source(OfferSource::Parent)
            .target(offer_target_static_child("b"))
            .build();
        let use_decl = UseBuilder::protocol().name("bar").path("/svc/hippo").build();
        let capability_decl = CapabilityBuilder::protocol()
            .name("foo")
            .path("/offer_from_cm_namespace/svc/foo")
            .build();
        let components = vec![
            ("a", ComponentDeclBuilder::new().offer(offer_decl.clone()).child_default("b").build()),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        ];

        let mut builder = RoutingTestBuilderForAnalyzer::new("a", components);
        builder.set_namespace_capabilities(vec![capability_decl.clone()]);
        let test = builder.build().await;
        test.install_namespace_directory("/offer_from_cm_namespace");

        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_results = test.model.check_use_capability(&use_decl, &b_component).await;
        assert_eq!(route_results.len(), 1);
        let route_result = &route_results[0];
        assert!(route_result.error.is_none());
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Namespace(NamespaceSource {
                capability: capability_decl.into()
            })),
        );
    }

    ///  component manager builtin
    ///   |
    ///   a
    ///    \
    ///     b
    ///
    /// a: offers builtin protocol foo from component manager as bar
    /// b: uses protocol bar as /svc/hippo
    #[fuchsia::test]
    async fn route_map_offer_from_component_manager_builtin() {
        let offer_decl = OfferBuilder::protocol()
            .name("foo")
            .target_name("bar")
            .source(OfferSource::Parent)
            .target(offer_target_static_child("b"))
            .build();
        let use_decl = UseBuilder::protocol().name("bar").path("/svc/hippo").build();
        let capability_decl = CapabilityBuilder::protocol().name("foo").build();
        let components = vec![
            ("a", ComponentDeclBuilder::new().offer(offer_decl.clone()).child_default("b").build()),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        ];

        let mut builder = RoutingTestBuilderForAnalyzer::new("a", components);
        builder.set_builtin_capabilities(vec![capability_decl.clone()]);
        let test = builder.build().await;

        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_results = test.model.check_use_capability(&use_decl, &b_component).await;
        assert_eq!(route_results.len(), 1);
        let route_result = &route_results[0];
        assert!(route_result.error.is_none());

        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Builtin(BuiltinSource { capability: capability_decl.into() })),
        );
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: creates environment "env" and registers resolver "base" from c.
    /// b: resolved by resolver "base" through "env".
    /// c: exposes resolver "base" from self.
    #[fuchsia::test]
    async fn route_map_resolver_from_parent_environment() {
        let a_url = make_test_url("a");
        let b_url = "base://b/".to_string();
        let c_url = make_test_url("c");

        let registration_decl = ResolverRegistration {
            resolver: "base".parse().unwrap(),
            source: RegistrationSource::Child("c".to_string()),
            scheme: "base".parse().unwrap(),
        };
        let expose_decl =
            ExposeBuilder::resolver().name("base").source(ExposeSource::Self_).build();
        let resolver_decl = CapabilityBuilder::resolver().name("base").build();

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new_empty_component()
                    .child(ChildBuilder::new().name("b").url(&b_url).environment("env"))
                    .child(ChildBuilder::new().name("c"))
                    .environment(
                        EnvironmentBuilder::new().name("env").resolver(registration_decl.clone()),
                    )
                    .build(),
            ),
            (b_url, ComponentDeclBuilder::new().build()),
            (
                c_url,
                ComponentDeclBuilder::new()
                    .expose(expose_decl.clone())
                    .capability(resolver_decl.clone())
                    .build(),
            ),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");

        let route_result = test.model.check_resolver(&b_component);

        assert_eq!(route_result.using_node, Moniker::parse_str("b").unwrap());
        assert_eq!(route_result.capability, Some("base".parse().unwrap()));
        assert!(route_result.error.is_none());
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                capability: resolver_decl.into(),
                moniker: vec!["c"].try_into().unwrap(),
            })),
        );
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: creates environment "env" and registers resolver "base" from self.
    /// b: has environment "env" extending the realm's environment.
    /// c: inherits "env" and is resolved by "base" from grandparent.
    #[fuchsia::test]
    async fn route_map_resolver_from_grandparent_environment() {
        let a_url = make_test_url("a");
        let b_url = make_test_url("b");
        let c_url = "base://c/".to_string();

        let registration_decl = ResolverRegistration {
            resolver: "base".parse().unwrap(),
            source: RegistrationSource::Self_,
            scheme: "base".into(),
        };
        let resolver_decl = CapabilityBuilder::resolver().name("base").build();
        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("env"))
                    .environment(
                        EnvironmentBuilder::new().name("env").resolver(registration_decl.clone()),
                    )
                    .capability(resolver_decl.clone())
                    .build(),
            ),
            (
                b_url,
                ComponentDeclBuilder::new_empty_component()
                    .child(ChildBuilder::new().name("c").url(&c_url))
                    .build(),
            ),
            (c_url, ComponentDeclBuilder::new_empty_component().build()),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let c_component =
            test.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");

        let route_result = test.model.check_resolver(&c_component);

        assert_eq!(route_result.using_node, Moniker::parse_str("b/c").unwrap());
        assert_eq!(route_result.capability, Some("base".parse().unwrap()));
        assert!(route_result.error.is_none());
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                capability: resolver_decl.into(),
                moniker: Moniker::root(),
            })),
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: is provided with the standard built-in boot resolver.
    /// b: is resolved by the standard boot resolver.
    #[fuchsia::test]
    async fn route_map_resolver_from_builtin_environment() {
        let a_url = make_test_url("a");
        let b_url = format!("{}://b/", BOOT_SCHEME);

        let boot_resolver_decl = CapabilityDecl::Resolver(ResolverDecl {
            name: BOOT_RESOLVER_NAME.parse().unwrap(),
            source_path: Some("/builtin/source/path".parse().unwrap()),
        });

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").url(&b_url))
                    .build(),
            ),
            (b_url, ComponentDeclBuilder::new().build()),
        ];

        let mut builder = RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components);
        builder.set_builtin_boot_resolver(component_internal::BuiltinBootResolver::Boot);
        builder.set_builtin_capabilities(vec![boot_resolver_decl.clone()]);
        let test = builder.build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");

        let route_map = test.model.check_resolver(&b_component);

        assert_eq!(route_map.using_node, Moniker::parse_str("b").unwrap());
        assert_eq!(route_map.capability, Some(BOOT_RESOLVER_NAME.parse().unwrap()));
        assert!(route_map.error.is_none());
        assert_eq!(
            route_map.route,
            vec![RouteSegment::ProvideAsBuiltin { capability: boot_resolver_decl }]
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: creates environment "env" and registers resolver "test" from self.
    /// b: has environment "env" and a relative url that is resolved by resolver "test" from parent.
    #[fuchsia::test]
    async fn route_map_resolver_relative_child_url() {
        let a_url = make_test_url("a");
        let b_relative = "#b";
        let b_url = format!("{}{}", a_url, b_relative);

        let resolver_registration = ResolverRegistration {
            resolver: "test".parse().unwrap(),
            source: RegistrationSource::Self_,
            scheme: "test".into(),
        };
        let resolver_decl = CapabilityBuilder::resolver().name("test").build();
        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").url(b_relative).environment("env"))
                    .environment(
                        EnvironmentBuilder::new()
                            .name("env")
                            .resolver(resolver_registration.clone()),
                    )
                    .capability(resolver_decl.clone())
                    .build(),
            ),
            (b_url, ComponentDeclBuilder::new().build()),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");

        let route_result = test.model.check_resolver(&b_component);

        assert_eq!(route_result.using_node, Moniker::parse_str("b").unwrap());
        assert_eq!(route_result.capability, Some("test".parse().unwrap()));
        assert!(route_result.error.is_none());
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Component(ComponentSource {
                capability: resolver_decl.into(),
                moniker: Moniker::root(),
            })),
        );
    }

    /// a: is provided with the built-in ELF runner, and requires that runner
    ///    in its `ProgramDecl`.
    #[fuchsia::test]
    async fn route_map_program_runner_from_builtin_environment() {
        let elf_runner_decl = CapabilityDecl::Runner(RunnerDecl {
            name: "elf".parse().unwrap(),
            source_path: Some("/builtin/source/path".parse().unwrap()),
        });
        let component_decl =
            ComponentDeclBuilder::new_empty_component().program_runner("elf").build();

        let components = vec![("a", component_decl.clone())];

        let mut builder = RoutingTestBuilderForAnalyzer::new("a", components);
        builder.set_builtin_capabilities(vec![elf_runner_decl.clone()]);
        builder.register_mock_builtin_runner("elf");
        let test = builder.build().await;
        let a_component = test.look_up_instance(&Moniker::root()).await.expect("a instance");

        let route_result = test
            .model
            .check_program_runner(
                &component_decl.program.expect("expected ProgramDecl for a"),
                &a_component,
            )
            .expect("expected program runner route");

        assert!(route_result.error.is_none());
        assert_eq!(
            route_result.source,
            Some(CapabilitySource::Builtin(BuiltinSource { capability: elf_runner_decl.into() })),
        );
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: Creates environment "env".
    ///    Registers resolver "base" from self.
    ///    Registers runner "hobbit" from as "dwarf" from self.
    ///    Offers directory "foo_data" from self as "bar_data".
    /// b: Has environment "env".
    ///    Requires runner "dwarf", routed successfully from "env".
    ///    Requires resolver "base" to resolve child "c", routed successfully from "env".
    ///    Uses directory "bar_data", routed successfully from parent.
    ///    Exposes "bad_protocol" from child, routing should fail.
    ///    Uses event stream "started", but routing is not checked because the "event stream"
    ///    capability type is not selected.
    /// c: is resolved by resolver "base" from grandparent.
    #[fuchsia::test]
    async fn route_maps_all_routes_for_instance() {
        let a_url = make_test_url("a");
        let b_url = "base://b/".to_string();
        let c_url = "base://c/".to_string();

        let resolver_registration_decl = ResolverRegistration {
            resolver: "base_resolver".parse().unwrap(),
            source: RegistrationSource::Self_,
            scheme: "base".into(),
        };
        let runner_registration_decl = RunnerRegistration {
            source_name: "hobbit".parse().unwrap(),
            source: RegistrationSource::Self_,
            target_name: "dwarf".parse().unwrap(),
        };
        let resolver_decl = CapabilityBuilder::resolver().name("base_resolver").build();
        let runner_decl = CapabilityBuilder::runner().name("hobbit").build();
        let use_directory_decl =
            UseBuilder::directory().name("bar_data").path("/data/hippo").build();
        let offer_directory_decl = OfferBuilder::directory()
            .name("foo_data")
            .target_name("bar_data")
            .source(OfferSource::Self_)
            .target(offer_target_static_child("b"))
            .rights(fio::R_STAR_DIR)
            .build();
        let directory_decl =
            CapabilityBuilder::directory().name("foo_data").path("/foo/data").build();
        let expose_protocol_decl =
            ExposeBuilder::protocol().name("bad_protocol").source_static_child("c").build();
        let use_event_decl =
            UseBuilder::event_stream().name("started_on_a").path("/started").build();

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").url(&b_url).environment("env"))
                    .environment(
                        EnvironmentBuilder::new()
                            .name("env")
                            .resolver(resolver_registration_decl.clone())
                            .runner(runner_registration_decl.clone()),
                    )
                    .offer(offer_directory_decl.clone())
                    .capability(directory_decl.clone())
                    .capability(resolver_decl.clone())
                    .capability(runner_decl.clone())
                    .build(),
            ),
            (
                b_url,
                ComponentDeclBuilder::new_empty_component()
                    .child(ChildBuilder::new().name("c").url(&c_url))
                    .program_runner("dwarf")
                    .expose(expose_protocol_decl.clone())
                    .use_(use_directory_decl.clone())
                    .use_(use_event_decl)
                    .build(),
            ),
            (c_url, ComponentDeclBuilder::new_empty_component().build()),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let root_component = test.look_up_instance(&Moniker::root()).await.expect("root instance");

        let route_maps = test
            .model
            .check_routes_for_instance(
                &b_component,
                &HashSet::from_iter(
                    vec![
                        CapabilityTypeName::Resolver,
                        CapabilityTypeName::Runner,
                        CapabilityTypeName::Directory,
                        CapabilityTypeName::Protocol,
                    ]
                    .into_iter(),
                ),
            )
            .await;
        assert_eq!(route_maps.len(), 4);

        let directories =
            route_maps.get(&CapabilityTypeName::Directory).expect("expected directory results");
        assert_eq!(
            directories,
            &vec![VerifyRouteResult {
                using_node: Moniker::parse_str("b").unwrap(),
                target_decl: TargetDecl::Use(use_directory_decl.clone()),
                capability: Some("bar_data".parse().unwrap()),
                error: None,
                route: vec![
                    RouteSegment::UseBy {
                        moniker: vec!["b"].try_into().unwrap(),
                        capability: use_directory_decl,
                    },
                    RouteSegment::OfferBy {
                        moniker: Moniker::root(),
                        capability: offer_directory_decl,
                    },
                    RouteSegment::DeclareBy {
                        moniker: Moniker::root(),
                        capability: directory_decl.clone(),
                    }
                ],
                source: Some(CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::Directory(match directory_decl {
                        CapabilityDecl::Directory(decl) => decl,
                        _ => panic!("unexpected capability variant"),
                    }),
                    moniker: root_component.moniker().clone(),
                })),
            }]
        );

        let runners = route_maps.get(&CapabilityTypeName::Runner).expect("expected runner results");
        assert_eq!(
            runners,
            &vec![VerifyRouteResult {
                using_node: Moniker::parse_str("b").unwrap(),
                target_decl: TargetDecl::Use(UseDecl::Runner(UseRunnerDecl {
                    source: UseSource::Environment,
                    source_name: "dwarf".parse().unwrap(),
                    source_dictionary: Default::default(),
                })),
                capability: Some("dwarf".parse().unwrap()),
                error: None,
                route: vec![],
                source: Some(CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::Runner(match runner_decl {
                        CapabilityDecl::Runner(decl) => decl,
                        _ => panic!("unexpected capability variant"),
                    }),
                    moniker: root_component.moniker().clone(),
                })),
            }]
        );

        let resolvers =
            route_maps.get(&CapabilityTypeName::Resolver).expect("expected resolver results");
        assert_eq!(
            resolvers,
            &vec![VerifyRouteResult {
                using_node: Moniker::parse_str("b").unwrap(),
                target_decl: TargetDecl::ResolverFromEnvironment("base".to_string()),
                capability: Some("base_resolver".parse().unwrap()),
                error: None,
                route: vec![],
                source: Some(CapabilitySource::Component(ComponentSource {
                    capability: ComponentCapability::Resolver(match resolver_decl {
                        CapabilityDecl::Resolver(decl) => decl,
                        _ => panic!("unexpected capability variant"),
                    }),
                    moniker: root_component.moniker().clone(),
                })),
            }]
        );

        let protocols =
            route_maps.get(&CapabilityTypeName::Protocol).expect("expected protocol results");
        assert_eq!(
            protocols,
            &vec![VerifyRouteResult {
                using_node: Moniker::parse_str("b").unwrap(),
                target_decl: TargetDecl::Expose(expose_protocol_decl.clone()),
                capability: Some("bad_protocol".parse().unwrap()),
                error: Some(AnalyzerModelError::RoutingError(
                    RoutingError::ExposeFromChildExposeNotFound {
                        capability_id: "bad_protocol".to_string(),
                        child_moniker: "c".try_into().unwrap(),
                        moniker: b_component.moniker().clone(),
                    },
                )),
                route: vec![],
                source: None,
            }],
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: Offers protocol "fuchsia.examples.Echo" from void to b
    /// b: Uses "fuchsia.examples.Echo" optionally
    #[fuchsia::test]
    async fn route_maps_do_not_include_valid_void_routes() {
        let a_url = make_test_url("a");
        let b_url = "base://b/".to_string();

        let use_protocol_decl = UseBuilder::protocol()
            .name("fuchsia.examples.Echo")
            .availability(Availability::Optional)
            .build();
        let offer_protocol_decl = OfferBuilder::protocol()
            .name("fuchsia.examples.Echo")
            .source(OfferSource::Void)
            .target(offer_target_static_child("b"))
            .availability(Availability::Optional)
            .build();

        let components = vec![
            (
                a_url.clone(),
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").url(&b_url))
                    .offer(offer_protocol_decl.clone())
                    .build(),
            ),
            (
                b_url,
                ComponentDeclBuilder::new_empty_component()
                    .program_runner("dwarf")
                    .use_(use_protocol_decl.clone())
                    .build(),
            ),
        ];

        let test =
            RoutingTestBuilderForAnalyzer::new_with_custom_urls(a_url, components).build().await;
        let b_component =
            test.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");

        let route_maps = test
            .model
            .check_routes_for_instance(
                &b_component,
                &HashSet::from_iter(vec![CapabilityTypeName::Protocol].into_iter()),
            )
            .await;
        assert_eq!(route_maps.len(), 1);
        let protocols =
            route_maps.get(&CapabilityTypeName::Protocol).expect("expected protocol results");
        assert_eq!(protocols, &vec![]);
    }

    ///    a
    ///   /  \
    ///  b    c
    ///
    /// a: Offers dictionary "test-dict" from "b" to "c".
    /// b: Exposes a dynamically filled dictionary "test-dict"
    /// c: Uses the dynamically placed protocol "fuchsia.foo.Bar" from the "test-dict"
    #[fuchsia::test]
    async fn validate_routes_with_dynamic_dictionaries() {
        const DICT_NAME: &str = "test-dict";
        const ROUTER_PATH: &str = "/svc/fuchsia.component.sandbox.DictionaryRouter";
        const PROTOCOL_NAME: &str = "fuchsia.foo.Bar";
        const PROTOCOL_NOT_IN_CONFIG: &str = "fuchsia.foo.NotInConfig";

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child_default("b")
                    .child_default("c")
                    .offer(
                        OfferBuilder::protocol()
                            .name(PROTOCOL_NAME)
                            .from_dictionary(DICT_NAME)
                            .source_static_child("b")
                            .target_static_child("c"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name(PROTOCOL_NOT_IN_CONFIG)
                            .from_dictionary(DICT_NAME)
                            .source_static_child("b")
                            .target_static_child("c"),
                    )
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::dictionary().name(DICT_NAME).path(ROUTER_PATH))
                    .expose(ExposeBuilder::dictionary().name(DICT_NAME).source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(
                        UseBuilder::protocol()
                            .name(PROTOCOL_NAME)
                            .source(UseSource::Parent)
                            .build(),
                    )
                    .use_(
                        UseBuilder::protocol()
                            .name(PROTOCOL_NOT_IN_CONFIG)
                            .source(UseSource::Parent)
                            .build(),
                    )
                    .build(),
            ),
        ];
        let mut builder = RoutingTestBuilderForAnalyzer::new("a", components.clone());
        let b_moniker: Moniker = vec!["b"].try_into().unwrap();
        builder.set_dynamic_config(DynamicConfig {
            dictionaries: hashmap! {
                b_moniker => hashmap! {
                    cm_types::Name::new(DICT_NAME).unwrap() => vec![
                        (CapabilityTypeName::Protocol, cm_types::Name::new(PROTOCOL_NAME).unwrap()),
                    ],
                }
            },
            ..DynamicConfig::default()
        });
        let model = builder.build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: format!("/svc/{PROTOCOL_NAME}").parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: format!("/svc/{PROTOCOL_NOT_IN_CONFIG}").parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
            )
            .await;
    }
}
