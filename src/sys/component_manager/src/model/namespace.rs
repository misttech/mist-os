// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::constants::PKG_PATH;
use crate::model::component::{ComponentInstance, Package, WeakComponentInstance};
use crate::model::routing::legacy::RouteRequestExt;
use crate::model::routing::router_ext::RouterExt;
use crate::model::routing::{report_routing_failure, BedrockUseRouteRequest};
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::mapper::NoopRouteMapper;
use ::routing::{route_to_storage_decl, verify_instance_in_component_id_index, RouteRequest};
use cm_rust::{ComponentDecl, UseDecl, UseProtocolDecl};
use errors::CreateNamespaceError;
use fidl::endpoints::ClientEnd;
use fidl::prelude::*;
use fidl_fuchsia_io as fio;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::{FutureExt, StreamExt};
use router_error::RouterError;
use sandbox::{Capability, Dict, Open};
use serve_processargs::NamespaceBuilder;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::warn;
use vfs::execution_scope::ExecutionScope;

/// Creates a component's namespace.
///
/// TODO(b/298106231): eventually this should only build a delivery map as
/// the program dict will be fetched from the resolved component state.
pub async fn create_namespace(
    package: Option<&Package>,
    component: &Arc<ComponentInstance>,
    decl: &ComponentDecl,
    program_input_dict: &Dict,
    scope: ExecutionScope,
) -> Result<NamespaceBuilder, CreateNamespaceError> {
    let not_found_sender = not_found_logging(component);
    let mut namespace = NamespaceBuilder::new(scope.clone(), not_found_sender);
    if let Some(package) = package {
        let pkg_dir = fuchsia_fs::directory::clone_no_describe(&package.package_dir, None)
            .map_err(CreateNamespaceError::ClonePkgDirFailed)?;
        add_pkg_directory(&mut namespace, pkg_dir)?;
    }
    let uses = deduplicate_event_stream(decl.uses.iter());
    add_use_decls(&mut namespace, component, uses, program_input_dict).await?;
    Ok(namespace)
}

/// This function transforms a sequence of [`UseDecl`] such that the duplicate event stream
/// uses by paths are removed.
///
/// Different from all other use declarations, multiple event stream capabilities may be used
/// at the same path, the semantics being a single FIDL protocol capability is made available
/// at that path, subscribing to all the specified events:
/// see [`crate::model::events::registry::EventRegistry`].
fn deduplicate_event_stream<'a>(
    iter: std::slice::Iter<'a, UseDecl>,
) -> impl Iterator<Item = &'a UseDecl> {
    let mut paths = HashSet::new();
    iter.filter_map(move |use_decl| match use_decl {
        UseDecl::EventStream(ref event_stream) => {
            if !paths.insert(event_stream.target_path.clone()) {
                None
            } else {
                Some(use_decl)
            }
        }
        _ => Some(use_decl),
    })
}

/// Adds the package directory to the namespace under the path "/pkg".
fn add_pkg_directory(
    namespace: &mut NamespaceBuilder,
    pkg_dir: fio::DirectoryProxy,
) -> Result<(), CreateNamespaceError> {
    // TODO(https://fxbug.dev/42060182): Use Proxy::into_client_end when available.
    let client_end = ClientEnd::new(pkg_dir.into_channel().unwrap().into_zx_channel());
    let directory: sandbox::Directory = client_end.into();
    let path = cm_types::NamespacePath::new(PKG_PATH.to_str().unwrap()).unwrap();
    namespace.add_entry(Capability::Directory(directory), &path)?;
    Ok(())
}

/// Adds namespace entries for a component's use declarations.
async fn add_use_decls(
    namespace: &mut NamespaceBuilder,
    component: &Arc<ComponentInstance>,
    uses: impl Iterator<Item = &UseDecl>,
    program_input_dict: &Dict,
) -> Result<(), CreateNamespaceError> {
    for use_ in uses {
        if let cm_rust::UseDecl::Runner(_) = use_ {
            // The runner is not available in the namespace.
            continue;
        }
        if let cm_rust::UseDecl::Config(_) = use_ {
            // Configuration is not available in the namespace.
            continue;
        }

        // Prevent component from using storage capability if it is restricted to the component
        // ID index, and the component isn't in the index. To check that the storage capability
        // is restricted to the storage decl, we have to resolve the storage source capability.
        // Because storage capabilities are only ever `offer`d down the component tree, and we
        // always resolve parents before children, this resolution will walk the cache-happy
        // path.
        //
        // TODO: Eventually combine this logic with the general-purpose startup capability
        // check.
        if let cm_rust::UseDecl::Storage(decl) = use_ {
            if let Ok(source) =
                route_to_storage_decl(decl.clone(), component, &mut NoopRouteMapper).await
            {
                verify_instance_in_component_id_index(&source, component)
                    .map_err(CreateNamespaceError::InstanceNotInInstanceIdIndex)?;
            }
        }

        let target_path =
            use_.path().ok_or_else(|| CreateNamespaceError::UseDeclWithoutPath(use_.clone()))?;
        let capability: Capability = match use_ {
            // Bedrock
            cm_rust::UseDecl::Protocol(s) => {
                protocol_use(s.clone(), component, program_input_dict).into()
            }

            // Legacy
            use_ => {
                let request = RouteRequest::from(use_.clone());
                request.into_capability(component)
            }
        };
        match use_ {
            cm_rust::UseDecl::Directory(_) | cm_rust::UseDecl::Storage(_) => {
                namespace.add_entry(capability, &target_path.clone().into())
            }
            cm_rust::UseDecl::Protocol(_)
            | cm_rust::UseDecl::Service(_)
            | cm_rust::UseDecl::EventStream(_) => namespace.add_object(capability, target_path),
            cm_rust::UseDecl::Runner(_) => {
                std::process::abort();
            }
            cm_rust::UseDecl::Config(_) => {
                std::process::abort();
            }
        }?
    }

    Ok(())
}

/// Makes a capability for the service/protocol described by `use_`. The service will be
/// proxied to the outgoing directory of the source component.
///
/// `component` is a weak pointer, which is important because we don't want the VFS
/// closure to hold a strong pointer to this component lest it create a reference cycle.
fn protocol_use(
    decl: UseProtocolDecl,
    component: &Arc<ComponentInstance>,
    program_input_dict: &Dict,
) -> Open {
    let (router, request) = BedrockUseRouteRequest::UseProtocol(decl.clone()).into_router(
        component.as_weak(),
        program_input_dict,
        false,
    );

    // When there are router errors, they are sent to the error handler, which reports
    // errors.
    let weak_target = component.as_weak();
    let legacy_request = RouteRequest::UseProtocol(decl);
    Open::new(router.into_directory_entry(
        request,
        fio::DirentType::Service,
        component.execution_scope.clone(),
        move |error: &RouterError| {
            let Ok(target) = weak_target.upgrade() else {
                return None;
            };
            let legacy_request = legacy_request.clone();
            Some(
                async move { report_routing_failure(&legacy_request, &target, error).await }
                    .boxed(),
            )
        },
    ))
}

fn not_found_logging(component: &Arc<ComponentInstance>) -> UnboundedSender<String> {
    let (sender, mut receiver) = unbounded();
    let component_for_logger: WeakComponentInstance = component.as_weak();

    component.nonblocking_task_group().spawn(async move {
        while let Some(path) = receiver.next().await {
            match component_for_logger.upgrade() {
                Ok(target) => {
                    target
                        .with_logger_as_default(|| {
                            warn!(
                                "No capability available at path {} for component {}, \
                                verify the component has the proper `use` declaration.",
                                path, target.moniker
                            );
                        })
                        .await;
                }
                Err(_) => {}
            }
        }
    });

    sender
}
