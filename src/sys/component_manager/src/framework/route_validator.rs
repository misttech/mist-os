// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::instance::ResolvedInstanceState;
use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::model::Model;
use crate::model::routing::service::AnonymizedServiceRoute;
use crate::model::routing::{self, BedrockRouteRequest, Route, RoutingError};
use ::routing::capability_source::{
    AnonymizedAggregateSource, CapabilitySource, InternalCapability,
};
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::RouteRequest as LegacyRouteRequest;
use async_trait::async_trait;
use cm_rust::{ExposeDecl, SourceName, UseDecl};
use cm_types::{Name, RelativePath};
use errors::{ActionError, StartActionError};
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use futures::future::join_all;
use futures::TryStreamExt;
use lazy_static::lazy_static;
use moniker::{ExtendedMoniker, Moniker};
use router_error::{Explain, RouterError};
use sandbox::{Capability, RouterResponse};
use std::cmp::Ordering;
use std::sync::{Arc, Weak};
use tracing::warn;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_sys2 as fsys,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fsys::RouteValidatorMarker::PROTOCOL_NAME.parse().unwrap();
}

impl RouteValidatorCapabilityProvider {
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

            let mut uses = resolved.decl().uses.clone();
            if let Some(runner) = resolved.decl().program.as_ref().and_then(|d| d.runner.as_ref()) {
                uses.push(Self::use_for_runner(runner));
            }
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
            let (availability, res) = request.route(&instance).await;
            match res {
                Ok((outcome, moniker, service_instances)) => {
                    let moniker = extended_moniker_to_str(scope_moniker, moniker);
                    fsys::RouteReport {
                        capability,
                        decl_type,
                        outcome: Some(outcome),
                        error: None,
                        source_moniker: Some(moniker),
                        availability,
                        service_instances,
                        ..Default::default()
                    }
                }
                Err(e) => {
                    let error = Some(fsys::RouteError {
                        summary: Some(e.to_string()),
                        ..Default::default()
                    });
                    fsys::RouteReport {
                        capability,
                        decl_type,
                        error,
                        outcome: Some(fsys::RouteOutcome::Failed),
                        availability,
                        ..Default::default()
                    }
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
            let mut requests: Vec<_> = resolved
                .decl()
                .uses
                .iter()
                .map(|use_| {
                    let target = fsys::RouteTarget {
                        name: use_.source_name().as_str().into(),
                        decl_type: fsys::DeclType::Use,
                    };
                    let request = RouteRequest::try_from(use_.clone()).unwrap();
                    (target, request)
                })
                .collect();
            // `program.runner`, if set, is equivalent to a `use`.
            if let Some(runner) = resolved.decl().program.as_ref().and_then(|d| d.runner.as_ref()) {
                let target = fsys::RouteTarget {
                    name: runner.as_str().into(),
                    decl_type: fsys::DeclType::Use,
                };
                let use_ = Self::use_for_runner(runner);
                let request = RouteRequest::try_from(use_).unwrap();
                requests.push((target, request));
            }

            let exposes = routing::aggregate_exposes(resolved.decl().exposes.iter());
            requests.extend(exposes.into_iter().map(|(target_name, e)| {
                let target = fsys::RouteTarget {
                    name: target_name.to_string(),
                    decl_type: fsys::DeclType::Expose,
                };
                let request = RouteRequest::from_expose_decls(resolved.moniker(), e).unwrap();
                (target, request)
            }));
            Ok(requests)
        } else {
            // Return results that fuzzy match (substring match) `target.name`.
            let targets = targets
                .into_iter()
                .map(|target| match target.decl_type {
                    fsys::DeclType::Any => {
                        let mut use_target = target.clone();
                        use_target.decl_type = fsys::DeclType::Use;
                        let mut expose_target = target;
                        expose_target.decl_type = fsys::DeclType::Expose;
                        vec![use_target, expose_target].into_iter()
                    }
                    _ => vec![target].into_iter(),
                })
                .flatten();
            targets
                .map(|target| match target.decl_type {
                    fsys::DeclType::Use => {
                        let mut matching_requests: Vec<_> = resolved
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
                                let request = RouteRequest::try_from(u.clone()).unwrap();
                                Some(Ok((target, request)))
                            })
                            .collect();
                        // `program.runner`, if set, is equivalent to a `use`.
                        if let Some(runner) = resolved
                            .decl()
                            .program
                            .as_ref()
                            .and_then(|d| d.runner.as_ref())
                            .filter(|r| r.as_str().contains(&target.name))
                        {
                            let target = fsys::RouteTarget {
                                name: runner.as_str().into(),
                                decl_type: fsys::DeclType::Use,
                            };
                            let u = Self::use_for_runner(runner);
                            let request = RouteRequest::try_from(u).unwrap();
                            matching_requests.push(Ok((target, request)));
                        }

                        matching_requests.into_iter()
                    }
                    fsys::DeclType::Expose => {
                        let exposes = routing::aggregate_exposes(resolved.decl().exposes.iter());

                        #[allow(clippy::needless_collect)] // aligns the iterator type w/ above
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
                                let request =
                                    RouteRequest::from_expose_decls(resolved.moniker(), e).unwrap();
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

    fn use_for_runner(runner: &Name) -> cm_rust::UseDecl {
        cm_rust::UseDecl::Runner(cm_rust::UseRunnerDecl {
            source: cm_rust::UseSource::Environment,
            source_name: runner.clone(),
            source_dictionary: RelativePath::dot(),
        })
    }

    /// Serve the fuchsia.sys2.RouteValidator protocol for a given scope on a given stream
    async fn serve(self, mut stream: fsys::RouteValidatorRequestStream) {
        let res: Result<(), fidl::Error> = async move {
            while let Some(request) = stream.try_next().await? {
                let Some(model) = self.model.upgrade() else {
                    return Ok(());
                };
                match request {
                    fsys::RouteValidatorRequest::Validate { moniker, responder } => {
                        let result = Self::validate(&model, &self.scope_moniker, &moniker).await;
                        if let Err(e) = responder.send(result.as_deref().map_err(|e| *e)) {
                            warn!(error = %e, "RouteValidator failed to send Validate response");
                        }
                    }
                    fsys::RouteValidatorRequest::Route { moniker, targets, responder } => {
                        let result =
                            Self::route(&model, &self.scope_moniker, &moniker, targets).await;
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

#[derive(Clone, Debug)]
enum RouteRequest {
    Bedrock(BedrockRouteRequest),
    Legacy(LegacyRouteRequest),
}

impl RouteRequest {
    async fn route(
        self,
        instance: &Arc<ComponentInstance>,
    ) -> (
        Option<fdecl::Availability>,
        Result<
            (fsys::RouteOutcome, ExtendedMoniker, Option<Vec<fsys::ServiceInstance>>),
            Box<dyn Explain>,
        >,
    ) {
        match self {
            Self::Legacy(route_request) => {
                let availability = route_request.availability().map(From::from);
                let res = Self::route_legacy(route_request, instance)
                    .await
                    .map_err(|e| -> Box<dyn Explain> { Box::new(e) });
                (availability, res)
            }
            Self::Bedrock(route_request) => {
                let availability = Some(route_request.availability().into());
                let res = Self::route_bedrock(route_request, instance)
                    .await
                    .map_err(|e| -> Box<dyn Explain> { Box::new(e) });
                (availability, res)
            }
        }
    }

    async fn route_legacy(
        route_request: LegacyRouteRequest,
        instance: &Arc<ComponentInstance>,
    ) -> Result<
        (fsys::RouteOutcome, ExtendedMoniker, Option<Vec<fsys::ServiceInstance>>),
        RoutingError,
    > {
        let source = route_request.route(&instance).await?;
        let source = source.source;
        let source_moniker = source.source_moniker();
        let outcome = match &source {
            CapabilitySource::Void(_) => fsys::RouteOutcome::Void,
            _ => fsys::RouteOutcome::Success,
        };
        let service_info = match source {
            CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
                capability,
                moniker,
                members,
                ..
            }) => {
                let component = instance.find_absolute(&moniker).await?;
                let route = AnonymizedServiceRoute {
                    source_moniker: component.moniker.clone(),
                    members: members.clone(),
                    service_name: capability.source_name().clone(),
                };
                let state = component.lock_state().await;
                if let Some(service_dir) = state
                    .get_resolved_state()
                    .and_then(|r| r.anonymized_services.get(&route).cloned())
                {
                    let mut service_info = service_dir.entries().await;
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
                                instance_name: Some(e.name.clone().into()),
                                child_name: Some(child_name),
                                child_instance_name: Some(e.service_instance.clone().into()),
                                ..Default::default()
                            }
                        })
                        .collect();
                    Some(service_info)
                } else {
                    None
                }
            }
            _ => None,
        };
        Ok((outcome, source_moniker, service_info))
    }

    async fn route_bedrock(
        route_request: BedrockRouteRequest,
        instance: &Arc<ComponentInstance>,
    ) -> Result<
        (fsys::RouteOutcome, ExtendedMoniker, Option<Vec<fsys::ServiceInstance>>),
        RouterError,
    > {
        let res: Result<Capability, ActionError> = async move {
            let resolved_state = instance.lock_resolved_state().await.map_err(|err| {
                ActionError::from(StartActionError::ResolveActionError {
                    moniker: instance.moniker.clone(),
                    err: Box::new(err),
                })
            })?;
            Ok(route_request.into_router(instance.as_weak(), &resolved_state.sandbox))
        }
        .await;
        let router = res.map_err(|e| RouterError::NotFound(Arc::new(e)))?;

        let res = match router {
            Capability::ConnectorRouter(router) => {
                router.route(None, true).await.and_then(|resp| match resp {
                    RouterResponse::Debug(data) => Ok(data),
                    _ => {
                        warn!("[route_validator] Route did not return debug info");
                        Err(RouterError::Internal)
                    }
                })
            }
            Capability::DictionaryRouter(router) => {
                router.route(None, true).await.and_then(|resp| match resp {
                    RouterResponse::Debug(data) => Ok(data),
                    _ => {
                        warn!("[route_validator] Route did not return debug info");
                        Err(RouterError::Internal)
                    }
                })
            }
            Capability::DirEntryRouter(router) => {
                router.route(None, true).await.and_then(|resp| match resp {
                    RouterResponse::Debug(data) => Ok(data),
                    _ => {
                        warn!("[route_validator] Route did not return debug info");
                        Err(RouterError::Internal)
                    }
                })
            }
            Capability::DataRouter(router) => {
                router.route(None, true).await.and_then(|resp| match resp {
                    RouterResponse::Debug(data) => Ok(data),
                    _ => {
                        warn!("[route_validator] Route did not return debug info");
                        Err(RouterError::Internal)
                    }
                })
            }
            _ => {
                warn!("[route_validator] Sandbox capability was not a Router type");
                Err(RouterError::Internal)
            }
        };
        res.map(|data| {
            let capability_source = CapabilitySource::try_from(data)
                .expect("failed to convert Data to capability source");
            let outcome = match &capability_source {
                CapabilitySource::Void(_) => fsys::RouteOutcome::Void,
                _ => fsys::RouteOutcome::Success,
            };
            (outcome, capability_source.source_moniker(), None)
        })
    }

    pub fn from_expose_decls(
        moniker: &Moniker,
        exposes: Vec<&ExposeDecl>,
    ) -> Result<Self, RoutingError> {
        match BedrockRouteRequest::try_from(&exposes) {
            Ok(r) => Ok(Self::Bedrock(r)),
            Err(()) => Ok(Self::Legacy(LegacyRouteRequest::from_expose_decls(moniker, exposes)?)),
        }
    }
}

impl From<UseDecl> for RouteRequest {
    fn from(u: UseDecl) -> Self {
        match BedrockRouteRequest::try_from(u) {
            Ok(r) => Self::Bedrock(r),
            Err(r) => Self::Legacy(r.into()),
        }
    }
}

pub struct RouteValidatorFrameworkCapability {
    model: Weak<Model>,
}

impl RouteValidatorFrameworkCapability {
    pub fn new(model: Weak<Model>) -> Self {
        Self { model }
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
        Box::new(RouteValidatorCapabilityProvider {
            model: self.model.clone(),
            scope_moniker: scope.moniker,
        })
    }
}

struct RouteValidatorCapabilityProvider {
    model: Weak<Model>,
    scope_moniker: Moniker,
}

#[async_trait]
impl InternalCapabilityProvider for RouteValidatorCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsys::RouteValidatorMarker>::new(server_end);
        self.serve(server_end.into_stream()).await;
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
        let (availability, res) = route_request.route(instance).await;
        let report = match res {
            Ok((outcome, moniker, service_instances)) => {
                let moniker = extended_moniker_to_str(&Moniker::root(), moniker);
                fsys::RouteReport {
                    outcome: Some(outcome),
                    source_moniker: Some(moniker),
                    service_instances,
                    capability,
                    decl_type,
                    availability,
                    ..Default::default()
                }
            }
            Err(e) => {
                let outcome = Some(fsys::RouteOutcome::Failed);
                let error =
                    Some(fsys::RouteError { summary: Some(e.to_string()), ..Default::default() });
                fsys::RouteReport {
                    outcome,
                    capability,
                    decl_type,
                    error,
                    availability,
                    ..Default::default()
                }
            }
        };
        reports.push(report);
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
        let report = match RouteRequest::from_expose_decls(&instance.moniker, e) {
            Ok(route_request) => {
                let (availability, res) = route_request.route(instance).await;
                match res {
                    Ok((outcome, moniker, service_instances)) => {
                        let moniker = extended_moniker_to_str(&Moniker::root(), moniker);
                        fsys::RouteReport {
                            outcome: Some(outcome),
                            source_moniker: Some(moniker),
                            service_instances,
                            capability,
                            decl_type,
                            availability,
                            ..Default::default()
                        }
                    }
                    Err(e) => {
                        let error = Some(fsys::RouteError {
                            summary: Some(e.to_string()),
                            ..Default::default()
                        });
                        fsys::RouteReport {
                            capability,
                            decl_type,
                            outcome: Some(fsys::RouteOutcome::Failed),
                            error,
                            availability,
                            ..Default::default()
                        }
                    }
                }
            }
            Err(e) => {
                let error =
                    Some(fsys::RouteError { summary: Some(e.to_string()), ..Default::default() });
                fsys::RouteReport {
                    capability,
                    decl_type,
                    outcome: Some(fsys::RouteOutcome::Failed),
                    error,
                    ..Default::default()
                }
            }
        };
        reports.push(report);
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability;
    use crate::model::component::StartReason;
    use crate::model::start::Start;
    use crate::model::testing::out_dir::OutDir;
    use crate::model::testing::test_helpers::{TestEnvironmentBuilder, TestModelResult};
    use assert_matches::assert_matches;
    use cm_rust::*;
    use cm_rust_testing::*;
    use fidl::endpoints;
    use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio};

    async fn route_validator(
        test: &TestModelResult,
    ) -> (fsys::RouteValidatorProxy, RouteValidatorFrameworkCapability) {
        let host = RouteValidatorFrameworkCapability::new(Arc::downgrade(&test.model));
        let (proxy, server) = endpoints::create_proxy::<fsys::RouteValidatorMarker>();
        capability::open_framework(&host, test.model.root(), server.into()).await.unwrap();
        (proxy, host)
    }

    #[derive(Ord, PartialOrd, Eq, PartialEq)]
    struct Key {
        capability: String,
        decl_type: fsys::DeclType,
    }

    /// Validate API reports that routing succeeded normally.
    #[fuchsia::test]
    async fn validate() {
        // Test several capability types.
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new_empty_component()
                    .use_(UseBuilder::runner().name("elf").source_static_child("my_child"))
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Framework)
                            .name("fuchsia.component.Realm"),
                    )
                    .use_(UseBuilder::protocol().name("foo.bar").source_static_child("my_child"))
                    .expose(
                        ExposeBuilder::protocol().name("foo.bar").source_static_child("my_child"),
                    )
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .dictionary_default("dict")
                    .protocol_default("foo.bar")
                    .runner_default("elf")
                    .expose(ExposeBuilder::runner().name("elf").source(ExposeSource::Self_))
                    .expose(ExposeBuilder::dictionary().name("dict").source(ExposeSource::Self_))
                    .expose(ExposeBuilder::protocol().name("foo.bar").source(ExposeSource::Self_))
                    .build(),
            ),
        ];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // Validate the root
        let mut results = validator.validate(".").await.unwrap().unwrap();

        results.sort_by_key(|r| Key {
            capability: r.capability.clone().unwrap(),
            decl_type: r.decl_type.clone().unwrap(),
        });

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "elf" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "foo.bar" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
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
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.component.Realm" && m == "."
        );

        assert!(results.is_empty());

        // Validate `my_child`
        let mut results = validator.validate("my_child").await.unwrap().unwrap();

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "test_runner" && m == "<component_manager>"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "dict" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "elf" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "foo.bar" && m == "my_child"
        );

        assert!(results.is_empty());
    }

    /// Validate API reports that routing succeeded with a `void` source.
    #[fuchsia::test]
    async fn validate_from_void() {
        let use_from_child_decl = UseBuilder::protocol()
            .source_static_child("my_child")
            .name("foo.bar")
            .availability(cm_rust::Availability::Optional)
            .build();
        let expose_from_child_decl = ExposeBuilder::protocol()
            .name("foo.bar")
            .source_static_child("my_child")
            .availability(cm_rust::Availability::Optional)
            .build();
        let expose_from_void_decl = ExposeBuilder::protocol()
            .name("foo.bar")
            .source(ExposeSource::Void)
            .availability(cm_rust::Availability::Optional)
            .build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new_empty_component()
                    .use_(use_from_child_decl)
                    .expose(expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new_empty_component()
                    .protocol_default("foo.bar")
                    .expose(expose_from_void_decl)
                    .build(),
            ),
        ];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // `my_child` should not be resolved right now
        let instance = test.model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_none());

        // Validate the root
        let mut results = validator.validate(".").await.unwrap().unwrap();

        results.sort_by_key(|r| Key {
            capability: r.capability.clone().unwrap(),
            decl_type: r.decl_type.clone().unwrap(),
        });

        assert_eq!(results.len(), 2);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Void),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                availability: Some(fdecl::Availability::Optional),
                error: None,
                ..
            } if s == "foo.bar" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Void),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                availability: Some(fdecl::Availability::Optional),
                error: None,
                ..
            } if s == "foo.bar" && m == "my_child"
        );

        // This validation should have caused `my_child` to be resolved
        let instance = test.model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_some());

        // Validate `my_child`
        let mut results = validator.validate("my_child").await.unwrap().unwrap();
        assert_eq!(results.len(), 1);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Void),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                availability: Some(fdecl::Availability::Optional),
                error: None,
                ..
            } if s == "foo.bar" && m == "my_child"
        );
    }

    /// Validate API reports that routing failed and returns the error.
    #[fuchsia::test]
    async fn validate_error() {
        let invalid_source_name_use_from_child_decl =
            UseBuilder::protocol().source_static_child("my_child").name("a").build();
        let invalid_source_name_expose_from_child_decl =
            ExposeBuilder::protocol().name("c").source_static_child("my_child").build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new_empty_component()
                    .use_(invalid_source_name_use_from_child_decl)
                    .expose(invalid_source_name_expose_from_child_decl)
                    .child_default("my_child")
                    .build(),
            ),
            ("my_child", ComponentDeclBuilder::new_empty_component().build()),
        ];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // `my_child` should not be resolved right now
        let instance = test.model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
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
                outcome: Some(fsys::RouteOutcome::Failed),
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
                outcome: Some(fsys::RouteOutcome::Failed),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "c"
        );

        // This validation should have caused `my_child` to be resolved
        let instance = test.model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
        assert!(instance.is_some());
    }

    /// Route API reports that routing succeeded normally, with exact capability names as inputs.
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

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

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
                outcome: Some(fsys::RouteOutcome::Success),
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
                outcome: Some(fsys::RouteOutcome::Success),
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
                outcome: Some(fsys::RouteOutcome::Success),
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
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "biz.buz" && m == "my_child"
        );
    }

    /// Route API reports that routing succeeded with a `void` source.
    #[fuchsia::test]
    async fn route_from_void() {
        let use_from_child_decl = UseBuilder::protocol()
            .source_static_child("my_child")
            .name("biz.buz")
            .path("/svc/foo.bar")
            .availability(cm_rust::Availability::Optional)
            .build();
        let expose_from_child_decl = ExposeBuilder::protocol()
            .name("biz.buz")
            .target_name("foo.bar")
            .source_static_child("my_child")
            .availability(cm_rust::Availability::Optional)
            .build();
        let expose_from_void_decl = ExposeBuilder::protocol()
            .name("biz.buz")
            .source(ExposeSource::Void)
            .availability(cm_rust::Availability::Optional)
            .build();

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
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
                    .expose(expose_from_void_decl)
                    .build(),
            ),
        ];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // Validate the root
        let targets = &[
            fsys::RouteTarget { name: "biz.buz".parse().unwrap(), decl_type: fsys::DeclType::Use },
            fsys::RouteTarget {
                name: "foo.bar".parse().unwrap(),
                decl_type: fsys::DeclType::Expose,
            },
        ];
        let mut results = validator.route(".", targets).await.unwrap().unwrap();

        assert_eq!(results.len(), 2);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Void),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                availability: Some(fdecl::Availability::Optional),
                error: None,
                ..
            } if s == "biz.buz" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Void),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                availability: Some(fdecl::Availability::Optional),
                error: None,
                ..
            } if s == "foo.bar" && m == "my_child"
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
                outcome: Some(fsys::RouteOutcome::Void),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                availability: Some(fdecl::Availability::Optional),
                error: None,
                ..
            } if s == "biz.buz" && m == "my_child"
        );
    }

    /// Route API reports that routing succeeded normally, with no capability names (that is, route
    /// all capabilities).
    #[fuchsia::test]
    async fn route_all() {
        // Test several capability types.
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new_empty_component()
                    .use_(UseBuilder::runner().name("elf").source_static_child("my_child"))
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Framework)
                            .name("fuchsia.component.Realm"),
                    )
                    .expose(
                        ExposeBuilder::runner()
                            .name("elf")
                            .target_name("exposed_elf")
                            .source_static_child("my_child"),
                    )
                    .expose(
                        ExposeBuilder::resolver()
                            .name("qax.qux")
                            .target_name("foo.buz")
                            .source_static_child("my_child"),
                    )
                    .expose(ExposeBuilder::dictionary().name("dict").source(ExposeSource::Self_))
                    .dictionary_default("dict")
                    .child_default("my_child")
                    .build(),
            ),
            (
                "my_child",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::resolver().name("qax.qux").path("/svc/qax.qux"))
                    .runner_default("elf")
                    .expose(ExposeBuilder::runner().name("elf").source(ExposeSource::Self_))
                    .expose(ExposeBuilder::resolver().name("qax.qux").source(ExposeSource::Self_))
                    .build(),
            ),
        ];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // Validate the root, passing an empty vector. This should match all capabilities
        let mut results = validator.route(".", &[]).await.unwrap().unwrap();

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "elf" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.component.Realm" && m == "."
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "dict" && m == "."
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "exposed_elf" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "foo.buz" && m == "my_child"
        );

        assert!(results.is_empty());

        // Validate the child, passing an empty vector. Here we only care about checking that the
        // program's runner was routed.
        let mut results = validator.route("my_child", &[]).await.unwrap().unwrap();

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "test_runner" && m == "<component_manager>"
        );
    }

    /// Route API reports that routing succeeded normally, with a partial capability name (fuzzy
    /// match).
    #[fuchsia::test]
    async fn route_fuzzy() {
        let use_decl = UseBuilder::protocol()
            .source(UseSource::Framework)
            .name("fuchsia.component.Realm")
            .build();
        let use_decl2 = UseBuilder::protocol().source(UseSource::Self_).name("fuchsia.foo").build();
        let use_decl3 =
            UseBuilder::protocol().source(UseSource::Framework).name("no.match").build();
        let expose_from_child_decl = ExposeBuilder::protocol()
            .name("qax.qux")
            .target_name("fuchsia.buz")
            .source_static_child("my_child")
            .build();
        let expose_from_child_decl2 = ExposeBuilder::protocol()
            .name("qax.qux")
            .target_name("fuchsia.biz")
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
                    .use_(use_decl)
                    .use_(use_decl2)
                    .use_(use_decl3)
                    .expose(expose_from_child_decl)
                    .expose(expose_from_child_decl2)
                    .expose(expose_from_child_decl3)
                    .protocol_default("fuchsia.foo")
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

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // Validate the root
        let targets = &[fsys::RouteTarget {
            name: "fuchsia.".parse().unwrap(),
            decl_type: fsys::DeclType::Any,
        }];
        let mut results = validator.route(".", targets).await.unwrap().unwrap();

        assert_eq!(results.len(), 4);

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.component.Realm" && m == "."
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.foo" && m == "."
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.biz" && m == "my_child"
        );

        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "fuchsia.buz" && m == "my_child"
        );

        // Validate the child (program runner)
        let targets = &[fsys::RouteTarget {
            name: "test_run".parse().unwrap(),
            decl_type: fsys::DeclType::Any,
        }];
        let mut results = validator.route("my_child", targets).await.unwrap().unwrap();

        assert_eq!(results.len(), 1);
        let report = results.remove(0);
        assert_matches!(
            report,
            fsys::RouteReport {
                outcome: Some(fsys::RouteOutcome::Success),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Use),
                source_moniker: Some(m),
                error: None,
                ..
            } if s == "test_runner" && m == "<component_manager>"
        );
    }

    /// Route API reports that routing succeeded normally with a service capability, including
    /// returing service info.
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

        let test = TestEnvironmentBuilder::new()
            .set_components(components)
            .set_realm_moniker(Moniker::root())
            .build()
            .await;
        let realm_proxy = test.realm_proxy.as_ref().unwrap();
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

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
            test.mock_runner.add_host_fn(&format!("test:///{}_resolved", name), out_dir.host_fn());

            let child = test
                .model
                .root()
                .find_and_maybe_resolve(&format!("coll:{}", name).as_str().try_into().unwrap())
                .await
                .unwrap();
            child.ensure_started(&StartReason::Debug).await.unwrap();
        }

        // Open the service directory from `target` so that it gets instantiated.
        {
            let target = test
                .model
                .root()
                .find_and_maybe_resolve(&"target".try_into().unwrap())
                .await
                .unwrap();
            target.ensure_started(&StartReason::Debug).await.unwrap();
            test.mock_runner.wait_for_url("test:///target_resolved").await;
            let ns = test.mock_runner.get_namespace("test:///target_resolved").unwrap();
            let ns = ns.lock().await;
            // /pkg and /svc
            let mut ns = ns.clone().flatten();
            ns.sort();
            assert_eq!(ns.len(), 2);
            let ns = ns.remove(1);
            assert_eq!(ns.path.to_string(), "/svc");
            let svc_dir = ns.directory.into_proxy();
            fuchsia_fs::directory::open_directory(&svc_dir, "foo.bar", fio::Flags::empty())
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
                outcome: Some(fsys::RouteOutcome::Success),
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

    /// Route API reports that routing failed and returns the error.
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

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (validator, _host) = route_validator(&test).await;

        test.model.start().await;

        // `my_child` should not be resolved right now
        let instance = test.model.root().find_resolved(&vec!["my_child"].try_into().unwrap()).await;
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
                outcome: Some(fsys::RouteOutcome::Failed),
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
                outcome: Some(fsys::RouteOutcome::Failed),
                capability: Some(s),
                decl_type: Some(fsys::DeclType::Expose),
                source_moniker: None,
                error: Some(_),
                ..
            } if s == "c"
        );
    }
}
