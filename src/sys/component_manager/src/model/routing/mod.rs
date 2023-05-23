// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod open;
pub mod providers;
pub mod service;
pub use ::routing::error::RoutingError;
pub use open::*;

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::ComponentInstance,
            error::{ModelError, RouteAndOpenCapabilityError},
            storage,
        },
    },
    ::routing::{
        self, capability_source::ComponentCapability,
        component_instance::ComponentInstanceInterface, error::AvailabilityRoutingError,
        mapper::NoopRouteMapper, router::RouteBundle,
    },
    async_trait::async_trait,
    cm_moniker::InstancedRelativeMoniker,
    cm_rust::{CapabilityTypeName, ExposeDecl, ExposeDeclCommon, UseDecl, UseStorageDecl},
    fidl::epitaph::ChannelEpitaphExt,
    fuchsia_zircon as zx,
    moniker::RelativeMonikerBase,
    std::{collections::BTreeMap, sync::Arc},
    tracing::{debug, info, warn},
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
        let optional_use = self.target_use_optional();
        routing::route_capability(self, target, &mut NoopRouteMapper).await.map_err(|err| {
            if optional_use {
                match err {
                    RoutingError::AvailabilityRoutingError(_) => {
                        // `err` is already an AvailabilityRoutingError.
                        // Return it as-is.
                        err
                    }
                    _ => {
                        // Wrap the error, to surface the target's
                        // optional usage.
                        RoutingError::AvailabilityRoutingError(
                            AvailabilityRoutingError::FailedToRouteToOptionalTarget {
                                reason: err.to_string(),
                            },
                        )
                    }
                }
            } else {
                // Not an optional `use` so return the error as-is.
                err
            }
        })
    }
}

/// Routes a capability from `target` to its source. Opens the capability if routing succeeds.
///
/// If the capability is not allowed to be routed to the `target`, per the
/// [`crate::model::policy::GlobalPolicyChecker`], the capability is not opened and an error
/// is returned.
pub(super) async fn route_and_open_capability(
    route_request: RouteRequest,
    target: &Arc<ComponentInstance>,
    open_options: OpenOptions<'_>,
) -> Result<(), RouteAndOpenCapabilityError> {
    match route_request.clone() {
        r @ RouteRequest::UseStorage(_) | r @ RouteRequest::OfferStorage(_) => {
            let storage_source = r.route(target).await.map_err(|e| {
                RouteAndOpenCapabilityError::RoutingError { request: route_request, err: e }
            })?;

            let backing_dir_info = storage::route_backing_directory(storage_source.source.clone())
                .await
                .map_err(|e| {
                    let storage_decl = match &storage_source.source {
                        CapabilitySource::Component {
                            capability: ComponentCapability::Storage(storage_decl),
                            ..
                        } => storage_decl.clone(),
                        r => unreachable!("unexpected storage source: {:?}", r),
                    };

                    let request = RouteRequest::StorageBackingDirectory(storage_decl);

                    RouteAndOpenCapabilityError::RoutingError { request, err: e }
                })?;

            Ok(OpenRequest::new_from_storage_source(backing_dir_info, target, open_options)
                .open()
                .await
                .map_err(|e| RouteAndOpenCapabilityError::OpenError {
                    source: storage_source.source,
                    err: e,
                })?)
        }
        r => {
            let route_source = r.route(target).await.map_err(|e| {
                RouteAndOpenCapabilityError::RoutingError { request: route_request, err: e }
            })?;

            // clone the source as additional context in case of an error
            let capability_source = route_source.source.clone();

            Ok(OpenRequest::new_from_route_source(route_source, target, open_options)
                .open()
                .await
                .map_err(|e| RouteAndOpenCapabilityError::OpenError {
                    source: capability_source,
                    err: e,
                })?)
        }
    }
}

/// Create a new `RouteRequest` from a `UseDecl`, checking that the capability type can
/// be installed in a namespace.
pub fn request_for_namespace_capability_use(use_decl: UseDecl) -> Option<RouteRequest> {
    match use_decl {
        UseDecl::Directory(decl) => Some(RouteRequest::UseDirectory(decl)),
        UseDecl::Protocol(decl) => Some(RouteRequest::UseProtocol(decl)),
        UseDecl::Service(decl) => Some(RouteRequest::UseService(decl)),
        UseDecl::Storage(decl) => Some(RouteRequest::UseStorage(decl)),
        _ => None,
    }
}

/// Create a new `RouteRequest` from an `ExposeDecl`, checking that the capability type can
/// be installed in a namespace.
///
/// REQUIRES: `exposes` is nonempty.
/// REQUIRES: `exposes` share the same type and target name.
/// REQUIRES: `exposes.len() > 1` only if it is a service.
pub fn request_for_namespace_capability_expose(exposes: Vec<&ExposeDecl>) -> Option<RouteRequest> {
    let first_expose = exposes.first().expect("invalid empty expose list");
    let first_type_name = CapabilityTypeName::from(*first_expose);
    assert!(
        exposes.iter().all(|e| {
            let type_name: CapabilityTypeName = CapabilityTypeName::from(*e);
            first_type_name == type_name && first_expose.target_name() == e.target_name()
        }),
        "invalid expose input: {:?}",
        exposes
    );
    match first_expose {
        cm_rust::ExposeDecl::Protocol(e) => {
            assert!(exposes.len() == 1, "multiple exposes");
            Some(RouteRequest::ExposeProtocol(e.clone()))
        }
        cm_rust::ExposeDecl::Service(_) => {
            // Gather the exposes into a bundle. Services can aggregate, in which case
            // multiple expose declarations map to one expose directory entry.
            let exposes: Vec<_> = exposes
                .into_iter()
                .filter_map(|e| match e {
                    cm_rust::ExposeDecl::Service(e) => Some(e.clone()),
                    _ => None,
                })
                .collect();
            Some(RouteRequest::ExposeService(RouteBundle::from_exposes(exposes)))
        }
        cm_rust::ExposeDecl::Directory(e) => {
            assert!(exposes.len() == 1, "multiple exposes");
            Some(RouteRequest::ExposeDirectory(e.clone()))
        }
        cm_rust::ExposeDecl::Runner(_) | cm_rust::ExposeDecl::Resolver(_) => {
            // Runners, resolvers, and event streams do not add directory entries.
            None
        }
    }
}

/// Routes a storage capability from `target` to its source and deletes its isolated storage.
pub(super) async fn route_and_delete_storage(
    use_storage_decl: UseStorageDecl,
    target: &Arc<ComponentInstance>,
) -> Result<(), ModelError> {
    let storage_source = RouteRequest::UseStorage(use_storage_decl.clone()).route(target).await?;

    let backing_dir_info = storage::route_backing_directory(storage_source.source).await?;

    // As of today, the storage component instance must contain the target. This is because
    // it is impossible to expose storage declarations up.
    let relative_moniker = InstancedRelativeMoniker::scope_down(
        &backing_dir_info.storage_source_moniker,
        &target.instanced_moniker(),
    )
    .unwrap();
    storage::delete_isolated_storage(
        backing_dir_info,
        target.persistent_storage,
        relative_moniker,
        target.instance_id().as_ref(),
    )
    .await
}

static ROUTE_ERROR_HELP: &'static str = "To learn more, see \
https://fuchsia.dev/go/components/connect-errors";

/// Sets an epitaph on `server_end` for a capability routing failure, and logs the error. Logs a
/// failure to route a capability. Formats `err` as a `String`, but elides the type if the error is
/// a `RoutingError`, the common case.
pub async fn report_routing_failure(
    target: &Arc<ComponentInstance>,
    cap: &ComponentCapability,
    err: ModelError,
    server_end: zx::Channel,
) {
    server_end
        .close_with_epitaph(err.as_zx_status())
        .unwrap_or_else(|error| debug!(%error, "failed to send epitaph"));
    let err_str = match &err {
        ModelError::RoutingError { err } => err.to_string(),
        _ => err.to_string(),
    };
    target
        .with_logger_as_default(|| {
            match err {
                ModelError::RouteAndOpenCapabilityError {
                    err:
                        RouteAndOpenCapabilityError::RoutingError {
                            err:
                                RoutingError::AvailabilityRoutingError(
                                    AvailabilityRoutingError::RouteFromVoidToOptionalTarget,
                                ),
                            ..
                        },
                } => {
                    // If the route failed because the capability is
                    // intentionally not provided, then this failure is expected
                    // and the warn level is unwarranted, so use the debug level
                    // in this case.
                    debug!(
                        "Optional {} `{}` was not available for target component `{}`: {}\n{}",
                        cap.type_name(),
                        cap.source_id(),
                        &target.abs_moniker,
                        &err_str,
                        ROUTE_ERROR_HELP
                    );
                }
                ModelError::RouteAndOpenCapabilityError {
                    err:
                        RouteAndOpenCapabilityError::RoutingError {
                            err:
                                RoutingError::AvailabilityRoutingError(
                                    AvailabilityRoutingError::FailedToRouteToOptionalTarget {
                                        ..
                                    },
                                ),
                            ..
                        },
                } => {
                    // If the target declared the capability as optional, but
                    // the capability could not be routed (such as if the source
                    // component is not available) the component _should_
                    // tolerate the missing optional capability. However, this
                    // should be logged. Developers are encouraged to change how
                    // they build and/or assemble different product
                    // configurations so declared routes are always end-to-end
                    // complete routes.
                    // TODO(fxbug.dev/109112): if we change the log for
                    // `Required` capabilities to `error!()`, consider also
                    // changing this log for `Optional` to `warn!()`.
                    info!(
                        "Optional {} `{}` was not available for target component `{}`: {}\n{}",
                        cap.type_name(),
                        cap.source_id(),
                        &target.abs_moniker,
                        &err_str,
                        ROUTE_ERROR_HELP
                    );
                }
                _ => {
                    // TODO(fxbug.dev/109112): consider changing this to `error!()`
                    warn!(
                        "Required {} `{}` was not available for target component `{}`: {}\n{}",
                        cap.type_name(),
                        cap.source_id(),
                        &target.abs_moniker,
                        &err_str,
                        ROUTE_ERROR_HELP
                    );
                }
            }
        })
        .await
}

/// Group exposes by `target_name`. This will group all exposes that form an aggregate capability
/// together.
pub fn aggregate_exposes(exposes: &Vec<ExposeDecl>) -> BTreeMap<&str, Vec<&ExposeDecl>> {
    let mut out: BTreeMap<&str, Vec<&ExposeDecl>> = BTreeMap::new();
    for expose in exposes {
        out.entry(expose.target_name().str()).or_insert(vec![]).push(expose);
    }
    out
}
