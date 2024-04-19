// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod open;
pub mod providers;
pub mod router;
pub mod router_ext;
pub mod service;
pub use ::routing::error::RoutingError;
pub use open::*;

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, WeakComponentInstance},
            error::ModelError,
            storage,
        },
    },
    ::routing::{component_instance::ComponentInstanceInterface, mapper::NoopRouteMapper},
    async_trait::async_trait,
    bedrock_error::{BedrockError, Explain},
    cm_rust::{ExposeDecl, ExposeDeclCommon, UseStorageDecl},
    cm_types::{Availability, Name},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio,
    moniker::MonikerBase,
    sandbox::{Capability, Open},
    std::{collections::BTreeMap, sync::Arc},
    tracing::{info, warn},
    vfs::{directory::entry::OpenRequest, path::Path, ToObjectRequest},
};

pub type RouteRequest = ::routing::RouteRequest;
pub type RouteSource = ::routing::RouteSource<ComponentInstance>;

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

// Helper function to log and return an error if the capability is void.
fn check_source_for_void(source: &CapabilitySource) -> Result<(), RoutingError> {
    if let CapabilitySource::Void { .. } = source {
        return Err(RoutingError::SourceCapabilityIsVoid.into());
    };
    Ok(())
}

pub(super) fn capability_into_open(capability: Capability) -> Result<Open, BedrockError> {
    match capability {
        Capability::Unit(_) => Err(RoutingError::SourceCapabilityIsVoid.into()),
        cap => Ok(Open::new(
            cap.try_into_directory_entry()
                .map_err(crate::model::error::OpenError::DoesNotSupportOpen)?,
        )),
    }
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
) -> Result<(), BedrockError> {
    match route_request.clone() {
        r @ RouteRequest::UseStorage(_) | r @ RouteRequest::OfferStorage(_) => {
            let storage_source = r.route(target).await?;
            check_source_for_void(&storage_source.source)?;

            let backing_dir_info =
                storage::route_backing_directory(storage_source.source.clone()).await?;
            Ok(CapabilityOpenRequest::new_from_storage_source(
                backing_dir_info,
                target,
                open_request,
            )
            .open()
            .await?)
        }
        r => {
            let route_source = r.route(target).await?;
            check_source_for_void(&route_source.source)?;

            // clone the source as additional context in case of an error

            Ok(CapabilityOpenRequest::new_from_route_source(route_source, target, open_request)
                .map_err(|e| BedrockError::RoutingError(Arc::new(e)))?
                .open()
                .await?)
        }
    }
}

/// Same as `route_and_open_capability` except this returns a new request.  This will only work for
/// protocols.
pub(super) async fn open_capability<Proxy: fidl::endpoints::Proxy>(
    route_request: &RouteRequest,
    target: &Arc<ComponentInstance>,
) -> Result<Proxy, BedrockError> {
    let (proxy, server) = create_proxy::<Proxy::Protocol>().unwrap();
    let mut object_request = fio::OpenFlags::empty().to_object_request(server);
    route_and_open_capability(
        route_request,
        target,
        OpenRequest::new(
            target.execution_scope.clone(),
            fio::OpenFlags::empty(),
            Path::dot(),
            &mut object_request,
        ),
    )
    .await?;
    Ok(proxy)
}

/// Create a new `RouteRequest` from an `ExposeDecl`, checking that the capability type can
/// be installed in a namespace.
///
/// REQUIRES: `exposes` is nonempty.
/// REQUIRES: `exposes` share the same type and target name.
/// REQUIRES: `exposes.len() > 1` only if it is a service.
pub fn request_for_namespace_capability_expose(exposes: Vec<&ExposeDecl>) -> Option<RouteRequest> {
    let first_expose = exposes.first().expect("invalid empty expose list");
    match first_expose {
        cm_rust::ExposeDecl::Protocol(_)
        | cm_rust::ExposeDecl::Service(_)
        | cm_rust::ExposeDecl::Directory(_) => Some(exposes.into()),
        // These do not add directory entries.
        cm_rust::ExposeDecl::Runner(_)
        | cm_rust::ExposeDecl::Resolver(_)
        | cm_rust::ExposeDecl::Config(_) => None,
        cm_rust::ExposeDecl::Dictionary(_) => {
            // TODO(https://fxbug.dev/301674053): Support this.
            None
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
    let backing_dir_info = storage::route_backing_directory(storage_source.source).await?;
    Ok(RoutedStorage { backing_dir_info, target: WeakComponentInstance::new(target) })
}

pub(super) async fn delete_storage(routed_storage: RoutedStorage) -> Result<(), ModelError> {
    let target = routed_storage.target.upgrade()?;

    // As of today, the storage component instance must contain the target. This is because
    // it is impossible to expose storage declarations up.
    let moniker = target
        .instanced_moniker()
        .strip_prefix(&routed_storage.backing_dir_info.storage_source_moniker)
        .unwrap();
    storage::delete_isolated_storage(
        routed_storage.backing_dir_info,
        target.persistent_storage,
        moniker,
        target.instance_id(),
    )
    .await
}

static ROUTE_ERROR_HELP: &'static str = "To learn more, see \
https://fuchsia.dev/go/components/connect-errors";

/// Sets an epitaph on `server_end` for a capability routing failure, and logs the error. Logs a
/// failure to route a capability. Formats `err` as a `String`, but elides the type if the error is
/// a `RoutingError`, the common case.
pub async fn report_routing_failure(
    request: &RouteRequest,
    target: &Arc<ComponentInstance>,
    err: &impl Explain,
) {
    target
        .with_logger_as_default(|| {
            let availability = request.target_use_availability().unwrap_or(Availability::Required);
            match availability {
                Availability::Required => {
                    // TODO(https://fxbug.dev/42060474): consider changing this to `error!()`
                    warn!(
                    "{availability} {request} was not available for target component `{}`: {}\n{}",
                    &target.moniker, &err, ROUTE_ERROR_HELP
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
                    "{availability} {request} was not available for target component `{}`: {}\n{}",
                    &target.moniker, &err, ROUTE_ERROR_HELP
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
