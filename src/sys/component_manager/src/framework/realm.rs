// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.cti

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::framework;
use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::model::Model;
use ::routing::capability_source::InternalCapability;
use ::routing::component_instance::ComponentInstanceInterface;
use anyhow::Error;
use async_trait::async_trait;
use cm_config::RuntimeConfig;
use cm_rust::FidlIntoNative;
use cm_types::{Name, OPEN_FLAGS_MAX_POSSIBLE_RIGHTS};
use errors::OpenExposedDirError;
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use futures::prelude::*;
use lazy_static::lazy_static;
use moniker::{ChildName, Moniker};
use std::cmp;
use std::sync::{Arc, Weak};
use tracing::{debug, error, warn};
use vfs::directory::entry::OpenRequest;
use vfs::path::Path;
use vfs::ToObjectRequest;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fcomponent::RealmMarker::PROTOCOL_NAME.parse().unwrap();
}

struct RealmCapabilityProvider {
    scope_moniker: Moniker,
    model: Weak<Model>,
    config: Arc<RuntimeConfig>,
}

#[async_trait]
impl InternalCapabilityProvider for RealmCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fcomponent::RealmMarker>::new(server_end);
        // We only need to look up the component matching this scope.
        // These operations should all work, even if the component is not running.
        let Some(model) = self.model.upgrade() else {
            return;
        };
        let Ok(component) = model.root().find_and_maybe_resolve(&self.scope_moniker).await else {
            return;
        };
        drop(model);
        let weak = WeakComponentInstance::new(&component);
        drop(component);
        let serve_result = self.serve(weak, server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            // TODO: Set an epitaph to indicate this was an unexpected error.
            warn!(%error, "serve failed");
        }
    }
}

pub struct Realm {
    model: Weak<Model>,
    config: Arc<RuntimeConfig>,
}

impl Realm {
    pub fn new(model: Weak<Model>, config: Arc<RuntimeConfig>) -> Self {
        Self { model, config }
    }
}

impl FrameworkCapability for Realm {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(RealmCapabilityProvider {
            scope_moniker: scope.moniker,
            model: self.model.clone(),
            config: self.config.clone(),
        })
    }
}

impl RealmCapabilityProvider {
    async fn serve(
        &self,
        component: WeakComponentInstance,
        mut stream: fcomponent::RealmRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            let method_name = request.method_name();
            let result = self.handle_request(request, &component).await;
            match result {
                // If the error was PEER_CLOSED then we don't need to log it as a client can
                // disconnect while we are processing its request.
                Err(error) if !error.is_closed() => {
                    warn!(%method_name, %error, "Couldn't send Realm response");
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn handle_request(
        &self,
        request: fcomponent::RealmRequest,
        component: &WeakComponentInstance,
    ) -> Result<(), fidl::Error> {
        match request {
            fcomponent::RealmRequest::CreateChild { responder, collection, decl, args } => {
                let res =
                    async { Self::create_child(component, collection, decl, args).await }.await;
                responder.send(res)?;
            }
            fcomponent::RealmRequest::DestroyChild { responder, child } => {
                let res = Self::destroy_child(component, child).await;
                responder.send(res)?;
            }
            fcomponent::RealmRequest::ListChildren { responder, collection, iter } => {
                let res = Self::list_children(
                    component,
                    self.config.list_children_batch_size,
                    collection,
                    iter,
                )
                .await;
                responder.send(res)?;
            }
            fcomponent::RealmRequest::OpenExposedDir { responder, child, exposed_dir } => {
                let res = Self::open_exposed_dir(component, child, exposed_dir).await;
                responder.send(res)?;
            }
            fcomponent::RealmRequest::OpenController { child, controller, responder } => {
                let res = Self::open_controller(component, child, controller).await;
                responder.send(res)?;
            }
        }
        Ok(())
    }

    async fn create_child(
        weak: &WeakComponentInstance,
        collection: fdecl::CollectionRef,
        child_decl: fdecl::Child,
        child_args: fcomponent::CreateChildArgs,
    ) -> Result<(), fcomponent::Error> {
        let component = weak.upgrade().map_err(|_| fcomponent::Error::InstanceDied)?;

        cm_fidl_validator::validate_dynamic_child(&child_decl).map_err(|error| {
            warn!(%error, "failed to create dynamic child. child decl is invalid");
            fcomponent::Error::InvalidArguments
        })?;
        let child_decl = child_decl.fidl_into_native();

        component.add_dynamic_child(collection.name.clone(), &child_decl, child_args).await.map_err(
            |err| {
                warn!(
                    "Failed to create child \"{}\" in collection \"{}\" of component \"{}\": {}",
                    child_decl.name, collection.name, component.moniker, err
                );

                err.into()
            },
        )
    }

    async fn open_controller(
        component: &WeakComponentInstance,
        child: fdecl::ChildRef,
        controller: ServerEnd<fcomponent::ControllerMarker>,
    ) -> Result<(), fcomponent::Error> {
        match Self::get_child(component, child.clone()).await? {
            Some(child) => {
                child.nonblocking_task_group().spawn(framework::controller::run_controller(
                    child.as_weak(),
                    controller.into_stream().unwrap(),
                ));
            }
            None => {
                debug!(?child, "open_controller() failed: instance not found");
                return Err(fcomponent::Error::InstanceNotFound);
            }
        }
        Ok(())
    }

    async fn open_exposed_dir(
        component: &WeakComponentInstance,
        child: fdecl::ChildRef,
        exposed_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> Result<(), fcomponent::Error> {
        match Self::get_child(component, child.clone()).await? {
            Some(child) => {
                // Resolve child in order to instantiate exposed_dir.
                child.resolve().await.map_err(|e| {
                    warn!(
                        "resolve failed for child {:?} of component {}: {}",
                        child, component.moniker, e
                    );
                    return fcomponent::Error::InstanceCannotResolve;
                })?;
                // open_exposed does not have a rights input parameter, so this
                // makes use of the  POSIX_[WRITABLE|EXECUTABLE] flags to open
                // a connection with those rights if available from the parent
                // directory connection but without failing if not available.
                let flags = OPEN_FLAGS_MAX_POSSIBLE_RIGHTS | fio::OpenFlags::DIRECTORY;
                let mut object_request = flags.to_object_request(exposed_dir);
                child
                    .open_exposed(OpenRequest::new(
                        child.execution_scope.clone(),
                        flags,
                        Path::dot(),
                        &mut object_request,
                    ))
                    .await
                    .map_err(|error| match error {
                        OpenExposedDirError::InstanceDestroyed
                        | OpenExposedDirError::InstanceNotResolved => {
                            fcomponent::Error::InstanceDied
                        }
                        OpenExposedDirError::Open(_) => fcomponent::Error::Internal,
                    })?;
            }
            None => {
                debug!(?child, "open_exposed_dir() failed: instance not found");
                return Err(fcomponent::Error::InstanceNotFound);
            }
        }
        Ok(())
    }

    async fn destroy_child(
        component: &WeakComponentInstance,
        child: fdecl::ChildRef,
    ) -> Result<(), fcomponent::Error> {
        let component = component.upgrade().map_err(|_| fcomponent::Error::InstanceDied)?;
        child.collection.as_ref().ok_or(fcomponent::Error::InvalidArguments)?;
        let child_moniker = ChildName::try_new(&child.name, child.collection.as_ref())
            .map_err(|_| fcomponent::Error::InvalidArguments)?;
        component.remove_dynamic_child(&child_moniker).await.map_err(|error| {
            debug!(%error, ?child, "remove_dynamic_child() failed");
            error
        })?;
        Ok(())
    }

    async fn get_child(
        parent: &WeakComponentInstance,
        child: fdecl::ChildRef,
    ) -> Result<Option<Arc<ComponentInstance>>, fcomponent::Error> {
        let parent = parent.upgrade().map_err(|_| fcomponent::Error::InstanceDied)?;
        let state = parent.lock_resolved_state().await.map_err(|error| {
            debug!(%error, moniker=%parent.moniker, "failed to resolve instance");
            fcomponent::Error::InstanceCannotResolve
        })?;
        let child_moniker = ChildName::try_new(&child.name, child.collection.as_ref())
            .map_err(|_| fcomponent::Error::InvalidArguments)?;
        Ok(state.get_child(&child_moniker).map(|r| r.clone()))
    }

    async fn list_children(
        component: &WeakComponentInstance,
        batch_size: usize,
        collection: fdecl::CollectionRef,
        iter: ServerEnd<fcomponent::ChildIteratorMarker>,
    ) -> Result<(), fcomponent::Error> {
        let component = component.upgrade().map_err(|_| fcomponent::Error::InstanceDied)?;
        let state = component.lock_resolved_state().await.map_err(|error| {
            error!(%error, "failed to resolve InstanceState");
            fcomponent::Error::Internal
        })?;
        let decl = state.decl();
        decl.find_collection(&collection.name).ok_or(fcomponent::Error::CollectionNotFound)?;
        let mut children: Vec<_> = state
            .children()
            .filter_map(|(m, _)| match m.collection() {
                Some(c) => {
                    if c.as_str() == &collection.name {
                        Some(fdecl::ChildRef {
                            name: m.name().to_string(),
                            collection: m.collection().map(|s| s.to_string()),
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        children.sort_unstable_by(|a, b| {
            let a = &a.name;
            let b = &b.name;
            if a == b {
                cmp::Ordering::Equal
            } else if a < b {
                cmp::Ordering::Less
            } else {
                cmp::Ordering::Greater
            }
        });
        let stream = iter.into_stream().map_err(|_| fcomponent::Error::AccessDenied)?;
        fasync::Task::spawn(async move {
            if let Err(error) = Self::serve_child_iterator(children, stream, batch_size).await {
                // TODO: Set an epitaph to indicate this was an unexpected error.
                warn!(%error, "serve_child_iterator failed");
            }
        })
        .detach();
        Ok(())
    }

    async fn serve_child_iterator(
        children: Vec<fdecl::ChildRef>,
        mut stream: fcomponent::ChildIteratorRequestStream,
        batch_size: usize,
    ) -> Result<(), Error> {
        let mut iter = children.chunks(batch_size);
        while let Some(request) = stream.try_next().await? {
            match request {
                fcomponent::ChildIteratorRequest::Next { responder } => {
                    responder.send(iter.next().unwrap_or(&[]))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_environment::BuiltinEnvironment;
    use crate::capability;
    use crate::model::component::StartReason;
    use crate::model::events::source::EventSource;
    use crate::model::events::stream::EventStream;
    use crate::model::testing::mocks::*;
    use crate::model::testing::out_dir::OutDir;
    use crate::model::testing::test_helpers::*;
    use crate::model::testing::test_hook::*;
    use assert_matches::assert_matches;
    use cm_rust::{ComponentDecl, ExposeSource};
    use cm_rust_testing::*;
    use fidl::endpoints;
    use fuchsia_component::client;
    use futures::lock::Mutex;
    use hooks::EventType;
    use routing_test_helpers::component_decl_with_exposed_binder;
    use std::collections::HashSet;
    use {
        fidl_fidl_examples_routing_echo as echo, fidl_fuchsia_component as fcomponent,
        fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    };

    struct RealmCapabilityTest {
        builtin_environment: Option<Arc<Mutex<BuiltinEnvironment>>>,
        mock_runner: Arc<MockRunner>,
        component: Option<Arc<ComponentInstance>>,
        _host: Realm,
        realm_proxy: fcomponent::RealmProxy,
        hook: Arc<TestHook>,
    }

    impl RealmCapabilityTest {
        async fn new(
            components: Vec<(&'static str, ComponentDecl)>,
            component_moniker: Moniker,
        ) -> Self {
            // Init model.
            let config = RuntimeConfig { list_children_batch_size: 2, ..Default::default() };
            let hook = Arc::new(TestHook::new());
            let TestModelResult { model, builtin_environment, mock_runner, .. } =
                TestEnvironmentBuilder::new()
                    .set_runtime_config(config)
                    .set_components(components)
                    // Install TestHook at the front so that when we receive an event the hook has
                    // already run so the result is reflected in its printout
                    .set_front_hooks(hook.hooks())
                    .build()
                    .await;

            // Look up and start component.
            let component = model
                .root()
                .start_instance(&component_moniker, &StartReason::Eager)
                .await
                .expect("failed to start component");

            // Host framework service.
            let host = Realm::new(Arc::downgrade(&model), model.context().runtime_config().clone());
            let (realm_proxy, server) =
                endpoints::create_proxy::<fcomponent::RealmMarker>().unwrap();
            capability::open_framework(&host, &component, server.into()).await.unwrap();
            Self {
                builtin_environment: Some(builtin_environment),
                mock_runner,
                component: Some(component),
                _host: host,
                realm_proxy,
                hook,
            }
        }

        fn component(&self) -> &Arc<ComponentInstance> {
            self.component.as_ref().unwrap()
        }

        fn drop_component(&mut self) {
            self.component = None;
            self.builtin_environment = None;
        }

        async fn new_event_stream(&self, events: Vec<Name>) -> (EventSource, EventStream) {
            new_event_stream(
                self.builtin_environment.as_ref().expect("builtin_environment is none").clone(),
                events,
            )
            .await
        }
    }

    fn child_decl(name: &str) -> fdecl::Child {
        fdecl::Child {
            name: Some(name.to_owned()),
            url: Some(format!("test:///{}", name)),
            startup: Some(fdecl::StartupMode::Lazy),
            environment: None,
            on_terminate: None,
            ..Default::default()
        }
    }

    #[fuchsia::test]
    async fn create_dynamic_child() {
        // Set up model and realm service.
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                (
                    "system",
                    ComponentDeclBuilder::new()
                        .collection(CollectionBuilder::new().name("coll").allow_long_names())
                        .build(),
                ),
                // Eagerly launched so it needs a definition
                ("b", ComponentDeclBuilder::new().build()),
            ],
            vec!["system"].try_into().unwrap(),
        )
        .await;

        let (_event_source, mut event_stream) =
            test.new_event_stream(vec![EventType::Started.into()]).await;

        // Test that a dynamic child with a long name can also be created.
        let long_name = &"c".repeat(cm_types::MAX_LONG_NAME_LENGTH);

        // Create children "a", "b", and "<long_name>" in collection.
        // each.
        let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        {
            // Create a child
            test.realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl("a"),
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap()
                .unwrap();
        }
        {
            // Create a child (eager)
            let mut child_decl = child_decl("b");
            child_decl.startup = Some(fdecl::StartupMode::Eager);
            test.realm_proxy
                .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
                .await
                .unwrap()
                .unwrap();

            // Ensure that an event exists for the new child
            event_stream
                .wait_until(EventType::Started, "system/coll:b".try_into().unwrap())
                .await
                .unwrap();
        }
        {
            // Create a child (long name)
            test.realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl(long_name),
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap()
                .unwrap();
        }

        // Verify that the component topology matches expectations.
        let actual_children = get_live_children(test.component()).await;
        let mut expected_children: HashSet<ChildName> = HashSet::new();
        expected_children.insert("coll:a".try_into().unwrap());
        expected_children.insert("coll:b".try_into().unwrap());
        expected_children.insert(format!("coll:{}", long_name).as_str().try_into().unwrap());
        assert_eq!(actual_children, expected_children);
    }

    #[fuchsia::test]
    async fn create_dynamic_child_errors() {
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                (
                    "system",
                    ComponentDeclBuilder::new()
                        .collection_default("coll")
                        .collection(
                            CollectionBuilder::new()
                                .name("pcoll")
                                .durability(fdecl::Durability::Transient)
                                .allow_long_names(),
                        )
                        .collection(
                            CollectionBuilder::new()
                                .name("dynoff")
                                .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                        )
                        .build(),
                ),
            ],
            vec!["system"].try_into().unwrap(),
        )
        .await;

        // Invalid arguments.
        {
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let child_decl = fdecl::Child {
                name: Some("a".to_string()),
                url: None,
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }
        {
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let child_decl = fdecl::Child {
                name: Some("a".to_string()),
                url: Some("test:///a".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: Some("env".to_string()),
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Long dynamic child name violations.
        {
            // Name exceeds MAX_NAME_LENGTH when `allow_long_names` is not set.
            // The FIDL call succeeds but the server responds with an error.
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let child_decl = fdecl::Child {
                name: Some("a".repeat(cm_types::MAX_NAME_LENGTH + 1).to_string()),
                url: None,
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);

            // Name length exceeds the MAX_LONG_NAME_LENGTH when `allow_long_names` is set.
            // In this case the FIDL call fails to encode because the name field
            // is defined in the FIDL library as `string:MAX_LONG_NAME_LENGTH`.
            let collection_ref = fdecl::CollectionRef { name: "pcoll".to_string() };
            let child_decl = fdecl::Child {
                name: Some("a".repeat(cm_types::MAX_LONG_NAME_LENGTH + 1).to_string()),
                url: None,
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
                .await
                .expect_err("unexpected success");
            // When exceeding the long max name length, the FIDL call itself
            // fails because the name field is defined as `string:1024`.
            assert_matches!(err, fidl::Error::StringTooLong { .. });
        }

        // Instance already exists.
        {
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let res = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl("a"),
                    fcomponent::CreateChildArgs::default(),
                )
                .await;
            res.expect("fidl call failed").expect("failed to create child a");
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl("a"),
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InstanceAlreadyExists);
        }

        // Collection not found.
        {
            let collection_ref = fdecl::CollectionRef { name: "nonexistent".to_string() };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl("a"),
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::CollectionNotFound);
        }

        fn sample_offer_from(source: fdecl::Ref) -> fdecl::Offer {
            fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source: Some(source),
                source_name: Some("foo".to_string()),
                target_name: Some("foo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })
        }

        // Disallowed dynamic offers specified.
        {
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let child_decl = fdecl::Child {
                name: Some("b".to_string()),
                url: Some("test:///b".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl,
                    fcomponent::CreateChildArgs {
                        dynamic_offers: Some(vec![sample_offer_from(fdecl::Ref::Parent(
                            fdecl::ParentRef {},
                        ))]),
                        ..Default::default()
                    },
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Malformed dynamic offers specified.
        {
            let collection_ref = fdecl::CollectionRef { name: "dynoff".to_string() };
            let child_decl = fdecl::Child {
                name: Some("b".to_string()),
                url: Some("test:///b".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl,
                    fcomponent::CreateChildArgs {
                        dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                            source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                            source_name: Some("foo".to_string()),
                            target_name: Some("foo".to_string()),
                            // Note: has no `dependency_type`.
                            ..Default::default()
                        })]),
                        ..Default::default()
                    },
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Dynamic offer source is a static component that doesn't exist.
        {
            let collection_ref = fdecl::CollectionRef { name: "dynoff".to_string() };
            let child_decl = fdecl::Child {
                name: Some("b".to_string()),
                url: Some("test:///b".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl,
                    fcomponent::CreateChildArgs {
                        dynamic_offers: Some(vec![sample_offer_from(fdecl::Ref::Child(
                            fdecl::ChildRef {
                                name: "does_not_exist".to_string(),
                                collection: None,
                            },
                        ))]),
                        ..Default::default()
                    },
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Source is a collection that doesn't exist (and using a Service).
        {
            let collection_ref = fdecl::CollectionRef { name: "dynoff".to_string() };
            let child_decl = fdecl::Child {
                name: Some("b".to_string()),
                url: Some("test:///b".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl,
                    fcomponent::CreateChildArgs {
                        dynamic_offers: Some(vec![fdecl::Offer::Service(fdecl::OfferService {
                            source: Some(fdecl::Ref::Collection(fdecl::CollectionRef {
                                name: "does_not_exist".to_string(),
                            })),
                            source_name: Some("foo".to_string()),
                            target_name: Some("foo".to_string()),
                            ..Default::default()
                        })]),
                        ..Default::default()
                    },
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Source is a component in the same collection that doesn't exist.
        {
            let collection_ref = fdecl::CollectionRef { name: "dynoff".to_string() };
            let child_decl = fdecl::Child {
                name: Some("b".to_string()),
                url: Some("test:///b".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl,
                    fcomponent::CreateChildArgs {
                        dynamic_offers: Some(vec![sample_offer_from(fdecl::Ref::Child(
                            fdecl::ChildRef {
                                name: "does_not_exist".to_string(),
                                collection: Some("dynoff".to_string()),
                            },
                        ))]),
                        ..Default::default()
                    },
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Source is the component itself, which would create a cycle.
        {
            let collection_ref = fdecl::CollectionRef { name: "dynoff".to_string() };
            let child_decl = fdecl::Child {
                name: Some("b".to_string()),
                url: Some("test:///b".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                environment: None,
                ..Default::default()
            };
            let err = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl,
                    fcomponent::CreateChildArgs {
                        dynamic_offers: Some(vec![sample_offer_from(fdecl::Ref::Child(
                            fdecl::ChildRef {
                                name: "b".to_string(),
                                collection: Some("dynoff".to_string()),
                            },
                        ))]),
                        ..Default::default()
                    },
                )
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }
    }

    #[fuchsia::test]
    async fn realm_instance_died() {
        let mut test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                ("system", ComponentDeclBuilder::new().collection_default("coll").build()),
            ],
            vec!["system"].try_into().unwrap(),
        )
        .await;
        let collection_ref = fdecl::CollectionRef { name: "coll".into() };
        test.realm_proxy
            .create_child(&collection_ref, &child_decl("a"), fcomponent::CreateChildArgs::default())
            .await
            .unwrap()
            .unwrap();

        // If the component is dropped, this should cancel the server task and close the channel.
        test.drop_component();
        assert_matches!(test.realm_proxy.take_event_stream().next().await, None);
    }

    #[fuchsia::test]
    async fn destroy_dynamic_child() {
        // Set up model and realm service.
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                ("system", ComponentDeclBuilder::new().collection_default("coll").build()),
                ("a", component_decl_with_exposed_binder()),
                ("b", component_decl_with_exposed_binder()),
            ],
            vec!["system"].try_into().unwrap(),
        )
        .await;

        let (_event_source, mut event_stream) = test
            .new_event_stream(vec![EventType::Stopped.into(), EventType::Destroyed.into()])
            .await;

        // Create children "a" and "b" in collection, and start them.
        for name in &["a", "b"] {
            let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
            let res = test
                .realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl(name),
                    fcomponent::CreateChildArgs::default(),
                )
                .await;
            res.expect("fidl call failed")
                .unwrap_or_else(|_| panic!("failed to create child {}", name));
            let child_ref =
                fdecl::ChildRef { name: name.to_string(), collection: Some("coll".to_string()) };
            let (exposed_dir, server_end) =
                endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let () = test
                .realm_proxy
                .open_exposed_dir(&child_ref, server_end)
                .await
                .expect("OpenExposedDir FIDL")
                .expect("OpenExposedDir Error");
            let _: fcomponent::BinderProxy =
                client::connect_to_protocol_at_dir_root::<fcomponent::BinderMarker>(&exposed_dir)
                    .expect("Connection to fuchsia.component.Binder");
        }

        let child = get_live_child(test.component(), "coll:a").await;
        let instance_id = get_incarnation_id(test.component(), "coll:a").await;
        assert_eq!(child.component_url, "test:///a");
        assert_eq!(instance_id, 1);
        let child = get_live_child(test.component(), "coll:b").await;
        let instance_id = get_incarnation_id(test.component(), "coll:b").await;
        assert_eq!(child.component_url, "test:///b");
        assert_eq!(instance_id, 2);

        // Destroy "a". "a" is no longer live from the client's perspective, although it's still
        // being destroyed.
        let child_ref =
            fdecl::ChildRef { name: "a".to_string(), collection: Some("coll".to_string()) };
        let (f, destroy_handle) = test.realm_proxy.destroy_child(&child_ref).remote_handle();
        fasync::Task::spawn(f).detach();

        // The component should be stopped (shut down) before it is destroyed.
        event_stream
            .wait_until(EventType::Stopped, vec!["system", "coll:a"].try_into().unwrap())
            .await
            .unwrap();
        event_stream
            .wait_until(EventType::Destroyed, vec!["system", "coll:a"].try_into().unwrap())
            .await
            .unwrap();

        // "a" is fully deleted now.
        assert!(!has_child(test.component(), "coll:a").await);
        {
            let actual_children = get_live_children(test.component()).await;
            let mut expected_children: HashSet<ChildName> = HashSet::new();
            expected_children.insert("coll:b".try_into().unwrap());
            let child_b = get_live_child(test.component(), "coll:b").await;
            assert!(!execution_is_shut_down(&child_b).await);
            assert_eq!(actual_children, expected_children);
        }

        let res = destroy_handle.await;
        res.expect("fidl call failed").expect("failed to destroy child a");

        // Recreate "a" and verify "a" is back (but it's a different "a"). The old "a" is gone
        // from the client's point of view, but it hasn't been cleaned up yet.
        let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        let child_decl = fdecl::Child {
            name: Some("a".to_string()),
            url: Some("test:///a_alt".to_string()),
            startup: Some(fdecl::StartupMode::Lazy),
            environment: None,
            ..Default::default()
        };
        let res = test
            .realm_proxy
            .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
            .await;
        res.expect("fidl call failed").expect("failed to recreate child a");

        let child = get_live_child(test.component(), "coll:a").await;
        let instance_id = get_incarnation_id(test.component(), "coll:a").await;
        assert_eq!(child.component_url, "test:///a_alt");
        assert_eq!(instance_id, 3);
    }

    #[fuchsia::test]
    async fn destroy_dynamic_child_errors() {
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                ("system", ComponentDeclBuilder::new().collection_default("coll").build()),
            ],
            vec!["system"].try_into().unwrap(),
        )
        .await;

        // Create child "a" in collection.
        let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        let res = test
            .realm_proxy
            .create_child(&collection_ref, &child_decl("a"), fcomponent::CreateChildArgs::default())
            .await;
        res.expect("fidl call failed").expect("failed to create child a");

        // Invalid arguments.
        {
            let child_ref = fdecl::ChildRef { name: "a".to_string(), collection: None };
            let err = test
                .realm_proxy
                .destroy_child(&child_ref)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Instance not found.
        {
            let child_ref =
                fdecl::ChildRef { name: "b".to_string(), collection: Some("coll".to_string()) };
            let err = test
                .realm_proxy
                .destroy_child(&child_ref)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InstanceNotFound);
        }
    }

    #[fuchsia::test]
    async fn dynamic_single_run_child() {
        // Set up model and realm service.
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                (
                    "system",
                    ComponentDeclBuilder::new()
                        .collection(
                            CollectionBuilder::new()
                                .name("coll")
                                .durability(fdecl::Durability::SingleRun),
                        )
                        .build(),
                ),
                ("a", component_decl_with_test_runner()),
            ],
            vec!["system"].try_into().unwrap(),
        )
        .await;

        let (_event_source, mut event_stream) = test
            .new_event_stream(vec![EventType::Started.into(), EventType::Destroyed.into()])
            .await;

        // Create child "a" in collection. Expect a Started event.
        let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        test.realm_proxy
            .create_child(&collection_ref, &child_decl("a"), fcomponent::CreateChildArgs::default())
            .await
            .unwrap()
            .unwrap();
        event_stream
            .wait_until(EventType::Started, vec!["system", "coll:a"].try_into().unwrap())
            .await
            .unwrap();

        // Started action completes.

        let child = {
            let state = test.component().lock_resolved_state().await.unwrap();
            let child = state.children().next().unwrap();
            assert_eq!("a", child.0.name().as_str());
            child.1.clone()
        };

        // The stop should trigger a delete/purge.
        child.stop_instance_internal(false).await.unwrap();

        event_stream
            .wait_until(EventType::Destroyed, vec!["system", "coll:a"].try_into().unwrap())
            .await
            .unwrap();

        // Verify that the component topology matches expectations.
        let actual_children = get_live_children(test.component()).await;
        let expected_children: HashSet<ChildName> = HashSet::new();
        assert_eq!(actual_children, expected_children);
    }

    #[fuchsia::test]
    async fn list_children_errors() {
        // Create a root component with a collection.
        let test = RealmCapabilityTest::new(
            vec![("root", ComponentDeclBuilder::new().collection_default("coll").build())],
            Moniker::root(),
        )
        .await;

        // Collection not found.
        {
            let collection_ref = fdecl::CollectionRef { name: "nonexistent".to_string() };
            let (_, server_end) = endpoints::create_proxy().unwrap();
            let err = test
                .realm_proxy
                .list_children(&collection_ref, server_end)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::CollectionNotFound);
        }
    }

    #[fuchsia::test]
    async fn open_exposed_dir() {
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                (
                    "system",
                    ComponentDeclBuilder::new()
                        .protocol_default("foo")
                        .expose(
                            ExposeBuilder::protocol()
                                .name("foo")
                                .target_name("hippo")
                                .source(ExposeSource::Self_),
                        )
                        .build(),
                ),
            ],
            Moniker::root(),
        )
        .await;
        let (_event_source, mut event_stream) = test
            .new_event_stream(vec![EventType::Resolved.into(), EventType::Started.into()])
            .await;
        let mut out_dir = OutDir::new();
        out_dir.add_echo_protocol("/svc/foo".parse().unwrap());
        test.mock_runner.add_host_fn("test:///system_resolved", out_dir.host_fn());

        // Open exposed directory of child.
        let child_ref = fdecl::ChildRef { name: "system".to_string(), collection: None };
        let (dir_proxy, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let res = test.realm_proxy.open_exposed_dir(&child_ref, server_end).await;
        res.expect("fidl call failed").expect("open_exposed_dir() failed");

        // Assert that child was resolved.
        let event =
            event_stream.wait_until(EventType::Resolved, vec!["system"].try_into().unwrap()).await;
        assert!(event.is_some());

        // Assert that event stream doesn't have any outstanding messages.
        // This ensures that EventType::Started for "system" has not been
        // registered.
        let event = event_stream
            .wait_until(EventType::Started, vec!["system"].try_into().unwrap())
            .now_or_never();
        assert!(event.is_none());

        // Check flags on directory opened. These are not exactly the flags we
        // set in `open_exposed_dir`, because fuchsia.io transforms POSIX_*
        // flags into their respective {RIGHT_READABLE, RIGHT_WRITABLE,
        // RIGHT_EXECUTABLE} variants if and only if all intermediate nodes are
        // readable, writable, or executable. Additionally, we already know
        // this is a directory, and fuchsia.io does not propagate that flag.
        let (status, flags) = dir_proxy.get_flags().await.expect("getting exposed dir flags");
        assert_matches!(zx::Status::ok(status), Ok(()));
        assert_eq!(
            flags,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::RIGHT_EXECUTABLE
        );

        // Now that it was asserted that "system:0" has yet to start,
        // assert that it starts after making connection below.
        let echo_proxy =
            client::connect_to_named_protocol_at_dir_root::<echo::EchoMarker>(&dir_proxy, "hippo")
                .expect("failed to open hippo service");
        let event =
            event_stream.wait_until(EventType::Started, vec!["system"].try_into().unwrap()).await;
        assert!(event.is_some());
        let res = echo_proxy.echo_string(Some("hippos")).await;
        assert_eq!(res.expect("failed to use echo service"), Some("hippos".to_string()));

        // Verify topology matches expectations.
        let expected_urls = &["test:///root_resolved", "test:///system_resolved"];
        test.mock_runner.wait_for_urls(expected_urls).await;
        assert_eq!("(system)", test.hook.print());
    }

    #[fuchsia::test]
    async fn open_exposed_dir_dynamic_child() {
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().collection_default("coll").build()),
                (
                    "system",
                    ComponentDeclBuilder::new()
                        .protocol_default("foo")
                        .expose(
                            ExposeBuilder::protocol()
                                .name("foo")
                                .target_name("hippo")
                                .source(ExposeSource::Self_),
                        )
                        .build(),
                ),
            ],
            Moniker::root(),
        )
        .await;

        let (_event_source, mut event_stream) = test
            .new_event_stream(vec![EventType::Resolved.into(), EventType::Started.into()])
            .await;
        let mut out_dir = OutDir::new();
        out_dir.add_echo_protocol("/svc/foo".parse().unwrap());
        test.mock_runner.add_host_fn("test:///system_resolved", out_dir.host_fn());

        // Add "system" to collection.
        let collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        let res = test
            .realm_proxy
            .create_child(
                &collection_ref,
                &child_decl("system"),
                fcomponent::CreateChildArgs::default(),
            )
            .await;
        res.expect("fidl call failed").expect("failed to create child system");

        // Open exposed directory of child.
        let child_ref =
            fdecl::ChildRef { name: "system".to_string(), collection: Some("coll".to_owned()) };
        let (dir_proxy, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let res = test.realm_proxy.open_exposed_dir(&child_ref, server_end).await;
        res.expect("fidl call failed").expect("open_exposed_dir() failed");

        // Assert that child was resolved.
        let event = event_stream
            .wait_until(EventType::Resolved, vec!["coll:system"].try_into().unwrap())
            .await;
        assert!(event.is_some());

        // Assert that event stream doesn't have any outstanding messages.
        // This ensures that EventType::Started for "system" has not been
        // registered.
        let event = event_stream
            .wait_until(EventType::Started, vec!["coll:system"].try_into().unwrap())
            .now_or_never();
        assert!(event.is_none());

        // Now that it was asserted that "system" has yet to start,
        // assert that it starts after making connection below.
        let echo_proxy =
            client::connect_to_named_protocol_at_dir_root::<echo::EchoMarker>(&dir_proxy, "hippo")
                .expect("failed to open hippo service");
        let event = event_stream
            .wait_until(EventType::Started, vec!["coll:system"].try_into().unwrap())
            .await;
        assert!(event.is_some());
        let res = echo_proxy.echo_string(Some("hippos")).await;
        assert_eq!(res.expect("failed to use echo service"), Some("hippos".to_string()));

        // Verify topology matches expectations.
        let expected_urls = &["test:///root_resolved", "test:///system_resolved"];
        test.mock_runner.wait_for_urls(expected_urls).await;
        assert_eq!("(coll:system)", test.hook.print());
    }

    #[fuchsia::test]
    async fn open_exposed_dir_errors() {
        let test = RealmCapabilityTest::new(
            vec![
                (
                    "root",
                    ComponentDeclBuilder::new()
                        .child_default("system")
                        .child_default("unresolvable")
                        .child_default("unrunnable")
                        .build(),
                ),
                ("system", component_decl_with_test_runner()),
                ("unrunnable", component_decl_with_test_runner()),
            ],
            Moniker::root(),
        )
        .await;
        test.mock_runner.cause_failure("unrunnable");

        // Instance not found.
        {
            let child_ref = fdecl::ChildRef { name: "missing".to_string(), collection: None };
            let (_, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let err = test
                .realm_proxy
                .open_exposed_dir(&child_ref, server_end)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InstanceNotFound);
        }

        // Instance cannot resolve.
        {
            let child_ref = fdecl::ChildRef { name: "unresolvable".to_string(), collection: None };
            let (_, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let err = test
                .realm_proxy
                .open_exposed_dir(&child_ref, server_end)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InstanceCannotResolve);
        }

        // Instance can't run.
        {
            let child_ref = fdecl::ChildRef { name: "unrunnable".to_string(), collection: None };
            let (dir_proxy, server_end) =
                endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let res = test.realm_proxy.open_exposed_dir(&child_ref, server_end).await;
            res.expect("fidl call failed").expect("open_exposed_dir() failed");
            let echo_proxy = client::connect_to_named_protocol_at_dir_root::<echo::EchoMarker>(
                &dir_proxy, "hippo",
            )
            .expect("failed to open hippo service");
            let res = echo_proxy.echo_string(Some("hippos")).await;
            assert!(res.is_err());
        }
    }

    #[fuchsia::test]
    async fn open_controller() {
        let test = RealmCapabilityTest::new(
            vec![
                ("root", ComponentDeclBuilder::new().child_default("system").build()),
                ("system", component_decl_with_test_runner()),
            ],
            Moniker::root(),
        )
        .await;

        // Success.
        {
            let child_ref = fdecl::ChildRef { name: "system".to_string(), collection: None };
            let (_, server_end) =
                endpoints::create_proxy::<fcomponent::ControllerMarker>().unwrap();
            test.realm_proxy
                .open_controller(&child_ref, server_end)
                .await
                .expect("fidl call failed")
                .expect("open_controller() failed");
            // Business logic tests for `fuchsia.component.Controller` is at
            // src/sys/component_manager/tests/controller/src/lib.rs
        }

        // Invalid ref.
        {
            let child_ref = fdecl::ChildRef { name: "-&*:(\\".to_string(), collection: None };
            let (_, server_end) =
                endpoints::create_proxy::<fcomponent::ControllerMarker>().unwrap();
            let err = test
                .realm_proxy
                .open_controller(&child_ref, server_end)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InvalidArguments);
        }

        // Instance not found.
        {
            let child_ref = fdecl::ChildRef { name: "missing".to_string(), collection: None };
            let (_, server_end) =
                endpoints::create_proxy::<fcomponent::ControllerMarker>().unwrap();
            let err = test
                .realm_proxy
                .open_controller(&child_ref, server_end)
                .await
                .expect("fidl call failed")
                .expect_err("unexpected success");
            assert_eq!(err, fcomponent::Error::InstanceNotFound);
        }
    }
}
