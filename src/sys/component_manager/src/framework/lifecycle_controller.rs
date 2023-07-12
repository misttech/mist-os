// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::capability::{CapabilityProvider, CapabilitySource, PERMITTED_FLAGS},
    crate::model::{
        actions::{ActionSet, StopAction},
        component::StartReason,
        error::{CapabilityProviderError, ModelError},
        hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
        model::Model,
    },
    async_trait::async_trait,
    cm_rust::FidlIntoNative,
    cm_task_scope::TaskScope,
    cm_types::Name,
    cm_util::channel,
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx,
    futures::lock::Mutex,
    futures::prelude::*,
    lazy_static::lazy_static,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMoniker, MonikerError},
    std::convert::TryFrom,
    std::path::PathBuf,
    std::sync::{Arc, Weak},
    tracing::warn,
};

lazy_static! {
    pub static ref LIFECYCLE_CONTROLLER_CAPABILITY_NAME: Name =
        fsys::LifecycleControllerMarker::PROTOCOL_NAME.parse().unwrap();
}

#[derive(Clone)]
pub struct LifecycleController {
    model: Arc<Model>,
}

impl LifecycleController {
    pub fn new(model: Arc<Model>) -> Self {
        Self { model }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "LifecycleController",
            vec![EventType::CapabilityRouted],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    async fn resolve_instance(
        &self,
        scope_moniker: &AbsoluteMoniker,
        moniker: String,
    ) -> Result<(), fsys::ResolveError> {
        let moniker =
            join_monikers(scope_moniker, &moniker).map_err(|_| fsys::ResolveError::BadMoniker)?;
        let instance =
            self.model.find(&moniker).await.ok_or(fsys::ResolveError::InstanceNotFound)?;
        instance.resolve().await.map(|_| ()).map_err(|error| {
            warn!(%moniker, %error, "failed to resolve instance");
            error.into()
        })
    }

    async fn start_instance(
        &self,
        scope_moniker: &AbsoluteMoniker,
        moniker: String,
        binder: ServerEnd<fcomponent::BinderMarker>,
    ) -> Result<(), fsys::StartError> {
        let moniker =
            join_monikers(scope_moniker, &moniker).map_err(|_| fsys::StartError::BadMoniker)?;
        let instance = self.model.find(&moniker).await.ok_or(fsys::StartError::InstanceNotFound)?;
        instance.start(&StartReason::Debug, None, vec![], vec![]).await.map(|_| ()).map_err(
            |error| {
                warn!(%moniker, %error, "failed to start instance");
                error.into()
            },
        )?;
        instance.scope_to_runtime(binder.into_channel()).await;
        Ok(())
    }

    async fn stop_instance(
        &self,
        scope_moniker: &AbsoluteMoniker,
        moniker: String,
    ) -> Result<(), fsys::StopError> {
        let moniker =
            join_monikers(scope_moniker, &moniker).map_err(|_| fsys::StopError::BadMoniker)?;
        let instance = self.model.find(&moniker).await.ok_or(fsys::StopError::InstanceNotFound)?;
        ActionSet::register(instance.clone(), StopAction::new(false)).await.map_err(|error| {
            warn!(%moniker, %error, "failed to stop instance");
            error.into()
        })?;
        Ok(())
    }

    async fn unresolve_instance(
        &self,
        scope_moniker: &AbsoluteMoniker,
        moniker: String,
    ) -> Result<(), fsys::UnresolveError> {
        let moniker =
            join_monikers(scope_moniker, &moniker).map_err(|_| fsys::UnresolveError::BadMoniker)?;
        let component =
            self.model.find(&moniker).await.ok_or(fsys::UnresolveError::InstanceNotFound)?;
        component.unresolve().await.map_err(|error| {
            warn!(%moniker, %error, "failed to unresolve instance");
            error.into()
        })?;
        Ok(())
    }

    async fn create_instance(
        &self,
        scope_moniker: &AbsoluteMoniker,
        parent_moniker: String,
        collection: fdecl::CollectionRef,
        child_decl: fdecl::Child,
        child_args: fcomponent::CreateChildArgs,
    ) -> Result<(), fsys::CreateError> {
        let parent_moniker = join_monikers(scope_moniker, &parent_moniker)
            .map_err(|_| fsys::CreateError::BadMoniker)?;
        let parent_component = self.model.look_up(&parent_moniker).await.map_err(|e| match e {
            ModelError::CollectionNotFound { name: _ } => fsys::CreateError::CollectionNotFound,
            ModelError::PathIsNotUtf8 { path: _ }
            | ModelError::UnexpectedComponentManagerMoniker
            | ModelError::ComponentInstanceError { err: _ } => fsys::CreateError::InstanceNotFound,
            ModelError::MonikerError { err: _ } => fsys::CreateError::BadMoniker,
            _ => fsys::CreateError::Internal,
        })?;

        cm_fidl_validator::validate_dynamic_child(&child_decl)
            .map_err(|error| {
                warn!(%parent_moniker, %error, "failed to create dynamic child. child decl validation failed");
                fsys::CreateError::BadChildDecl
            })?;
        if child_decl.environment.is_some() {
            warn!(%parent_moniker, "failed to create dynamic child. child decl cannot specify environment");
            return Err(fsys::CreateError::BadChildDecl);
        }
        let child_decl = child_decl.fidl_into_native();

        parent_component
            .add_dynamic_child(collection.name.clone(), &child_decl, child_args, false)
            .await
            .map(|_| ())
            .map_err(|error| {
                warn!(%parent_moniker, %error, "failed to add dynamic child");
                error.into()
            })
    }

    async fn destroy_instance(
        &self,
        scope_moniker: &AbsoluteMoniker,
        parent_moniker: String,
        child: fdecl::ChildRef,
    ) -> Result<(), fsys::DestroyError> {
        let parent_moniker = join_monikers(scope_moniker, &parent_moniker)
            .map_err(|_| fsys::DestroyError::BadMoniker)?;
        let parent_component =
            self.model.find(&parent_moniker).await.ok_or(fsys::DestroyError::InstanceNotFound)?;

        child.collection.as_ref().ok_or(fsys::DestroyError::BadChildRef)?;
        let child_moniker = ChildMoniker::try_new(&child.name, child.collection.as_ref())
            .map_err(|_| fsys::DestroyError::BadChildRef)?;

        parent_component.remove_dynamic_child(&child_moniker).await.map_err(|error| {
            warn!(%parent_moniker, %error, "failed to destroy dynamic child");
            error.into()
        })
    }

    pub async fn serve(
        &self,
        scope_moniker: AbsoluteMoniker,
        mut stream: fsys::LifecycleControllerRequestStream,
    ) {
        while let Ok(Some(operation)) = stream.try_next().await {
            match operation {
                fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                    let res = self.resolve_instance(&scope_moniker, moniker).await;
                    responder.send(res).unwrap_or_else(
                        |error| warn!(%error, "LifecycleController.ResolveInstance failed to send"),
                    );
                }
                fsys::LifecycleControllerRequest::UnresolveInstance { moniker, responder } => {
                    let res = self.unresolve_instance(&scope_moniker, moniker).await;
                    responder.send(res).unwrap_or_else(
                        |error| warn!(%error, "LifecycleController.UnresolveInstance failed to send"),
                    );
                }
                fsys::LifecycleControllerRequest::StartInstance { moniker, binder, responder } => {
                    let res = self.start_instance(&scope_moniker, moniker, binder).await;
                    responder.send(res).unwrap_or_else(
                        |error| warn!(%error, "LifecycleController.StartInstance failed to send"),
                    );
                }
                fsys::LifecycleControllerRequest::StopInstance { moniker, responder } => {
                    let res = self.stop_instance(&scope_moniker, moniker).await;
                    responder.send(res).unwrap_or_else(
                        |error| warn!(%error, "LifecycleController.StopInstance failed to send"),
                    );
                }
                fsys::LifecycleControllerRequest::CreateInstance {
                    parent_moniker,
                    collection,
                    decl,
                    args,
                    responder,
                } => {
                    let res = self
                        .create_instance(&scope_moniker, parent_moniker, collection, decl, args)
                        .await;
                    responder.send(res).unwrap_or_else(
                        |error| warn!(%error, "LifecycleController.CreateInstance failed to send"),
                    );
                }
                fsys::LifecycleControllerRequest::DestroyInstance {
                    parent_moniker,
                    child,
                    responder,
                } => {
                    let res = self.destroy_instance(&scope_moniker, parent_moniker, child).await;
                    responder.send(res).unwrap_or_else(
                        |error| warn!(%error, "LifecycleController.DestroyInstance failed to send"),
                    );
                }
            }
        }
    }

    /// Given a `CapabilitySource`, determine if it is a framework-provided
    /// LifecycleController capability. If so, serve the capability.
    async fn on_capability_routed_async(
        self: Arc<Self>,
        source: CapabilitySource,
        capability_provider: Arc<Mutex<Option<Box<dyn CapabilityProvider>>>>,
    ) -> Result<(), ModelError> {
        // If this is a scoped framework directory capability, then check the source path
        if let CapabilitySource::Framework { capability, component } = source {
            if capability.matches_protocol(&LIFECYCLE_CONTROLLER_CAPABILITY_NAME) {
                // Set the capability provider, if not already set.
                let mut capability_provider = capability_provider.lock().await;
                if capability_provider.is_none() {
                    *capability_provider =
                        Some(Box::new(LifecycleControllerCapabilityProvider::new(
                            self,
                            component.abs_moniker.clone(),
                        )));
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Hook for LifecycleController {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        match &event.payload {
            EventPayload::CapabilityRouted { source, capability_provider } => {
                self.on_capability_routed_async(source.clone(), capability_provider.clone())
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

pub struct LifecycleControllerCapabilityProvider {
    control: Arc<LifecycleController>,
    scope_moniker: AbsoluteMoniker,
}

impl LifecycleControllerCapabilityProvider {
    pub fn new(control: Arc<LifecycleController>, scope_moniker: AbsoluteMoniker) -> Self {
        Self { control, scope_moniker }
    }
}

#[async_trait]
impl CapabilityProvider for LifecycleControllerCapabilityProvider {
    async fn open(
        self: Box<Self>,
        task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let forbidden = flags - PERMITTED_FLAGS;
        if !forbidden.is_empty() {
            warn!(?forbidden, "LifecycleController capability");
            return Err(CapabilityProviderError::BadFlags);
        }

        if relative_path.components().count() != 0 {
            warn!(
                path=%relative_path.display(),
                "LifecycleController capability got open request with non-empty",
            );
            return Err(CapabilityProviderError::BadPath);
        }

        let server_end = channel::take_channel(server_end);

        let server_end = ServerEnd::<fsys::LifecycleControllerMarker>::new(server_end);
        let stream: fsys::LifecycleControllerRequestStream =
            server_end.into_stream().map_err(|_| CapabilityProviderError::StreamCreationError)?;
        task_scope
            .add_task(async move {
                self.control.serve(self.scope_moniker, stream).await;
            })
            .await;

        Ok(())
    }
}

/// Takes the scoped component's moniker and a relative moniker string and joins them into an
/// absolute moniker.
fn join_monikers(
    scope_moniker: &AbsoluteMoniker,
    moniker_str: &str,
) -> Result<AbsoluteMoniker, MonikerError> {
    let relative_moniker = AbsoluteMoniker::try_from(moniker_str)?;
    let abs_moniker = scope_moniker.descendant(&relative_moniker);
    Ok(abs_moniker)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            actions::test_utils::{is_discovered, is_resolved},
            testing::test_helpers::TestEnvironmentBuilder,
        },
        cm_rust_testing::{CollectionDeclBuilder, ComponentDeclBuilder},
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
        fidl_fuchsia_component_decl::{ChildRef, CollectionRef},
        fuchsia_async as fasync,
    };

    #[fuchsia::test]
    async fn lifecycle_controller_test() {
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .add_child(cm_rust::ChildDecl {
                        name: "a".to_string(),
                        url: "test:///a".to_string(),
                        startup: fdecl::StartupMode::Eager,
                        environment: None,
                        on_terminate: None,
                        config_overrides: None,
                    })
                    .add_child(cm_rust::ChildDecl {
                        name: "cant-resolve".to_string(),
                        url: "cant-resolve://cant-resolve".to_string(),
                        startup: fdecl::StartupMode::Eager,
                        environment: None,
                        on_terminate: None,
                        config_overrides: None,
                    })
                    .build(),
            ),
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(cm_rust::ChildDecl {
                        name: "b".to_string(),
                        url: "test:///b".to_string(),
                        startup: fdecl::StartupMode::Eager,
                        environment: None,
                        on_terminate: None,
                        config_overrides: None,
                    })
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().build()),
        ];

        let test_model_result =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let lifecycle_controller = {
            let env = test_model_result.builtin_environment.lock().await;
            env.lifecycle_controller.clone().unwrap()
        };

        let (lifecycle_proxy, lifecycle_request_stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();

        // async move {} is used here because we want this to own the lifecycle_controller
        let _lifecycle_server_task = fasync::Task::local(async move {
            lifecycle_controller.serve(AbsoluteMoniker::root(), lifecycle_request_stream).await
        });

        assert_eq!(lifecycle_proxy.resolve_instance(".").await.unwrap(), Ok(()));

        assert_eq!(lifecycle_proxy.resolve_instance("./a").await.unwrap(), Ok(()));

        assert_eq!(
            lifecycle_proxy.resolve_instance(".\\scope-escape-attempt").await.unwrap(),
            Err(fsys::ResolveError::BadMoniker)
        );

        assert_eq!(
            lifecycle_proxy.resolve_instance("./doesnt-exist").await.unwrap(),
            Err(fsys::ResolveError::InstanceNotFound)
        );

        assert_eq!(
            lifecycle_proxy.resolve_instance("./cant-resolve").await.unwrap(),
            Err(fsys::ResolveError::Internal)
        );
    }

    #[fuchsia::test]
    async fn lifecycle_controller_unresolve_component_test() {
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .add_child(cm_rust::ChildDecl {
                        name: "a".to_string(),
                        url: "test:///a".to_string(),
                        startup: fdecl::StartupMode::Eager,
                        environment: None,
                        on_terminate: None,
                        config_overrides: None,
                    })
                    .build(),
            ),
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(cm_rust::ChildDecl {
                        name: "b".to_string(),
                        url: "test:///b".to_string(),
                        startup: fdecl::StartupMode::Eager,
                        environment: None,
                        on_terminate: None,
                        config_overrides: None,
                    })
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().build()),
        ];

        let test_model_result =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let lifecycle_controller = {
            let env = test_model_result.builtin_environment.lock().await;
            env.lifecycle_controller.clone().unwrap()
        };

        let (lifecycle_proxy, lifecycle_request_stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();

        // async move {} is used here because we want this to own the lifecycle_controller
        let _lifecycle_server_task = fasync::Task::local(async move {
            lifecycle_controller.serve(AbsoluteMoniker::root(), lifecycle_request_stream).await
        });

        lifecycle_proxy.resolve_instance(".").await.unwrap().unwrap();
        let component_a =
            test_model_result.model.look_up(&vec!["a"].try_into().unwrap()).await.unwrap();
        let component_b =
            test_model_result.model.look_up(&vec!["a", "b"].try_into().unwrap()).await.unwrap();
        assert!(is_resolved(&component_a).await);
        assert!(is_resolved(&component_b).await);

        lifecycle_proxy.unresolve_instance(".").await.unwrap().unwrap();
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);

        assert_eq!(
            lifecycle_proxy.unresolve_instance("./nonesuch").await.unwrap(),
            Err(fsys::UnresolveError::InstanceNotFound)
        );

        // Unresolve again, which is ok because UnresolveAction is idempotent.
        assert_eq!(lifecycle_proxy.unresolve_instance(".").await.unwrap(), Ok(()));
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);
    }

    #[fuchsia::test]
    async fn lifecycle_create_and_destroy_test() {
        let collection = CollectionDeclBuilder::new_transient_collection("coll").build();
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .add_collection(collection)
                    .add_lazy_child("child")
                    .build(),
            ),
            ("child", ComponentDeclBuilder::new().build()),
        ];

        let test_model_result =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let lifecycle_controller = {
            let env = test_model_result.builtin_environment.lock().await;
            env.lifecycle_controller.clone().unwrap()
        };

        let (lifecycle_proxy, lifecycle_request_stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();

        // async move {} is used here because we want this to own the lifecycle_controller
        let _lifecycle_server_task = fasync::Task::local(async move {
            lifecycle_controller.serve(AbsoluteMoniker::root(), lifecycle_request_stream).await
        });

        assert_eq!(
            lifecycle_proxy
                .create_instance(
                    "./",
                    &CollectionRef { name: "coll".to_string() },
                    &fdecl::Child {
                        name: Some("child".to_string()),
                        url: Some("test:///child".to_string()),
                        startup: Some(fdecl::StartupMode::Lazy),
                        environment: None,
                        on_terminate: None,
                        ..Default::default()
                    },
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap(),
            Ok(())
        );

        assert_eq!(lifecycle_proxy.resolve_instance("./coll:child").await.unwrap(), Ok(()));

        assert_eq!(
            lifecycle_proxy
                .destroy_instance(
                    "./",
                    &ChildRef { name: "child".to_string(), collection: Some("coll".to_string()) }
                )
                .await
                .unwrap(),
            Ok(())
        );

        assert_eq!(
            lifecycle_proxy.resolve_instance("./coll:child").await.unwrap(),
            Err(fsys::ResolveError::InstanceNotFound)
        );
    }

    #[fuchsia::test]
    async fn lifecycle_create_fail_test() {
        let collection = CollectionDeclBuilder::new_transient_collection("coll").build();
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .add_collection(collection)
                    .add_lazy_child("child")
                    .build(),
            ),
            ("child", ComponentDeclBuilder::new().build()),
        ];

        let test_model_result =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let lifecycle_controller = {
            let env = test_model_result.builtin_environment.lock().await;
            env.lifecycle_controller.clone().unwrap()
        };

        let (lifecycle_proxy, lifecycle_request_stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();

        // async move {} is used here because we want this to own the lifecycle_controller
        let _lifecycle_server_task = fasync::Task::local(async move {
            lifecycle_controller.serve(AbsoluteMoniker::root(), lifecycle_request_stream).await
        });

        assert_eq!(
            lifecycle_proxy
                .create_instance(
                    "&#^$%",
                    &CollectionRef { name: "coll".to_string() },
                    &fdecl::Child {
                        name: Some("child".to_string()),
                        url: Some("test:///child".to_string()),
                        startup: Some(fdecl::StartupMode::Lazy),
                        environment: None,
                        on_terminate: None,
                        ..Default::default()
                    },
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap(),
            Err(fsys::CreateError::BadMoniker)
        );

        assert_eq!(
            lifecycle_proxy
                .create_instance(
                    "./does_not_exist",
                    &CollectionRef { name: "coll".to_string() },
                    &fdecl::Child {
                        name: Some("child".to_string()),
                        url: Some("test:///child".to_string()),
                        startup: Some(fdecl::StartupMode::Lazy),
                        environment: None,
                        on_terminate: None,
                        ..Default::default()
                    },
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap(),
            Err(fsys::CreateError::InstanceNotFound)
        );

        assert_eq!(
            lifecycle_proxy
                .create_instance(
                    "./",
                    &CollectionRef { name: "not_coll".to_string() },
                    &fdecl::Child {
                        name: Some("child".to_string()),
                        url: Some("test:///child".to_string()),
                        startup: Some(fdecl::StartupMode::Lazy),
                        environment: None,
                        on_terminate: None,
                        ..Default::default()
                    },
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap(),
            Err(fsys::CreateError::CollectionNotFound)
        );

        assert_eq!(
            lifecycle_proxy
                .create_instance(
                    "./",
                    &CollectionRef { name: "coll".to_string() },
                    &fdecl::Child {
                        name: Some("&*^%&@#$".to_string()),
                        url: Some("test:///child".to_string()),
                        startup: Some(fdecl::StartupMode::Lazy),
                        environment: None,
                        on_terminate: None,
                        ..Default::default()
                    },
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap(),
            Err(fsys::CreateError::BadChildDecl)
        );
    }
}
