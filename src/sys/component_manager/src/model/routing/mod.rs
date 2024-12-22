// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod bedrock;
pub mod legacy;
pub mod open;
pub mod providers;
pub mod service;
pub use ::routing::error::RoutingError;
pub use open::*;

use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::storage;
use ::routing::capability_source::CapabilitySource;
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::error::{ErrorReporter, RouteRequestErrorInfo};
use ::routing::mapper::NoopRouteMapper;
use ::routing::{RouteRequest, RouteSource};
use async_trait::async_trait;
use cm_rust::{ExposeDecl, ExposeDeclCommon, UseStorageDecl};
use cm_types::{Availability, Name};
use errors::ModelError;
use router_error::RouterError;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{error, info, warn};
use vfs::directory::entry::OpenRequest;

pub use bedrock::RouteRequest as BedrockRouteRequest;

#[async_trait]
pub trait Route {
    /// Routes a capability from `target` to its source.
    ///
    /// If the capability is not allowed to be routed to the `target`, per the
    /// [`crate::model::policy::GlobalPolicyChecker`], the capability is not opened and an error
    /// is returned.
    async fn route(self, target: &Arc<ComponentInstance>) -> Result<RouteSource, RoutingError>;
}

#[async_trait]
impl Route for RouteRequest {
    async fn route(self, target: &Arc<ComponentInstance>) -> Result<RouteSource, RoutingError> {
        routing::route_capability(self, target, &mut NoopRouteMapper).await
    }
}

#[derive(Debug)]
pub enum RoutingOutcome {
    Found,
    FromVoid,
}

/// Routes a capability from `target` to its source. Opens the capability if routing succeeds.
///
/// If the capability is not allowed to be routed to the `target`, per the
/// [`crate::model::policy::GlobalPolicyChecker`], the capability is not opened and an error
/// is returned.
pub(super) async fn route_and_open_capability(
    route_request: &RouteRequest,
    target: &Arc<ComponentInstance>,
    open_request: OpenRequest<'_>,
) -> Result<RoutingOutcome, RouterError> {
    let source = route_request.clone().route(target).await.map_err(RouterError::from)?;
    if let CapabilitySource::Void(_) = source.source {
        return Ok(RoutingOutcome::FromVoid);
    };

    match route_request {
        RouteRequest::UseStorage(_) | RouteRequest::OfferStorage(_) => {
            let backing_dir_info =
                storage::route_backing_directory(target, source.source.clone()).await?;
            CapabilityOpenRequest::new_from_storage_source(backing_dir_info, target, open_request)
                .open()
                .await?;
        }
        _ => {
            // clone the source as additional context in case of an error
            CapabilityOpenRequest::new_from_route_source(source, target, open_request)
                .map_err(|e| RouterError::NotFound(Arc::new(e)))?
                .open()
                .await?;
        }
    };
    Ok(RoutingOutcome::Found)
}

/// Same as `route_and_open_capability` except this reports the routing failure.
pub(super) async fn route_and_open_capability_with_reporting(
    route_request: &RouteRequest,
    target: &Arc<ComponentInstance>,
    open_request: OpenRequest<'_>,
) -> Result<(), RouterError> {
    let result = route_and_open_capability(route_request, &target, open_request).await;
    match result {
        Ok(RoutingOutcome::Found) => Ok(()),
        Ok(RoutingOutcome::FromVoid) => {
            Err(RoutingError::SourceCapabilityIsVoid { moniker: target.moniker.clone() }.into())
        }
        Err(e) => {
            report_routing_failure(&route_request, route_request.availability(), &target, &e).await;
            Err(e)
        }
    }
}

pub struct RoutedStorage {
    backing_dir_info: storage::BackingDirectoryInfo,
    target: WeakComponentInstance,
}

pub(super) async fn route_storage(
    use_storage_decl: UseStorageDecl,
    target: &Arc<ComponentInstance>,
) -> Result<RoutedStorage, ModelError> {
    let storage_source = RouteRequest::UseStorage(use_storage_decl.clone()).route(target).await?;
    let backing_dir_info = storage::route_backing_directory(target, storage_source.source).await?;
    Ok(RoutedStorage { backing_dir_info, target: WeakComponentInstance::new(target) })
}

pub(super) async fn delete_storage(routed_storage: RoutedStorage) -> Result<(), ModelError> {
    let target = routed_storage.target.upgrade()?;

    // As of today, the storage component instance must contain the target. This is because
    // it is impossible to expose storage declarations up.
    let moniker = target
        .moniker()
        .strip_prefix(&routed_storage.backing_dir_info.storage_source_moniker)
        .unwrap();
    storage::delete_isolated_storage(routed_storage.backing_dir_info, moniker, target.instance_id())
        .await
}

/// ErrorReporter that calls report_routing_failure.
#[derive(Clone)]
pub struct RoutingFailureErrorReporter {
    target: WeakComponentInstance,
}

impl RoutingFailureErrorReporter {
    pub fn new(target: WeakComponentInstance) -> Self {
        Self { target }
    }
}

#[async_trait]
impl ErrorReporter for RoutingFailureErrorReporter {
    async fn report(&self, request: &RouteRequestErrorInfo, err: &RouterError) {
        match self.target.upgrade() {
            Ok(target) => {
                report_routing_failure(request, Some(request.availability()), &target, err).await;
            }
            Err(upgrade_err) => {
                error!(%upgrade_err, %err,
                    "Failed to upgrade WeakComponentInstance while reporting routing error.")
            }
        }
    }
}

/// Logs a failure to route a capability. Formats `err` as a `String`, but
/// elides the type if the error is a `RoutingError`, the common case.
pub async fn report_routing_failure(
    capability_requested: impl std::fmt::Display,
    availability: Option<Availability>,
    target: &Arc<ComponentInstance>,
    err: impl std::error::Error,
) {
    target
        .with_logger_as_default(|| {
            let availability = availability.unwrap_or(Availability::Required);
            let moniker = &target.moniker;
            match availability {
                Availability::Required => {
                    // TODO(https://fxbug.dev/42060474): consider changing this to `error!()`
                    warn!(
                        "{capability_requested} was not available for target `{moniker}`:\n\t\
                        {err}\n\tFor more, run `ffx component doctor {moniker}`",
                    );
                }
                Availability::Optional
                | Availability::SameAsTarget
                | Availability::Transitional => {
                    // If the target declared the capability as optional, but
                    // the capability could not be routed (such as if the source
                    // component is not available) the component _should_
                    // tolerate the missing optional capability. However, this
                    // should be logged. Developers are encouraged to change how
                    // they build and/or assemble different product
                    // configurations so declared routes are always end-to-end
                    // complete routes.
                    // TODO(https://fxbug.dev/42060474): if we change the log for
                    // `Required` capabilities to `error!()`, consider also
                    // changing this log for `Optional` to `warn!()`.
                    info!(
                        "{availability} {capability_requested} was not available for target `{moniker}`:\n\t\
                        {err}\n\tFor more, run `ffx component doctor {moniker}`",
                    );
                }
            }
        })
        .await
}

/// Group exposes by `target_name`. This will group all exposes that form an aggregate capability
/// together.
pub fn aggregate_exposes<'a>(
    exposes: impl Iterator<Item = &'a ExposeDecl>,
) -> BTreeMap<&'a Name, Vec<&'a ExposeDecl>> {
    let mut out: BTreeMap<&Name, Vec<&ExposeDecl>> = BTreeMap::new();
    for expose in exposes {
        out.entry(&expose.target_name()).or_insert(vec![]).push(expose);
    }
    out
}
