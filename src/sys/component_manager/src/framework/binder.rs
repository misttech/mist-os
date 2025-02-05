// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::{StartReason, WeakComponentInstance};
use crate::model::routing::report_routing_failure;
use crate::model::start::Start;
use ::routing::RouteRequest;
use async_trait::async_trait;
use cm_types::Name;
use errors::ModelError;

use lazy_static::lazy_static;
use log::warn;
use routing::capability_source::InternalCapability;

lazy_static! {
    static ref BINDER_SERVICE: Name = "fuchsia.component.Binder".parse().unwrap();
    static ref DEBUG_REQUEST: RouteRequest = RouteRequest::UseProtocol(cm_rust::UseProtocolDecl {
        source: cm_rust::UseSource::Framework,
        source_name: BINDER_SERVICE.clone(),
        source_dictionary: Default::default(),
        target_path: cm_types::Path::new("/null").unwrap(),
        dependency_type: cm_rust::DependencyType::Strong,
        availability: Default::default(),
    });
}

/// Implementation of `fuchsia.component.Binder` FIDL protocol.
struct BinderCapabilityProvider {
    source: WeakComponentInstance,
    target: WeakComponentInstance,
}

impl BinderCapabilityProvider {
    pub fn new(source: WeakComponentInstance, target: WeakComponentInstance) -> Self {
        Self { source, target }
    }

    async fn bind(self: Box<Self>, server_end: zx::Channel) -> Result<(), ()> {
        let source = match self.source.upgrade().map_err(|e| ModelError::from(e)) {
            Ok(source) => source,
            Err(err) => {
                report_routing_failure_to_target(self.target, err).await;
                return Err(());
            }
        };

        let start_reason = StartReason::AccessCapability {
            target: self.target.moniker.clone(),
            name: BINDER_SERVICE.clone(),
        };
        match source.ensure_started(&start_reason).await {
            Ok(_) => {
                source.scope_to_runtime(server_end).await;
            }
            Err(err) => {
                report_routing_failure_to_target(self.target, err.into()).await;
                return Err(());
            }
        }
        Ok(())
    }
}

#[async_trait]
impl InternalCapabilityProvider for BinderCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let _ = self.bind(server_end).await;
    }
}

pub struct BinderFrameworkCapability {}

impl BinderFrameworkCapability {
    pub fn new() -> Self {
        Self {}
    }
}

impl FrameworkCapability for BinderFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&BINDER_SERVICE)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(BinderCapabilityProvider::new(scope, target))
    }
}

async fn report_routing_failure_to_target(target: WeakComponentInstance, err: ModelError) {
    match target.upgrade().map_err(|e| ModelError::from(e)) {
        Ok(target) => {
            report_routing_failure(&*DEBUG_REQUEST, DEBUG_REQUEST.availability(), &target, &err)
                .await;
        }
        Err(err) => {
            warn!(moniker:% = target.moniker, error:% = err; "failed to upgrade reference");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_environment::BuiltinEnvironment;
    use crate::model::events::source::EventSource;
    use crate::model::events::stream::EventStream;
    use crate::model::testing::test_helpers::*;
    use assert_matches::assert_matches;
    use cm_rust::ComponentDecl;
    use cm_rust_testing::*;
    use cm_util::TaskGroup;
    use fidl::client::Client;
    use fidl::encoding::DefaultFuchsiaResourceDialect;
    use fidl::handle::AsyncChannel;
    use fidl_fuchsia_io as fio;
    use futures::lock::Mutex;
    use futures::StreamExt;
    use hooks::EventType;
    use moniker::Moniker;
    use std::sync::Arc;
    use vfs::directory::entry::OpenRequest;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path as VfsPath;
    use vfs::ToObjectRequest;

    struct BinderCapabilityTestFixture {
        builtin_environment: Arc<Mutex<BuiltinEnvironment>>,
    }

    impl BinderCapabilityTestFixture {
        async fn new(components: Vec<(&'static str, ComponentDecl)>) -> Self {
            let TestModelResult { builtin_environment, .. } =
                TestEnvironmentBuilder::new().set_components(components).build().await;

            BinderCapabilityTestFixture { builtin_environment }
        }

        async fn new_event_stream(&self, events: Vec<Name>) -> (EventSource, EventStream) {
            new_event_stream(self.builtin_environment.clone(), events).await
        }

        async fn provider(
            &self,
            source: Moniker,
            target: Moniker,
        ) -> Box<BinderCapabilityProvider> {
            let builtin_environment = self.builtin_environment.lock().await;
            let source = builtin_environment
                .model
                .root()
                .find_and_maybe_resolve(&source)
                .await
                .expect("failed to look up source moniker");
            let target = builtin_environment
                .model
                .root()
                .find_and_maybe_resolve(&target)
                .await
                .expect("failed to look up target moniker");

            Box::new(BinderCapabilityProvider::new(
                WeakComponentInstance::new(&source),
                WeakComponentInstance::new(&target),
            ))
        }
    }

    #[fuchsia::test]
    async fn component_starts_on_open() {
        let fixture = BinderCapabilityTestFixture::new(vec![
            (
                "root",
                ComponentDeclBuilder::new().child_default("source").child_default("target").build(),
            ),
            ("source", component_decl_with_test_runner()),
            ("target", component_decl_with_test_runner()),
        ])
        .await;
        let (_event_source, mut event_stream) = fixture
            .new_event_stream(vec![EventType::Resolved.into(), EventType::Started.into()])
            .await;
        let (_client_end, server_end) = zx::Channel::create();
        let moniker: Moniker = vec!["source"].try_into().unwrap();

        let task_group = TaskGroup::new();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        fixture
            .provider(moniker.clone(), vec!["target"].try_into().unwrap())
            .await
            .open(
                task_group.clone(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .await
            .expect("failed to call open()");
        task_group.join().await;

        assert!(event_stream.wait_until(EventType::Resolved, moniker.clone()).await.is_some());
        assert!(event_stream.wait_until(EventType::Started, moniker.clone()).await.is_some());
    }

    // TODO(https://fxbug.dev/42073225): Figure out a way to test this behavior.
    #[ignore]
    #[fuchsia::test]
    async fn channel_is_closed_if_component_does_not_exist() {
        let fixture = BinderCapabilityTestFixture::new(vec![(
            "root",
            ComponentDeclBuilder::new()
                .child_default("target")
                .child_default("unresolvable")
                .build(),
        )])
        .await;
        let (client_end, server_end) = zx::Channel::create();
        let moniker: Moniker = vec!["foo"].try_into().unwrap();

        let task_group = TaskGroup::new();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        fixture
            .provider(moniker, Moniker::root())
            .await
            .open(
                task_group.clone(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .await
            .expect("failed to call open()");
        task_group.join().await;

        let client_end = AsyncChannel::from_channel(client_end);
        let client = Client::<DefaultFuchsiaResourceDialect>::new(client_end, "binder_service");
        let mut event_receiver = client.take_event_receiver();
        assert_matches!(
            event_receiver.next().await,
            Some(Err(fidl::Error::ClientChannelClosed {
                status: zx::Status::NOT_FOUND,
                protocol_name: "binder_service"
            }))
        );
        assert_matches!(event_receiver.next().await, None);
    }
}
