// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{
            CapabilityProvider, CapabilitySource, FrameworkCapability, InternalCapabilityProvider,
        },
        model::{
            component::instance::ResolvedInstanceState,
            component::{ComponentInstance, WeakComponentInstance},
            model::Model,
            routing::{self, service::AnonymizedServiceRoute, Route, RouteRequest, RoutingError},
        },
    },
    ::routing::capability_source::InternalCapability,
    async_trait::async_trait,
    cm_rust::{ExposeDecl, SourceName, UseDecl},
    cm_types::Name,
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx,
    futures::{future::join_all, TryStreamExt},
    lazy_static::lazy_static,
    moniker::{ExtendedMoniker, Moniker, MonikerBase},
    std::{
        cmp::Ordering,
        sync::{Arc, Weak},
    },
    tracing::warn,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fsys::RouteValidatorMarker::PROTOCOL_NAME.parse().unwrap();
}

/// Serves the fuchsia.sys2.RouteValidator protocol.
pub struct RouteValidator {
    model: Weak<Model>,
}

impl RouteValidator {
    fn new(model: Weak<Model>) -> Arc<Self> {
        Arc::new(Self { model })
    }

    async fn validate(
        model: &Model,
        scope_moniker: &Moniker,
        moniker_str: &str,
    ) -> Result<Vec<fsys::RouteReport>, fcomponent::Error> {
        // Construct the complete moniker using the scope moniker and the moniker string.
        let moniker =
            Moniker::try_from(moniker_str).map_err(|_| fcomponent::Error::InvalidArguments)?;
        let moniker = scope_moniker.concat(&moniker);

        let instance =
            model.root().find(&moniker).await.ok_or(fcomponent::Error::InstanceNotFound)?;

        // Get all use and expose declarations for this component
        let (uses, exposes) = {
            let state = instance.lock_state().await;

            // TODO(https://fxbug.dev/42052917): The error is that the instance is not currently
            // resolved. Use a better error here, when one exists.
            let resolved =
                state.get_resolved_state().ok_or(fcomponent::Error::InstanceCannotResolve)?;

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
        model: &Model,
        scope_moniker: &Moniker,
        moniker_str: &str,
        targets: Vec<fsys::RouteTarget>,
    ) -> Result<Vec<fsys::RouteReport>, fsys::RouteValidatorError> {
        // Construct the complete moniker using the scope moniker and the moniker string.

        let moniker = Moniker::try_from(moniker_str)
            .map_err(|_| fsys::RouteValidatorError::InvalidArguments)?;
        let moniker = scope_moniker.concat(&moniker);

        let instance = model
            .root()
            .find_and_maybe_resolve(&moniker)
            .await
            .map_err(|_| fsys::RouteValidatorError::InstanceNotFound)?;
        let state = instance.lock_state().await;
        // TODO(https://fxbug.dev/42052917): The error is that the instance is not currently
        // resolved. Use a better error here, when one exists.
        let resolved =
            state.get_resolved_state().ok_or(fsys::RouteValidatorError::InstanceNotResolved)?;

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
                let request = use_.clone().into();
                (target, request)
            });

            let exposes = routing::aggregate_exposes(resolved.decl().exposes.iter());
            let expose_requests = exposes.into_iter().map(|(target_name, e)| {
                let target = fsys::RouteTarget {
                    name: target_name.to_string(),
                    decl_type: fsys::DeclType::Expose,
                };
                let request = e.into();
                (target, request)
            });
            Ok(use_requests.chain(expose_requests).collect())
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
                                let request = u.clone().into();
                                Some(Ok((target, request)))
                            })
                            .collect();
                        matching_requests.into_iter()
                    }
                    fsys::DeclType::Expose => {
                        let exposes = routing::aggregate_exposes(resolved.decl().exposes.iter());
                        let matching_requests: Vec<_> = exposes
                            .into_iter()
                            .filter_map(|(target_name, e)| {
                                if !target_name.as_str().contains(&target.name) {
                                    return None;
                                }
                                // This could be a fuzzy match so update the capability name.
                                let target = fsys::RouteTarget {
                                    name: target_name.to_string(),
                                    decl_type: target.decl_type,
                                };
                                let request = e.into();
                                Some(Ok((target, request)))
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
                let Some(model) = self.model.upgrade() else {
                    return Ok(());
                };
                match request {
                    fsys::RouteValidatorRequest::Validate { moniker, responder } => {
                        let result = Self::validate(&model, &scope_moniker, &moniker).await;
                        if let Err(e) = responder.send(result.as_deref().map_err(|e| *e)) {
                            warn!(error = %e, "RouteValidator failed to send Validate response");
                        }
                    }
                    fsys::RouteValidatorRequest::Route { moniker, targets, responder } => {
                        let result = Self::route(&model, &scope_moniker, &moniker, targets).await;
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
            CapabilitySource::AnonymizedAggregate { capability, component, members, .. } => {
                let component = component.upgrade()?;
                let route = AnonymizedServiceRoute {
                    source_moniker: component.moniker.clone(),
                    members: members.clone(),
                    service_name: capability.source_name().clone(),
                };
                let state = component.lock_state().await;
                state.get_resolved_state().and_then(|r| r.anonymized_services.get(&route).cloned())
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
            ExtendedMoniker::ComponentInstance(m) => match m.strip_prefix(scope_moniker) {
                Ok(r) => r.to_string(),
                Err(_) => "<above scope>".to_string(),
            },
        }
    }
}

pub struct RouteValidatorFrameworkCapability {
    host: Arc<RouteValidator>,
}

impl RouteValidatorFrameworkCapability {
    pub fn new(model: Weak<Model>) -> Self {
        Self { host: RouteValidator::new(model) }
    }
}

impl FrameworkCapability for RouteValidatorFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(RouteValidatorCapabilityProvider::new(self.host.clone(), scope.moniker.clone()))
    }
}

pub struct RouteValidatorCapabilityProvider {
    query: Arc<RouteValidator>,
    scope_moniker: Moniker,
}

impl RouteValidatorCapabilityProvider {
    fn new(query: Arc<RouteValidator>, scope_moniker: Moniker) -> Self {
        Self { query, scope_moniker }
    }
}

#[async_trait]
impl InternalCapabilityProvider for RouteValidatorCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsys::RouteValidatorMarker>::new(server_end);
        self.query.serve(self.scope_moniker, server_end.into_stream().unwrap()).await;
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
        let route_request = RouteRequest::from(use_);
        let error = if let Err(e) = route_request.route(&instance).await {
            Some(fsys::RouteError { summary: Some(e.to_string()), ..Default::default() })
        } else {
            None
        };

        reports.push(fsys::RouteReport { capability, decl_type, error, ..Default::default() })
    }
    reports
}

async fn validate_exposes(
    exposes: Vec<ExposeDecl>,
    instance: &Arc<ComponentInstance>,
) -> Vec<fsys::RouteReport> {
    let mut reports = vec![];

    let exposes = routing::aggregate_exposes(exposes.iter());
    for (target_name, e) in exposes {
        let capability = Some(target_name.to_string());
        let decl_type = Some(fsys::DeclType::Expose);
        let route_request = RouteRequest::from(e);
        let error = if let Err(e) = route_request.route(instance).await {
            Some(fsys::RouteError { summary: Some(e.to_string()), ..Default::default() })
        } else {
            None
        };

        reports.push(fsys::RouteReport { capability, decl_type, error, ..Default::default() })
    }
    reports
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            component::StartReason,
            start::Start,
            structured_dict::ComponentInput,
            testing::{
                out_dir::OutDir,
                test_helpers::{TestEnvironmentBuilder, TestModelResult},
            },
        },
        assert_matches::assert_matches,
        cm_rust::*,
        cm_rust_testing::*,
        fidl::endpoints,
        fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    };

    #[derive(Ord, PartialOrd, Eq, PartialEq)]
    struct Key {
        capability: String,
        decl_type: fsys::DeclType,
    }

    #[fuchsia::test]
    async fn validate() {
        let use_from_framework_decl = UseBuilder::protocol()
            .source(UseSource::Framework)
            .name("fuchsia.component.Realm")
            .build();
        let use_from_child_decl =
            UseBuilder::protocol().source_static_child("my_child").name("foo.bar").build();
        let expose_from_child_decl =
            ExposeBuilder::protocol().name("foo.bar").source_static_child("my_child").build();
        let expose_from_self_decl =
            ExposeBuilder::protocol().name("foo.bar").source(ExposeSource::Self_).build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
                    .use_(use_from_child_decl)
                    .expose(expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .protocol_default("foo.bar")
                    .expose(expose_from_self_decl)
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;
        let validator_server = RouteValidator::new(Arc::downgrade(&model));
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();

        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start(ComponentInput::default()).await;

        // `my_child` should not be resolved right now
        let instance = model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
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
        let instance = model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
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
        let invalid_source_name_use_from_child_decl =
            UseBuilder::protocol().source_static_child("my_child").name("a").build();
        let invalid_source_name_expose_from_child_decl =
            ExposeBuilder::protocol().name("c").source_static_child("my_child").build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(invalid_source_name_use_from_child_decl)
                    .expose(invalid_source_name_expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            ("my_child", ComponentDeclBuilder::new().build()),
        ];

        let TestModelResult { model, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;
        let validator_server = RouteValidator::new(Arc::downgrade(&model));

        let (validator, validator_request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();

        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), validator_request_stream).await
        });

        model.start(ComponentInput::default()).await;

        // `my_child` should not be resolved right now
        let instance = model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        // Validate the root
        let mut results = validator.validate(".").await.unwrap().unwrap();
        assert_eq!(results.len(), 2);

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
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "c"
        );

        // This validation should have caused `my_child` to be resolved
        let instance = model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_some());
    }

    #[fuchsia::test]
    async fn route() {
        let use_from_framework_decl = UseBuilder::protocol()
            .source(UseSource::Framework)
            .name("fuchsia.component.Realm")
            .build();
        let use_from_child_decl = UseBuilder::protocol()
            .source_static_child("my_child")
            .name("biz.buz")
            .path("/svc/foo.bar")
            .build();
        let expose_from_child_decl = ExposeBuilder::protocol()
            .name("biz.buz")
            .target_name("foo.bar")
            .source_static_child("my_child")
            .build();
        let expose_from_self_decl =
            ExposeBuilder::protocol().name("biz.buz").source(ExposeSource::Self_).build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
                    .use_(use_from_child_decl)
                    .expose(expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::protocol().name("biz.buz").path("/svc/foo.bar").build(),
                    )
                    .expose(expose_from_self_decl)
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;
        let validator_server = RouteValidator::new(Arc::downgrade(&model));
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start(ComponentInput::default()).await;

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
        let use_from_framework_decl =
            UseBuilder::protocol().source(UseSource::Framework).name("foo.bar").build();
        let expose_from_child_decl = ExposeBuilder::resolver()
            .name("qax.qux")
            .target_name("foo.buz")
            .source_static_child("my_child")
            .build();
        let expose_from_self_decl =
            ExposeBuilder::resolver().name("qax.qux").source(ExposeSource::Self_).build();
        let capability_decl =
            CapabilityBuilder::resolver().name("qax.qux").path("/svc/qax.qux").build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(use_from_framework_decl)
                    .expose(expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .capability(capability_decl)
                    .expose(expose_from_self_decl)
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;
        let validator_server = RouteValidator::new(Arc::downgrade(&model));
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start(ComponentInput::default()).await;

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
        let use_from_framework_decl =
            UseBuilder::protocol().source(UseSource::Framework).name("foo.bar").build();
        let use_from_framework_decl2 =
            UseBuilder::protocol().source(UseSource::Framework).name("foo.buz").build();
        let use_from_framework_decl3 =
            UseBuilder::protocol().source(UseSource::Framework).name("no.match").build();
        let expose_from_child_decl = ExposeBuilder::protocol()
            .name("qax.qux")
            .target_name("foo.buz")
            .source_static_child("my_child")
            .build();
        let expose_from_child_decl2 = ExposeBuilder::protocol()
            .name("qax.qux")
            .target_name("foo.biz")
            .source_static_child("my_child")
            .build();
        let expose_from_child_decl3 =
            ExposeBuilder::protocol().name("no.match").source(ExposeSource::Framework).build();
        let expose_from_self_decl =
            ExposeBuilder::protocol().name("qax.qux").source(ExposeSource::Self_).build();

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
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .protocol_default("qax.qux")
                    .expose(expose_from_self_decl)
                    .build(),
            ),
        ];

        let TestModelResult { model, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;
        let validator_server = RouteValidator::new(Arc::downgrade(&model));
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start(ComponentInput::default()).await;

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
        let offer_from_collection_decl = OfferBuilder::service()
            .name("my_service")
            .source(OfferSource::Collection("coll".parse().unwrap()))
            .target_static_child("target")
            .build();
        let expose_from_self_decl =
            ExposeBuilder::service().name("my_service").source(ExposeSource::Self_).build();
        let use_decl = UseBuilder::service().name("my_service").path("/svc/foo.bar").build();
        let capability_decl =
            CapabilityBuilder::service().name("my_service").path("/svc/foo.bar").build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .offer(offer_from_collection_decl)
                    .collection_default("coll")
                    .child_default("target")
                    .build(),
            ),
            ("target", ComponentDeclBuilder::new().use_(use_decl).build()),
            (
                "child_a",
                ComponentDeclBuilder::new()
                    .capability(capability_decl.clone())
                    .expose(expose_from_self_decl.clone())
                    .build(),
            ),
            (
                "child_b",
                ComponentDeclBuilder::new()
                    .capability(capability_decl.clone())
                    .expose(expose_from_self_decl.clone())
                    .build(),
            ),
        ];

        let TestModelResult { model, realm_proxy, mock_runner, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new()
                .set_components(components)
                .set_realm_moniker(Moniker::root())
                .build()
                .await;
        let realm_proxy = realm_proxy.unwrap();

        let validator_server = RouteValidator::new(Arc::downgrade(&model));
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start(ComponentInput::default()).await;

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
                .root()
                .find_and_maybe_resolve(&format!("coll:{}", name).as_str().try_into().unwrap())
                .await
                .unwrap();
            child.ensure_started(&StartReason::Debug).await.unwrap();
        }

        // Open the service directory from `target` so that it gets instantiated.
        {
            let target =
                model.root().find_and_maybe_resolve(&"target".try_into().unwrap()).await.unwrap();
            target.ensure_started(&StartReason::Debug).await.unwrap();
            mock_runner.wait_for_url("test:///target_resolved").await;
            let ns = mock_runner.get_namespace("test:///target_resolved").unwrap();
            let ns = ns.lock().await;
            // /pkg and /svc
            let mut ns = ns.clone().flatten();
            ns.sort();
            assert_eq!(ns.len(), 2);
            let ns = ns.remove(1);
            assert_eq!(ns.path.to_string(), "/svc");
            let svc_dir = ns.directory.into_proxy().unwrap();
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
                    child_name == format!("child `coll:child_{}`", child_id) &&
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
        let invalid_source_name_use_from_child_decl =
            UseBuilder::protocol().source_static_child("my_child").name("a").build();
        let invalid_source_name_expose_from_child_decl =
            ExposeBuilder::protocol().name("c").source_static_child("my_child").build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(invalid_source_name_use_from_child_decl)
                    .expose(invalid_source_name_expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            ("my_child", ComponentDeclBuilder::new().build()),
        ];

        let TestModelResult { model, builtin_environment: _b, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;
        let validator_server = RouteValidator::new(Arc::downgrade(&model));
        let (validator, request_stream) =
            endpoints::create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        let _validator_task = fasync::Task::local(async move {
            validator_server.serve(Moniker::root(), request_stream).await
        });

        model.start(ComponentInput::default()).await;

        // `my_child` should not be resolved right now
        let instance = model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        let targets = &[
            fsys::RouteTarget { name: "a".parse().unwrap(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget { name: "c".parse().unwrap(), decl_type: fsys::DeclType::Expose },
        ];
        let mut results = validator.route(".", targets).await.unwrap().unwrap();
        assert_eq!(results.len(), 2);

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
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "c"
        );
    }
}
