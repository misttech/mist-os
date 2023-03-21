// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource, PERMITTED_FLAGS},
        model::{
            component::{ComponentInstance, InstanceState},
            error::ModelError,
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            model::Model,
            routing::{self, RouteRequest, RouteSource, RoutingError},
        },
    },
    async_trait::async_trait,
    cm_rust::{CapabilityName, ExposeDecl, ExposeDeclCommon, SourceName, UseDecl},
    cm_task_scope::TaskScope,
    cm_util::channel,
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    fuchsia_zircon as zx,
    futures::{future::join_all, lock::Mutex, TryStreamExt},
    lazy_static::lazy_static,
    moniker::{
        AbsoluteMoniker, AbsoluteMonikerBase, ExtendedMoniker, RelativeMoniker, RelativeMonikerBase,
    },
    std::{
        convert::TryFrom,
        path::PathBuf,
        sync::{Arc, Weak},
    },
    tracing::warn,
};

lazy_static! {
    pub static ref ROUTE_VALIDATOR_CAPABILITY_NAME: CapabilityName =
        fsys::RouteValidatorMarker::PROTOCOL_NAME.into();
}

/// Serves the fuchsia.sys2.RouteValidator protocol.
pub struct RouteValidator {
    model: Arc<Model>,
}

impl RouteValidator {
    pub fn new(model: Arc<Model>) -> Self {
        Self { model }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "RouteValidator",
            vec![EventType::CapabilityRouted],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    /// Given a `CapabilitySource`, determine if it is a framework-provided
    /// RouteValidator capability. If so, serve the capability.
    async fn on_capability_routed_async(
        self: Arc<Self>,
        source: CapabilitySource,
        capability_provider: Arc<Mutex<Option<Box<dyn CapabilityProvider>>>>,
    ) -> Result<(), ModelError> {
        // If this is a scoped framework directory capability, then check the source path
        if let CapabilitySource::Framework { capability, component } = source {
            if capability.matches_protocol(&ROUTE_VALIDATOR_CAPABILITY_NAME) {
                // Set the capability provider, if not already set.
                let mut capability_provider = capability_provider.lock().await;
                if capability_provider.is_none() {
                    *capability_provider = Some(Box::new(RouteValidatorCapabilityProvider::query(
                        self,
                        component.abs_moniker.clone(),
                    )));
                }
            }
        }
        Ok(())
    }

    async fn validate(
        self: &Arc<Self>,
        scope_moniker: &AbsoluteMoniker,
        relative_moniker_str: &str,
    ) -> Result<Vec<fsys::RouteReport>, fcomponent::Error> {
        // Construct the complete moniker using the scope moniker and the relative moniker string.
        let relative_moniker = RelativeMoniker::try_from(relative_moniker_str)
            .map_err(|_| fcomponent::Error::InvalidArguments)?;
        let moniker = scope_moniker.descendant(&relative_moniker);

        let instance =
            self.model.find(&moniker).await.ok_or(fcomponent::Error::InstanceNotFound)?;

        // Get all use and expose declarations for this component
        let (uses, exposes) = {
            let state = instance.lock_state().await;

            let resolved = match *state {
                InstanceState::Resolved(ref r) => r,
                // TODO(http://fxbug.dev/102026): The error is that the instance is not currently
                // resolved. Use a better error here, when one exists.
                _ => return Err(fcomponent::Error::InstanceCannotResolve),
            };

            let uses = resolved.decl().uses.clone();
            let exposes = resolved.decl().exposes.clone();

            (uses, exposes)
        };

        let mut reports = validate_uses(uses, &instance).await;
        let mut expose_reports = validate_exposes(exposes, &instance).await;
        reports.append(&mut expose_reports);

        Ok(reports)
    }

    async fn route(
        &self,
        scope_moniker: &AbsoluteMoniker,
        relative_moniker_str: &str,
        targets: Vec<fsys::RouteTarget>,
    ) -> Result<Vec<fsys::RouteReport>, fsys::RouteValidatorError> {
        // Construct the complete moniker using the scope moniker and the relative moniker string.
        let relative_moniker = RelativeMoniker::try_from(relative_moniker_str)
            .map_err(|_| fsys::RouteValidatorError::InvalidArguments)?;
        let moniker = scope_moniker.descendant(&relative_moniker);

        let instance =
            self.model.find(&moniker).await.ok_or(fsys::RouteValidatorError::InstanceNotFound)?;
        let state = instance.lock_state().await;
        let resolved = match *state {
            InstanceState::Resolved(ref r) => r,
            // TODO(fxbug.dev/102026): The error is that the instance is not currently
            // resolved. Use a better error here, when one exists.
            _ => return Err(fsys::RouteValidatorError::InstanceNotResolved),
        };

        let route_requests: Result<Vec<_>, fsys::RouteValidatorError> = targets
            .into_iter()
            .map(|target| match target.decl_type {
                fsys::DeclType::Use => {
                    let use_ = resolved
                        .decl()
                        .uses
                        .iter()
                        .find(|u| u.source_name().str() == &target.name)
                        .ok_or(fsys::RouteValidatorError::CapabilityNotFound)?;
                    let request = routing::request_for_namespace_capability_use(use_.clone())
                        .map_err(|_| fsys::RouteValidatorError::InvalidArguments)?;
                    Ok((target, request))
                }
                fsys::DeclType::Expose => {
                    let expose = resolved
                        .decl()
                        .exposes
                        .iter()
                        .find(|e| e.target_name().str() == &target.name)
                        .ok_or(fsys::RouteValidatorError::CapabilityNotFound)?;
                    let request = routing::request_for_namespace_capability_expose(expose.clone())
                        .map_err(|_| fsys::RouteValidatorError::InvalidArguments)?;
                    Ok((target, request))
                }
                fsys::DeclTypeUnknown!() => {
                    return Err(fsys::RouteValidatorError::InvalidArguments);
                }
            })
            .collect();
        let route_requests = route_requests?;
        drop(state);

        let route_futs = route_requests.into_iter().map(|pair| async {
            let (target, request) = pair;

            let capability = Some(target.name.into());
            let decl_type = Some(target.decl_type);
            match Self::route_instance(scope_moniker, request, &instance).await {
                Ok(info) => {
                    let source_moniker = info;
                    fsys::RouteReport {
                        capability,
                        decl_type,
                        error: None,
                        source_moniker: Some(source_moniker),
                        ..fsys::RouteReport::EMPTY
                    }
                }
                Err(e) => {
                    let error = Some(fsys::RouteError {
                        summary: Some(e.to_string()),
                        ..fsys::RouteError::EMPTY
                    });
                    fsys::RouteReport { capability, decl_type, error, ..fsys::RouteReport::EMPTY }
                }
            }
        });
        Ok(join_all(route_futs).await)
    }

    /// Serve the fuchsia.sys2.RouteValidator protocol for a given scope on a given stream
    async fn serve(
        self: Arc<Self>,
        scope_moniker: AbsoluteMoniker,
        mut stream: fsys::RouteValidatorRequestStream,
    ) {
        let res: Result<(), fidl::Error> = async move {
            while let Some(request) = stream.try_next().await? {
                match request {
                    fsys::RouteValidatorRequest::Validate { moniker, responder } => {
                        let mut result = self.validate(&scope_moniker, &moniker).await;
                        if let Err(e) = responder.send(&mut result) {
                            warn!(error = %e, "RouteValidator failed to send Validate response");
                        }
                    }
                    fsys::RouteValidatorRequest::Route { moniker, targets, responder } => {
                        let mut result = self.route(&scope_moniker, &moniker, targets).await;
                        if let Err(e) = responder.send(&mut result) {
                            warn!(error = %e, "RouteValidator failed to send Route response");
                        }
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = &res {
            warn!(error = %e, "RouteValidator server failed");
        }
    }

    /// Routes `request` from component `target` to the source.
    ///
    /// Returns information to populate in `fuchsia.sys2.RouteReport`.
    async fn route_instance(
        scope_moniker: &AbsoluteMoniker,
        request: RouteRequest,
        target: &Arc<ComponentInstance>,
    ) -> Result<String, RoutingError> {
        let source = routing::route(request, target).await?;
        let source = match source {
            RouteSource::Directory(s, _) => s,
            RouteSource::Event(s) => s,
            RouteSource::EventStream(s) => s,
            RouteSource::Protocol(s) => s,
            RouteSource::Resolver(s) => s,
            RouteSource::Runner(s) => s,
            RouteSource::Service(s) => s,
            RouteSource::Storage(s) => s,
            RouteSource::StorageBackingDirectory(s) => {
                let moniker = match s.storage_provider {
                    Some(s) => ExtendedMoniker::ComponentInstance(s.abs_moniker.clone()),
                    None => ExtendedMoniker::ComponentManager,
                };
                return Ok(Self::extended_moniker_to_str(scope_moniker, moniker));
            }
        };
        let moniker = Self::extended_moniker_to_str(
            scope_moniker,
            source.source_instance().extended_moniker(),
        );
        Ok(moniker)
    }

    fn extended_moniker_to_str(scope_moniker: &AbsoluteMoniker, m: ExtendedMoniker) -> String {
        match m {
            ExtendedMoniker::ComponentManager => m.to_string(),
            ExtendedMoniker::ComponentInstance(m) => {
                match RelativeMoniker::scope_down(scope_moniker, &m) {
                    Ok(r) => r.to_string(),
                    Err(_) => "<above scope>".to_string(),
                }
            }
        }
    }
}

#[async_trait]
impl Hook for RouteValidator {
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

pub struct RouteValidatorCapabilityProvider {
    query: Arc<RouteValidator>,
    scope_moniker: AbsoluteMoniker,
}

impl RouteValidatorCapabilityProvider {
    pub fn query(query: Arc<RouteValidator>, scope_moniker: AbsoluteMoniker) -> Self {
        Self { query, scope_moniker }
    }
}

#[async_trait]
impl CapabilityProvider for RouteValidatorCapabilityProvider {
    async fn open(
        self: Box<Self>,
        task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), ModelError> {
        let forbidden = flags - PERMITTED_FLAGS;
        if !forbidden.is_empty() {
            warn!(?forbidden, "RouteValidator capability");
            return Ok(());
        }

        if relative_path.components().count() != 0 {
            warn!(
                path=%relative_path.display(),
                "RouteValidator capability got open request with non-empty",
            );
            return Ok(());
        }

        let server_end = channel::take_channel(server_end);

        let server_end = ServerEnd::<fsys::RouteValidatorMarker>::new(server_end);
        let stream: fsys::RouteValidatorRequestStream =
            server_end.into_stream().map_err(ModelError::stream_creation_error)?;
        task_scope
            .add_task(async move {
                self.query.serve(self.scope_moniker, stream).await;
            })
            .await;

        Ok(())
    }
}

async fn validate_uses(
    uses: Vec<UseDecl>,
    instance: &Arc<ComponentInstance>,
) -> Vec<fsys::RouteReport> {
    let mut reports = vec![];
    for use_ in uses {
        let capability = Some(use_.source_name().to_string());
        let decl_type = Some(fsys::DeclType::Use);
        if let Ok(route_request) = routing::request_for_namespace_capability_use(use_) {
            let error = if let Err(e) = routing::route(route_request, &instance).await {
                Some(fsys::RouteError { summary: Some(e.to_string()), ..fsys::RouteError::EMPTY })
            } else {
                None
            };

            reports.push(fsys::RouteReport {
                capability,
                decl_type,
                error,
                ..fsys::RouteReport::EMPTY
            })
        }
    }
    reports
}

async fn validate_exposes(
    exposes: Vec<ExposeDecl>,
    instance: &Arc<ComponentInstance>,
) -> Vec<fsys::RouteReport> {
    let mut reports = vec![];
    for expose in exposes {
        let capability = Some(expose.target_name().to_string());
        let decl_type = Some(fsys::DeclType::Expose);
        if let Ok(route_request) = routing::request_for_namespace_capability_expose(expose) {
            let error = if let Err(e) = routing::route(route_request, instance).await {
                Some(fsys::RouteError { summary: Some(e.to_string()), ..fsys::RouteError::EMPTY })
            } else {
                None
            };

            reports.push(fsys::RouteReport {
                capability,
                decl_type,
                error,
                ..fsys::RouteReport::EMPTY
            })
        }
    }
    reports
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::testing::test_helpers::{TestEnvironmentBuilder, TestModelResult},
        assert_matches::assert_matches,
        cm_rust::*,
        cm_rust_testing::ComponentDeclBuilder,
        fidl::endpoints,
        fuchsia_async as fasync,
    };

    #[derive(Ord, PartialOrd, Eq, PartialEq)]
    struct Key {
        capability: String,
        decl_type: fsys::DeclType,
    }

    #[fuchsia::test]
    async fn validate() {
        let use_from_framework_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Framework,
            source_name: "fuchsia.component.Realm".into(),
            target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".to_string()),
            source_name: "foo.bar".into(),
            target_path: CapabilityPath::try_from("/svc/foo.bar").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".to_string()),
            source_name: "foo.bar".into(),
            target: ExposeTarget::Parent,
            target_name: "foo.bar".into(),
            availability: cm_rust::Availability::Required,
        });

        let expose_from_self_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "foo.bar".into(),
            target: ExposeTarget::Parent,
            target_name: "foo.bar".into(),
            availability: cm_rust::Availability::Required,
        });

        let capability_decl = ProtocolDecl {
            name: "foo.bar".into(),
            source_path: Some(CapabilityPath::try_from("/svc/foo.bar").unwrap()),
        };

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
                    .use_(use_from_child_decl)
                    .expose(expose_from_child_decl)
                    .add_lazy_child("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .protocol(capability_decl)
                    .expose(expose_from_self_decl)
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let validator_server = {
            let env = builtin_environment.lock().await;
            env.route_validator.clone().unwrap()
        };

        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();

        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(AbsoluteMoniker::root(), request_stream).await
        });

        model.start().await;

        // `my_child` should not be resolved right now
        let instance = model.find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        // Validate the root
        let mut results = validator.validate(".").await.unwrap().unwrap();

        results.sort_by_key(|r| Key {
            capability: r.capability.clone().unwrap(),
            decl_type: r.decl_type.clone().unwrap(),
        });

        assert_eq!(results.len(), 3);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: None,
                error: None,
                ..
            } if s == "foo.bar"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: None,
                ..
            } if s == "foo.bar"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: None,
                error: None,
                ..
            } if s == "fuchsia.component.Realm"
        );

        // This validation should have caused `my_child` to be resolved
        let instance = model.find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_some());

        // Validate `my_child`
        let mut results = validator.validate("./my_child").await.unwrap().unwrap();
        assert_eq!(results.len(), 1);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: None,
                ..
            } if s == "foo.bar"
        );
    }

    #[fuchsia::test]
    async fn validate_error() {
        let invalid_source_name_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".to_string()),
            source_name: "a".into(),
            target_path: CapabilityPath::try_from("/svc/a").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("bad_child".to_string()),
            source_name: "b".into(),
            target_path: CapabilityPath::try_from("/svc/b").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_name_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".to_string()),
            source_name: "c".into(),
            target: ExposeTarget::Parent,
            target_name: "c".into(),
            availability: cm_rust::Availability::Required,
        });

        let invalid_source_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("bad_child".to_string()),
            source_name: "d".into(),
            target: ExposeTarget::Parent,
            target_name: "d".into(),
            availability: cm_rust::Availability::Required,
        });

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(invalid_source_name_use_from_child_decl)
                    .use_(invalid_source_use_from_child_decl)
                    .expose(invalid_source_name_expose_from_child_decl)
                    .expose(invalid_source_expose_from_child_decl)
                    .add_lazy_child("my_child")
                    .build(),
            ),
            ("my_child", ComponentDeclBuilder::new().build()),
        ];

        let TestModelResult { model, builtin_environment, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let validator_server = {
            let env = builtin_environment.lock().await;
            env.route_validator.clone().unwrap()
        };

        let (validator, validator_request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();

        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(AbsoluteMoniker::root(), validator_request_stream).await
        });

        model.start().await;

        // `my_child` should not be resolved right now
        let instance = model.find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        // Validate the root
        let mut results = validator.validate(".").await.unwrap().unwrap();
        assert_eq!(results.len(), 4);

        results.sort_by_key(|r| Key {
            capability: r.capability.clone().unwrap(),
            decl_type: r.decl_type.clone().unwrap(),
        });

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "a"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "b"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "c"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "d"
        );

        // This validation should have caused `my_child` to be resolved
        let instance = model.find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_some());
    }

    #[fuchsia::test]
    async fn route() {
        let use_from_framework_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Framework,
            source_name: "fuchsia.component.Realm".into(),
            target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".into()),
            source_name: "biz.buz".into(),
            target_path: CapabilityPath::try_from("/svc/foo.bar").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".into()),
            source_name: "biz.buz".into(),
            target: ExposeTarget::Parent,
            target_name: "foo.bar".into(),
            availability: cm_rust::Availability::Required,
        });

        let expose_from_self_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "biz.buz".into(),
            target: ExposeTarget::Parent,
            target_name: "biz.buz".into(),
            availability: cm_rust::Availability::Required,
        });

        let capability_decl = ProtocolDecl {
            name: "biz.buz".into(),
            source_path: Some(CapabilityPath::try_from("/svc/foo.bar").unwrap()),
        };

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
                    .use_(use_from_child_decl)
                    .expose(expose_from_child_decl)
                    .add_lazy_child("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .protocol(capability_decl)
                    .expose(expose_from_self_decl)
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let validator_server = {
            let env = builtin_environment.lock().await;
            env.route_validator.clone().unwrap()
        };
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(AbsoluteMoniker::root(), request_stream).await
        });

        model.start().await;

        // Validate the root
        let mut targets = vec![
            fsys::RouteTarget { name: "biz.buz".into(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget { name: "foo.bar".into(), decl_type: fsys::DeclType::Expose },
            fsys::RouteTarget {
                name: "fuchsia.component.Realm".into(),
                decl_type: fsys::DeclType::Use,
            },
        ];
        let mut results = validator.route(".", &mut targets.iter_mut()).await.unwrap().unwrap();

        assert_eq!(results.len(), 3);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "biz.buz" && m == "./my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "foo.bar" && m == "./my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.component.Realm" && m == "."
        );

        // Validate `my_child`
        let mut targets =
            vec![fsys::RouteTarget { name: "biz.buz".into(), decl_type: fsys::DeclType::Expose }];
        let mut results =
            validator.route("./my_child", &mut targets.iter_mut()).await.unwrap().unwrap();

        assert_eq!(results.len(), 1);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "biz.buz" && m == "./my_child"
        );
    }

    #[fuchsia::test]
    async fn route_error() {
        let invalid_source_name_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".to_string()),
            source_name: "a".into(),
            target_path: CapabilityPath::try_from("/svc/a").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("bad_child".to_string()),
            source_name: "b".into(),
            target_path: CapabilityPath::try_from("/svc/b").unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_name_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".to_string()),
            source_name: "c".into(),
            target: ExposeTarget::Parent,
            target_name: "c".into(),
            availability: cm_rust::Availability::Required,
        });

        let invalid_source_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("bad_child".to_string()),
            source_name: "d".into(),
            target: ExposeTarget::Parent,
            target_name: "d".into(),
            availability: cm_rust::Availability::Required,
        });

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(invalid_source_name_use_from_child_decl)
                    .use_(invalid_source_use_from_child_decl)
                    .expose(invalid_source_name_expose_from_child_decl)
                    .expose(invalid_source_expose_from_child_decl)
                    .add_lazy_child("my_child")
                    .build(),
            ),
            ("my_child", ComponentDeclBuilder::new().build()),
        ];

        let TestModelResult { model, builtin_environment, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let validator_server = {
            let env = builtin_environment.lock().await;
            env.route_validator.clone().unwrap()
        };
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(AbsoluteMoniker::root(), request_stream).await
        });

        model.start().await;

        // `my_child` should not be resolved right now
        let instance = model.find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        let mut targets = vec![
            fsys::RouteTarget { name: "a".into(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget { name: "b".into(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget { name: "c".into(), decl_type: fsys::DeclType::Expose },
            fsys::RouteTarget { name: "d".into(), decl_type: fsys::DeclType::Expose },
        ];
        let mut results = validator.route(".", &mut targets.iter_mut()).await.unwrap().unwrap();
        assert_eq!(results.len(), 4);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "a"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "b"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "c"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "d"
        );
    }
}
