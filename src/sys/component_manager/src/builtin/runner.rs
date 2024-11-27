// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::capability::{BuiltinCapability, CapabilityProvider};
use crate::model::component::WeakComponentInstance;
use ::routing::capability_source::InternalCapability;
use ::routing::policy::ScopedPolicyChecker;
use async_trait::async_trait;
use cm_config::SecurityPolicy;
use cm_types::Name;
use cm_util::TaskGroup;
use errors::CapabilityProviderError;

use std::sync::Arc;
use vfs::directory::entry::OpenRequest;

/// Trait for built-in runner services. Wraps the generic Runner trait to provide a
/// ScopedPolicyChecker for the realm of the component being started, so that runners can enforce
/// security policy.
pub trait BuiltinRunnerFactory: Send + Sync {
    /// Get a connection to a scoped runner by pipelining a
    /// `fuchsia.component.runner/ComponentRunner` server endpoint.
    fn get_scoped_runner(
        self: Arc<Self>,
        checker: ScopedPolicyChecker,
        open_request: OpenRequest<'_>,
    ) -> Result<(), zx::Status>;
}

/// Provides a hook for routing built-in runners to realms.
#[derive(Clone)]
pub struct BuiltinRunner {
    name: Name,
    factory: Arc<dyn BuiltinRunnerFactory>,
    security_policy: Arc<SecurityPolicy>,
    add_to_env: bool,
}

impl BuiltinRunner {
    pub fn new(
        name: Name,
        factory: Arc<dyn BuiltinRunnerFactory>,
        security_policy: Arc<SecurityPolicy>,
        add_to_env: bool,
    ) -> Self {
        Self { name, factory, security_policy, add_to_env }
    }

    pub fn name(&self) -> &Name {
        &self.name
    }

    pub fn factory(&self) -> &Arc<dyn BuiltinRunnerFactory> {
        &self.factory
    }

    pub fn add_to_env(&self) -> bool {
        self.add_to_env
    }
}

impl BuiltinCapability for BuiltinRunner {
    fn matches(&self, capability: &InternalCapability) -> bool {
        matches!(capability, InternalCapability::Runner(n) if *n == self.name)
    }

    fn new_provider(&self, target: WeakComponentInstance) -> Box<dyn CapabilityProvider> {
        let checker = ScopedPolicyChecker::new(self.security_policy.clone(), target.moniker);
        let runner = self.factory.clone();
        Box::new(RunnerCapabilityProvider::new(runner, checker))
    }
}

/// Allows a Rust `Runner` object to be treated as a generic capability,
/// as is required by the capability routing code.
#[derive(Clone)]
struct RunnerCapabilityProvider {
    factory: Arc<dyn BuiltinRunnerFactory>,
    checker: ScopedPolicyChecker,
}

impl RunnerCapabilityProvider {
    fn new(factory: Arc<dyn BuiltinRunnerFactory>, checker: ScopedPolicyChecker) -> Self {
        RunnerCapabilityProvider { factory, checker }
    }
}

#[async_trait]
impl CapabilityProvider for RunnerCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        self.factory
            .clone()
            .get_scoped_runner(self.checker, open_request)
            .map_err(|err| CapabilityProviderError::VfsOpenError(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::mocks::MockRunner;
    use crate::model::testing::routing_test_helpers::*;
    use anyhow::Error;
    use assert_matches::assert_matches;
    use cm_rust::{CapabilityDecl, RunnerDecl};
    use cm_rust_testing::*;
    use futures::prelude::*;
    use moniker::Moniker;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::ToObjectRequest;
    use {fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_io as fio};

    fn sample_start_info(name: &str) -> fcrunner::ComponentStartInfo {
        fcrunner::ComponentStartInfo {
            resolved_url: Some(name.to_string()),
            program: None,
            ns: Some(vec![]),
            outgoing_dir: None,
            runtime_dir: None,
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    // Test sending a start command to a failing runner.
    #[fuchsia::test]
    async fn capability_provider_error_from_runner() -> Result<(), Error> {
        // Set up a capability provider wrapping a runner that returns an error on our
        // target URL.
        let mock_runner = Arc::new(MockRunner::new());
        mock_runner.add_failing_url("xxx://failing");
        let policy = Arc::new(SecurityPolicy::default());
        let moniker = Moniker::try_from(vec!["foo"]).unwrap();
        let checker = ScopedPolicyChecker::new(policy, moniker);
        let provider = Box::new(RunnerCapabilityProvider { factory: mock_runner, checker });

        // Open a connection to the provider.
        let (client, server) = fidl::endpoints::create_proxy::<fcrunner::ComponentRunnerMarker>()?;
        let server = server.into_channel();
        let task_group = TaskGroup::new();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server);
        provider
            .open(
                task_group.clone(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    Path::dot(),
                    &mut object_request,
                ),
            )
            .await?;

        // Ensure errors are propagated back to the caller.
        //
        // We make multiple calls over the same channel to ensure that the channel remains open
        // even after errors.
        for _ in 0..3i32 {
            let (client_controller, server_controller) =
                fidl::endpoints::create_endpoints::<fcrunner::ComponentControllerMarker>();
            client.start(sample_start_info("xxx://failing"), server_controller)?;
            let actual = client_controller
                .into_proxy()
                .take_event_stream()
                .next()
                .await
                .unwrap()
                .err()
                .unwrap();
            assert_matches!(
                actual,
                fidl::Error::ClientChannelClosed { status: zx::Status::UNAVAILABLE, .. }
            );
        }

        Ok(())
    }

    //   (cm)
    //    |
    //    a
    //
    // a: uses runner "elf" offered from the component mananger.
    #[fuchsia::test]
    async fn use_runner_from_component_manager() {
        let mock_runner = Arc::new(MockRunner::new());

        let components = vec![(
            "a",
            ComponentDeclBuilder::new_empty_component().program_runner("my_runner").build(),
        )];

        // Set up the system.
        let universe = RoutingTestBuilder::new("a", components)
            .set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
                name: "my_runner".parse().unwrap(),
                source_path: None,
            })])
            .add_builtin_runner("my_runner", mock_runner.clone())
            .build()
            .await;

        // Bind the root component.
        universe.start_instance(&Moniker::root()).await.expect("bind failed");

        // Ensure the instance starts up.
        mock_runner.wait_for_url("test:///a_resolved").await;
    }

    //   (cm)
    //    |
    //    a
    //    |
    //    b
    //
    // (cm): registers runner "elf".
    // b: uses runner "elf".
    #[fuchsia::test]
    async fn use_runner_from_component_manager_environment() {
        let mock_runner = Arc::new(MockRunner::new());

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .child_default("b")
                    .program_runner("elf")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("elf").build()),
        ];

        // Set up the system.
        let universe = RoutingTestBuilder::new("a", components)
            .set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
                name: "elf".parse().unwrap(),
                source_path: None,
            })])
            .add_builtin_runner("elf", mock_runner.clone())
            .build()
            .await;

        // Bind the child component.
        universe.start_instance(&vec!["b"].try_into().unwrap()).await.expect("bind failed");

        // Ensure the instances started up.
        mock_runner.wait_for_urls(&["test:///b_resolved"]).await;
    }
}
