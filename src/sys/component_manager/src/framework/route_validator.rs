// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource, PERMITTED_FLAGS},
        model::{
            component::{ComponentInstance, InstanceState, ResolvedInstanceState},
            error::{CapabilityProviderError, ModelError},
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            model::Model,
            routing::{self, service::CollectionServiceRoute, Route, RouteRequest, RoutingError},
        },
    },
    async_trait::async_trait,
    cm_rust::{ExposeDecl, SourceName, UseDecl},
    cm_task_scope::TaskScope,
    cm_types::Name,
    cm_util::channel,
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    fuchsia_zircon as zx,
    futures::{future::join_all, lock::Mutex, TryStreamExt},
    lazy_static::lazy_static,
    moniker::{ExtendedMoniker, Moniker, MonikerBase},
    std::{
        cmp::Ordering,
        path::PathBuf,
        sync::{Arc, Weak},
    },
    tracing::warn,
};

lazy_static! {
    pub static ref ROUTE_VALIDATOR_CAPABILITY_NAME: Name =
        fsys::RouteValidatorMarker::PROTOCOL_NAME.parse().unwrap();
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
                        component.moniker.clone(),
                    )));
                }
            }
        }
        Ok(())
    }

    async fn validate(
        self: &Arc<Self>,
        scope_moniker: &Moniker,
        relative_moniker_str: &str,
    ) -> Result<Vec<fsys::RouteReport>, fcomponent::Error> {
        // Construct the complete moniker using the scope moniker and the relative moniker string.
        let relative_moniker = Moniker::try_from(relative_moniker_str)
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
        scope_moniker: &Moniker,
        relative_moniker_str: &str,
        targets: Vec<fsys::RouteTarget>,
    ) -> Result<Vec<fsys::RouteReport>, fsys::RouteValidatorError> {
        // Construct the complete moniker using the scope moniker and the relative moniker string.
        let relative_moniker = Moniker::try_from(relative_moniker_str)
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

        let route_requests = Self::generate_route_requests(&resolved, targets)?;
        drop(state);

        let route_futs = route_requests.into_iter().map(|pair| async {
            let (target, request) = pair;

            let capability = Some(target.name.into());
            let decl_type = Some(target.decl_type);
            match Self::route_instance(scope_moniker, request, &instance).await {
                Ok(info) => {
                    let (source_moniker, service_instances) = info;
                    fsys::RouteReport {
                        capability,
                        decl_type,
                        error: None,
                        source_moniker: Some(source_moniker),
                        service_instances,
                        ..Default::default()
                    }
                }
                Err(e) => {
                    let error = Some(fsys::RouteError {
                        summary: Some(e.to_string()),
                        ..Default::default()
                    });
                    fsys::RouteReport { capability, decl_type, error, ..Default::default() }
                }
            }
        });
        Ok(join_all(route_futs).await)
    }

    fn generate_route_requests(
        resolved: &ResolvedInstanceState,
        targets: Vec<fsys::RouteTarget>,
    ) -> Result<Vec<(fsys::RouteTarget, RouteRequest)>, fsys::RouteValidatorError> {
        if targets.is_empty() {
            let use_requests = resolved.decl().uses.iter().map(|use_| {
                let target = fsys::RouteTarget {
                    name: use_.source_name().as_str().into(),
                    decl_type: fsys::DeclType::Use,
                };
                let request = routing::request_for_namespace_capability_use(use_.clone())
                    .ok_or(fsys::RouteValidatorError::InvalidArguments)?;
                Ok((target, request))
            });

            let exposes = routing::aggregate_exposes(&resolved.decl().exposes);
            let expose_requests = exposes.into_iter().map(|(target_name, e)| {
                let target = fsys::RouteTarget {
                    name: target_name.into(),
                    decl_type: fsys::DeclType::Expose,
                };
                let request = routing::request_for_namespace_capability_expose(e)
                    .ok_or(fsys::RouteValidatorError::InvalidArguments)?;
                Ok((target, request))
            });
            use_requests.chain(expose_requests).collect()
        } else {
            // Return results that fuzzy match (substring match) `target.name`.
            let targets = targets
                .into_iter()
                .map(|target| match target.decl_type {
                    fsys::DeclType::Any => {
                        let mut use_target = target.clone();
                        use_target.decl_type = fsys::DeclType::Use;
                        let mut expose_target = target.clone();
                        expose_target.decl_type = fsys::DeclType::Expose;
                        vec![use_target, expose_target].into_iter()
                    }
                    _ => vec![target].into_iter(),
                })
                .flatten();
            targets
                .map(|target| match target.decl_type {
                    fsys::DeclType::Use => {
                        let matching_requests: Vec<_> = resolved
                            .decl()
                            .uses
                            .iter()
                            .filter_map(|u| {
                                if !u.source_name().as_str().contains(&target.name) {
                                    return None;
                                }
                                // This could be a fuzzy match so update the capability name.
                                let target = fsys::RouteTarget {
                                    name: u.source_name().to_string(),
                                    decl_type: target.decl_type,
                                };
                                let res = routing::request_for_namespace_capability_use(u.clone())
                                    .ok_or(fsys::RouteValidatorError::InvalidArguments)
                                    .map(|request| (target, request));
                                Some(res)
                            })
                            .collect();
                        matching_requests.into_iter()
                    }
                    fsys::DeclType::Expose => {
                        let exposes = routing::aggregate_exposes(&resolved.decl().exposes);
                        let matching_requests: Vec<_> = exposes
                            .into_iter()
                            .filter_map(|(target_name, e)| {
                                if !target_name.contains(&target.name) {
                                    return None;
                                }
                                // This could be a fuzzy match so update the capability name.
                                let target = fsys::RouteTarget {
                                    name: target_name.into(),
                                    decl_type: target.decl_type,
                                };
                                let res = routing::request_for_namespace_capability_expose(e)
                                    .ok_or(fsys::RouteValidatorError::InvalidArguments)
                                    .map(|request| (target, request));
                                Some(res)
                            })
                            .collect();
                        matching_requests.into_iter()
                    }
                    fsys::DeclType::Any => unreachable!("Any was expanded"),
                    fsys::DeclTypeUnknown!() => {
                        vec![Err(fsys::RouteValidatorError::InvalidArguments)].into_iter()
                    }
                })
                .flatten()
                .collect()
        }
    }

    /// Serve the fuchsia.sys2.RouteValidator protocol for a given scope on a given stream
    async fn serve(
        self: Arc<Self>,
        scope_moniker: Moniker,
        mut stream: fsys::RouteValidatorRequestStream,
    ) {
        let res: Result<(), fidl::Error> = async move {
            while let Some(request) = stream.try_next().await? {
                match request {
                    fsys::RouteValidatorRequest::Validate { moniker, responder } => {
                        let result = self.validate(&scope_moniker, &moniker).await;
                        if let Err(e) = responder.send(result.as_deref().map_err(|e| *e)) {
                            warn!(error = %e, "RouteValidator failed to send Validate response");
                        }
                    }
                    fsys::RouteValidatorRequest::Route { moniker, targets, responder } => {
                        let result = self.route(&scope_moniker, &moniker, targets).await;
                        if let Err(e) = responder.send(result.as_deref().map_err(|e| *e)) {
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
        scope_moniker: &Moniker,
        request: RouteRequest,
        target: &Arc<ComponentInstance>,
    ) -> Result<(String, Option<Vec<fsys::ServiceInstance>>), RoutingError> {
        let source = request.route(target).await?;
        let source = &source.source;
        let service_dir = match source {
            CapabilitySource::CollectionAggregate {
                capability, component, collections, ..
            } => {
                let component = component.upgrade()?;
                let route = CollectionServiceRoute {
                    source_moniker: component.moniker.clone(),
                    collections: collections.clone(),
                    service_name: capability.source_name().clone(),
                };
                let state = component.lock_state().await;
                match &*state {
                    InstanceState::Resolved(r) => r.collection_services.get(&route).cloned(),
                    _ => None,
                }
            }
            _ => None,
        };
        let moniker = Self::extended_moniker_to_str(
            scope_moniker,
            source.source_instance().extended_moniker(),
        );
        let service_info = match service_dir {
            Some(service_dir) => {
                let mut service_info: Vec<_> = service_dir.entries().await;
                // Sort the entries (they can show up in any order)
                service_info.sort_by(|a, b| match a.source_id.cmp(&b.source_id) {
                    Ordering::Equal => a.service_instance.cmp(&b.service_instance),
                    o => o,
                });
                let service_info = service_info
                    .into_iter()
                    .map(|e| {
                        let child_name = format!("{}", e.source_id);
                        fsys::ServiceInstance {
                            instance_name: Some(e.name.clone()),
                            child_name: Some(child_name),
                            child_instance_name: Some(e.service_instance.to_string()),
                            ..Default::default()
                        }
                    })
                    .collect();
                Some(service_info)
            }
            None => None,
        };
        Ok((moniker, service_info))
    }

    fn extended_moniker_to_str(scope_moniker: &Moniker, m: ExtendedMoniker) -> String {
        match m {
            ExtendedMoniker::ComponentManager => m.to_string(),
            ExtendedMoniker::ComponentInstance(m) => match Moniker::scope_down(scope_moniker, &m) {
                Ok(r) => r.to_string(),
                Err(_) => "<above scope>".to_string(),
            },
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
    scope_moniker: Moniker,
}

impl RouteValidatorCapabilityProvider {
    pub fn query(query: Arc<RouteValidator>, scope_moniker: Moniker) -> Self {
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
    ) -> Result<(), CapabilityProviderError> {
        let forbidden = flags - PERMITTED_FLAGS;
        if !forbidden.is_empty() {
            warn!(?forbidden, "RouteValidator capability");
            return Err(CapabilityProviderError::BadFlags);
        }

        if relative_path.components().count() != 0 {
            warn!(
                path=%relative_path.display(),
                "RouteValidator capability got open request with non-empty",
            );
            return Err(CapabilityProviderError::BadPath);
        }

        let server_end = channel::take_channel(server_end);

        let server_end = ServerEnd::<fsys::RouteValidatorMarker>::new(server_end);
        let stream: fsys::RouteValidatorRequestStream =
            server_end.into_stream().map_err(|_| CapabilityProviderError::StreamCreationError)?;
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
        if let Some(route_request) = routing::request_for_namespace_capability_use(use_) {
            let error = if let Err(e) = route_request.route(&instance).await {
                Some(fsys::RouteError { summary: Some(e.to_string()), ..Default::default() })
            } else {
                None
            };

            reports.push(fsys::RouteReport { capability, decl_type, error, ..Default::default() })
        }
    }
    reports
}

async fn validate_exposes(
    exposes: Vec<ExposeDecl>,
    instance: &Arc<ComponentInstance>,
) -> Vec<fsys::RouteReport> {
    let mut reports = vec![];

    let exposes = routing::aggregate_exposes(&exposes);
    for (target_name, e) in exposes {
        let capability = Some(target_name.to_string());
        let decl_type = Some(fsys::DeclType::Expose);
        if let Some(route_request) = routing::request_for_namespace_capability_expose(e) {
            let error = if let Err(e) = route_request.route(instance).await {
                Some(fsys::RouteError { summary: Some(e.to_string()), ..Default::default() })
            } else {
                None
            };

            reports.push(fsys::RouteReport { capability, decl_type, error, ..Default::default() })
        }
    }
    reports
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            component::StartReason,
            testing::{
                out_dir::OutDir,
                test_helpers::{TestEnvironmentBuilder, TestModelResult},
            },
        },
        assert_matches::assert_matches,
        cm_rust::*,
        cm_rust_testing::ComponentDeclBuilder,
        fidl::endpoints,
        fidl_fuchsia_component_decl as fdecl, fuchsia_async as fasync,
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
            source_name: "fuchsia.component.Realm".parse().unwrap(),
            target_path: "/svc/fuchsia.component.Realm".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".to_string()),
            source_name: "foo.bar".parse().unwrap(),
            target_path: "/svc/foo.bar".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".to_string()),
            source_name: "foo.bar".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "foo.bar".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let expose_from_self_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "foo.bar".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "foo.bar".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let capability_decl = ProtocolDecl {
            name: "foo.bar".parse().unwrap(),
            source_path: Some("/svc/foo.bar".parse().unwrap()),
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
            validator_server.serve(Moniker::root(), request_stream).await
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
        let mut results = validator.validate("my_child").await.unwrap().unwrap();
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
            source_name: "a".parse().unwrap(),
            target_path: "/svc/a".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("bad_child".to_string()),
            source_name: "b".parse().unwrap(),
            target_path: "/svc/b".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_name_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".to_string()),
            source_name: "c".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "c".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let invalid_source_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("bad_child".to_string()),
            source_name: "d".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "d".parse().unwrap(),
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
            validator_server.serve(Moniker::root(), validator_request_stream).await
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
            source_name: "fuchsia.component.Realm".parse().unwrap(),
            target_path: "/svc/fuchsia.component.Realm".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".into()),
            source_name: "biz.buz".parse().unwrap(),
            target_path: "/svc/foo.bar".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".into()),
            source_name: "biz.buz".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "foo.bar".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let expose_from_self_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "biz.buz".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "biz.buz".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let capability_decl = ProtocolDecl {
            name: "biz.buz".parse().unwrap(),
            source_path: Some("/svc/foo.bar".parse().unwrap()),
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
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start().await;

        // Validate the root
        let targets = &[
            fsys::RouteTarget { name: "biz.buz".parse().unwrap(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget {
                name: "foo.bar".parse().unwrap(),
                decl_type: fsys::DeclType::Expose,
            },
            fsys::RouteTarget {
                name: "fuchsia.component.Realm".parse().unwrap(),
                decl_type: fsys::DeclType::Use,
            },
        ];
        let mut results = validator.route(".", targets).await.unwrap().unwrap();

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
            } if s == "biz.buz" && m == "my_child"
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
            } if s == "foo.bar" && m == "my_child"
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
        let targets = &[fsys::RouteTarget {
            name: "biz.buz".parse().unwrap(),
            decl_type: fsys::DeclType::Expose,
        }];
        let mut results = validator.route("my_child", targets).await.unwrap().unwrap();

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
            } if s == "biz.buz" && m == "my_child"
        );
    }

    #[fuchsia::test]
    async fn route_all() {
        let use_from_framework_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Framework,
            source_name: "foo.bar".parse().unwrap(),
            target_path: "/svc/foo.bar".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".into()),
            source_name: "qax.qux".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "foo.buz".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let expose_from_self_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "qax.qux".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "qax.qux".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let capability_decl = ProtocolDecl {
            name: "qax.qux".parse().unwrap(),
            source_path: Some("/svc/qax.qux".parse().unwrap()),
        };

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
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
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start().await;

        // Validate the root, passing an empty vector. This should match both capabilities
        let mut results = validator.route(".", &[]).await.unwrap().unwrap();

        assert_eq!(results.len(), 2);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "foo.bar" && m == "."
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
            } if s == "foo.buz" && m == "my_child"
        );
    }

    #[fuchsia::test]
    async fn route_fuzzy() {
        let use_from_framework_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Framework,
            source_name: "foo.bar".parse().unwrap(),
            target_path: "/svc/foo.bar".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });
        let use_from_framework_decl2 = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Framework,
            source_name: "foo.buz".parse().unwrap(),
            target_path: "/svc/foo.buz".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });
        let use_from_framework_decl3 = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Framework,
            source_name: "no.match".parse().unwrap(),
            target_path: "/svc/no.match".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".into()),
            source_name: "qax.qux".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "foo.buz".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });
        let expose_from_child_decl2 = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".into()),
            source_name: "qax.qux".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "foo.biz".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });
        let expose_from_child_decl3 = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Framework,
            source_name: "no.match".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "no.match".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let expose_from_self_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "qax.qux".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "qax.qux".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let capability_decl = ProtocolDecl {
            name: "qax.qux".parse().unwrap(),
            source_path: Some("/svc/qax.qux".parse().unwrap()),
        };

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
                    .use_(use_from_framework_decl2)
                    .use_(use_from_framework_decl3)
                    .expose(expose_from_child_decl)
                    .expose(expose_from_child_decl2)
                    .expose(expose_from_child_decl3)
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
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start().await;

        // Validate the root
        let targets =
            &[fsys::RouteTarget { name: "foo.".parse().unwrap(), decl_type: fsys::DeclType::Any }];
        let mut results = validator.route(".", targets).await.unwrap().unwrap();

        assert_eq!(results.len(), 4);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "foo.bar" && m == "."
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
            } if s == "foo.buz" && m == "."
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
            } if s == "foo.biz" && m == "my_child"
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
            } if s == "foo.buz" && m == "my_child"
        );
    }

    #[fuchsia::test]
    async fn route_service() {
        let offer_from_collection_decl = OfferDecl::Service(OfferServiceDecl {
            source: OfferSource::Collection("coll".parse().unwrap()),
            source_name: "my_service".parse().unwrap(),
            target: OfferTarget::static_child("target".into()),
            target_name: "my_service".parse().unwrap(),
            availability: Availability::Required,
            source_instance_filter: None,
            renamed_instances: None,
        });
        let expose_from_self_decl = ExposeDecl::Service(ExposeServiceDecl {
            source: ExposeSource::Self_,
            source_name: "my_service".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "my_service".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });
        let use_decl = UseDecl::Service(UseServiceDecl {
            source: UseSource::Parent,
            source_name: "my_service".parse().unwrap(),
            target_path: "/svc/foo.bar".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });
        let capability_decl = ServiceDecl {
            name: "my_service".parse().unwrap(),
            source_path: Some("/svc/foo.bar".parse().unwrap()),
        };

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .offer(offer_from_collection_decl)
                    .add_transient_collection("coll")
                    .add_lazy_child("target")
                    .build(),
            ),
            ("target", ComponentDeclBuilder::new().use_(use_decl).build()),
            (
                "child_a",
                ComponentDeclBuilder::new()
                    .service(capability_decl.clone())
                    .expose(expose_from_self_decl.clone())
                    .build(),
            ),
            (
                "child_b",
                ComponentDeclBuilder::new()
                    .service(capability_decl.clone())
                    .expose(expose_from_self_decl.clone())
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment, realm_proxy, mock_runner, .. } =
            TestEnvironmentBuilder::new()
                .set_components(components)
                .set_realm_moniker(Moniker::root())
                .build()
                .await;
        let realm_proxy = realm_proxy.unwrap();

        let validator_server = {
            let env = builtin_environment.lock().await;
            env.route_validator.clone().unwrap()
        };
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start().await;

        // Create two children in the collection, each exposing `my_service` with two instances.
        let collection_ref = fdecl::CollectionRef { name: "coll".parse().unwrap() };
        for name in &["child_a", "child_b"] {
            realm_proxy
                .create_child(
                    &collection_ref,
                    &child_decl(name),
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .unwrap()
                .unwrap();

            let mut out_dir = OutDir::new();
            out_dir.add_echo_protocol("/svc/foo.bar/instance_a/echo".parse().unwrap());
            out_dir.add_echo_protocol("/svc/foo.bar/instance_b/echo".parse().unwrap());
            mock_runner.add_host_fn(&format!("test:///{}_resolved", name), out_dir.host_fn());

            let child = model
                .look_up(&format!("coll:{}", name).as_str().try_into().unwrap())
                .await
                .unwrap();
            child.start(&StartReason::Debug, None, vec![], vec![]).await.unwrap();
        }

        // Open the service directory from `target` so that it gets instantiated.
        {
            let target = model.look_up(&"target".try_into().unwrap()).await.unwrap();
            target.start(&StartReason::Debug, None, vec![], vec![]).await.unwrap();
            let ns = mock_runner.get_namespace("test:///target_resolved").unwrap();
            let mut ns = ns.lock().await;
            // /pkg and /svc
            assert_eq!(ns.len(), 2);
            let ns = ns.remove(1);
            assert_eq!(ns.path.as_ref().unwrap(), "/svc");
            let svc_dir = ns.directory.unwrap().into_proxy().unwrap();
            fuchsia_fs::directory::open_directory(&svc_dir, "foo.bar", fio::OpenFlags::empty())
                .await
                .unwrap();
        }

        let targets = &[fsys::RouteTarget {
            name: "my_service".parse().unwrap(),
            decl_type: fsys::DeclType::Use,
        }];
        let mut results = validator.route("target", targets).await.unwrap().unwrap();

        assert_eq!(results.len(), 1);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                service_instances: Some(_),
                error: None,
                ..
            } if s == "my_service" && m == "."
        );
        let service_instances = report.service_instances.unwrap();
        assert_eq!(service_instances.len(), 4);
        // (child_id, instance_id)
        let pairs = vec![("a", "a"), ("a", "b"), ("b", "a"), ("b", "b")];
        for (service_instance, pair) in service_instances.into_iter().zip(pairs) {
            let (child_id, instance_id) = pair;
            assert_matches!(
                service_instance,
                fsys::ServiceInstance {
                    instance_name: Some(instance_name),
                    child_name: Some(child_name),
                    child_instance_name: Some(child_instance_name),
                    ..
                } if instance_name.len() == 32 &&
                    instance_name.chars().all(|c| c.is_ascii_hexdigit()) &&
                    child_name == format!("coll:child_{}", child_id) &&
                    child_instance_name == format!("instance_{}", instance_id)
            );
        }
    }

    fn child_decl(name: &str) -> fdecl::Child {
        fdecl::Child {
            name: Some(name.to_owned()),
            url: Some(format!("test:///{}", name)),
            startup: Some(fdecl::StartupMode::Lazy),
            ..Default::default()
        }
    }

    #[fuchsia::test]
    async fn route_error() {
        let invalid_source_name_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("my_child".to_string()),
            source_name: "a".parse().unwrap(),
            target_path: "/svc/a".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_use_from_child_decl = UseDecl::Protocol(UseProtocolDecl {
            source: UseSource::Child("bad_child".to_string()),
            source_name: "b".parse().unwrap(),
            target_path: "/svc/b".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let invalid_source_name_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("my_child".to_string()),
            source_name: "c".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "c".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });

        let invalid_source_expose_from_child_decl = ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Child("bad_child".to_string()),
            source_name: "d".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "d".parse().unwrap(),
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
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start().await;

        // `my_child` should not be resolved right now
        let instance = model.find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        let targets = &[
            fsys::RouteTarget { name: "a".parse().unwrap(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget { name: "b".parse().unwrap(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget { name: "c".parse().unwrap(), decl_type: fsys::DeclType::Expose },
            fsys::RouteTarget { name: "d".parse().unwrap(), decl_type: fsys::DeclType::Expose },
        ];
        let mut results = validator.route(".", targets).await.unwrap().unwrap();
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
