// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::error::BedrockErrorIsAuthoritative,
    crate::{
        constants::PKG_PATH,
        model::{
            component::{ComponentInstance, Package, WeakComponentInstance},
            error::CreateNamespaceError,
            routing::{
                self, route_and_open_capability,
                router::{ErrorCapsule, Request, Router},
                OpenOptions,
            },
        },
    },
    ::routing::{
        component_instance::ComponentInstanceInterface, mapper::NoopRouteMapper, rights::Rights,
        route_to_storage_decl, verify_instance_in_component_id_index, RouteRequest,
    },
    cm_rust::{self, ComponentDecl, UseDecl, UseStorageDecl},
    cm_types::IterablePath,
    cm_util::TaskGroup,
    fidl::{endpoints::ClientEnd, prelude::*},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        StreamExt,
    },
    sandbox::{AnyCapability, Dict, Directory, Open},
    serve_processargs::NamespaceBuilder,
    std::{collections::HashSet, sync::Arc},
    tracing::{error, warn},
    vfs::{execution_scope::ExecutionScope, path::Path},
};

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
    add_use_decls(&mut namespace, component, uses, program_input_dict, scope).await?;
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
    let path = cm_types::Path::new(PKG_PATH.to_str().unwrap()).unwrap();
    namespace.add_entry(Box::new(directory), path.as_ref())?;
    Ok(())
}

/// Adds namespace entries for a component's use declarations.
async fn add_use_decls(
    namespace: &mut NamespaceBuilder,
    component: &Arc<ComponentInstance>,
    uses: impl Iterator<Item = &UseDecl>,
    program_input_dict: &Dict,
    scope: ExecutionScope,
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

        let target_path =
            use_.path().ok_or_else(|| CreateNamespaceError::UseDeclWithoutPath(use_.clone()))?;
        let capability: AnyCapability = match use_ {
            cm_rust::UseDecl::Directory(_) => {
                directory_use(&use_, component.as_weak(), scope.clone())
            }
            cm_rust::UseDecl::Storage(storage) => {
                storage_use(storage, use_, component, scope.clone()).await?
            }
            cm_rust::UseDecl::Protocol(s) => service_or_protocol_use(
                UseDecl::Protocol(s.clone()),
                component.as_weak(),
                program_input_dict,
                component.blocking_task_group(),
            ),
            cm_rust::UseDecl::Service(s) => service_or_protocol_use(
                UseDecl::Service(s.clone()),
                component.as_weak(),
                program_input_dict,
                component.blocking_task_group(),
            ),
            cm_rust::UseDecl::EventStream(s) => service_or_protocol_use(
                UseDecl::EventStream(s.clone()),
                component.as_weak(),
                program_input_dict,
                component.blocking_task_group(),
            ),
            cm_rust::UseDecl::Runner(_) => {
                std::process::abort();
            }
            cm_rust::UseDecl::Config(_) => {
                std::process::abort();
            }
        };
        match use_ {
            cm_rust::UseDecl::Directory(_) | cm_rust::UseDecl::Storage(_) => {
                namespace.add_entry(capability, target_path.as_ref())
            }
            cm_rust::UseDecl::Protocol(_)
            | cm_rust::UseDecl::Service(_)
            | cm_rust::UseDecl::EventStream(_) => {
                namespace.add_object(capability, target_path.as_ref())
            }
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

/// Makes a capability representing the storage described by `use_decl`. Once the channel
/// is readable, the future calls `route_storage` to forward the channel to the source
/// component's outgoing directory and terminates.
async fn storage_use(
    use_storage_decl: &UseStorageDecl,
    use_decl: &UseDecl,
    component: &Arc<ComponentInstance>,
    scope: ExecutionScope,
) -> Result<Box<Directory>, CreateNamespaceError> {
    // Prevent component from using storage capability if it is restricted to the component ID
    // index, and the component isn't in the index.
    // To check that the storage capability is restricted to the storage decl, we have
    // to resolve the storage source capability. Because storage capabilities are only
    // ever `offer`d down the component tree, and we always resolve parents before
    // children, this resolution will walk the cache-happy path.
    // TODO(dgonyeo): Eventually combine this logic with the general-purpose startup
    // capability check.
    if let Ok(source) =
        route_to_storage_decl(use_storage_decl.clone(), &component, &mut NoopRouteMapper).await
    {
        verify_instance_in_component_id_index(&source, &component)
            .map_err(CreateNamespaceError::InstanceNotInInstanceIdIndex)?;
    }

    Ok(directory_use(use_decl, component.as_weak(), scope))
}

/// Makes a capability representing the directory described by `use_`. Once the
/// channel is readable, the future calls `route_directory` to forward the channel to the
/// source component's outgoing directory and terminates.
///
/// `component` is a weak pointer, which is important because we don't want the task
/// waiting for channel readability to hold a strong pointer to this component lest it
/// create a reference cycle.
fn directory_use(
    use_: &UseDecl,
    component: WeakComponentInstance,
    scope: ExecutionScope,
) -> Box<Directory> {
    let flags = match use_ {
        UseDecl::Directory(dir) => Rights::from(dir.rights).into_legacy(),
        UseDecl::Storage(_) => fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        _ => panic!("not a directory or storage capability"),
    };

    // Specify that the capability must be opened as a directory. In particular, this affects
    // how a devfs-based capability will handle the open call. If this flag is not specified,
    // devfs attempts to open the directory as a service, which is not what is desired here.
    let flags = flags | fio::OpenFlags::DIRECTORY;

    let use_ = use_.clone();
    let open_fn = move |_scope: vfs::execution_scope::ExecutionScope,
                        flags: fio::OpenFlags,
                        relative_path: vfs::path::Path,
                        server_end: zx::Channel| {
        let target = match component.upgrade() {
            Ok(component) => component,
            Err(e) => {
                error!(
                    "failed to upgrade WeakComponentInstance routing use \
                        decl `{:?}`: {:?}",
                    &use_, e
                );
                return;
            }
        };
        // Spawn a separate task to perform routing in the blocking scope. This way it won't
        // block namespace teardown, but it will block component destruction.
        target.blocking_task_group().spawn(route_directory(
            target,
            use_.clone(),
            relative_path.into_string(),
            flags,
            server_end,
        ));
    };

    let open = Open::new(open_fn, fio::DirentType::Directory);
    Box::new(open.into_directory(flags, scope))
}

async fn route_directory(
    target: Arc<ComponentInstance>,
    use_: UseDecl,
    relative_path: String,
    flags: fio::OpenFlags,
    mut server_end: zx::Channel,
) {
    let (route_request, open_options) = match &use_ {
        UseDecl::Directory(use_dir_decl) => (
            RouteRequest::UseDirectory(use_dir_decl.clone()),
            OpenOptions {
                flags: flags | fio::OpenFlags::DIRECTORY,
                relative_path,
                server_chan: &mut server_end,
            },
        ),
        UseDecl::Storage(use_storage_decl) => (
            RouteRequest::UseStorage(use_storage_decl.clone()),
            OpenOptions {
                flags: flags | fio::OpenFlags::DIRECTORY,
                relative_path,
                server_chan: &mut server_end,
            },
        ),
        _ => panic!("not a directory or storage capability"),
    };
    if let Err(e) = route_and_open_capability(&route_request, &target, open_options).await {
        routing::report_routing_failure(&route_request, &target, e.into(), server_end).await;
    }
}

/// Makes a capability for the service/protocol described by `use_`. The service will be
/// proxied to the outgoing directory of the source component.
///
/// `component` is a weak pointer, which is important because we don't want the VFS
/// closure to hold a strong pointer to this component lest it create a reference cycle.
fn service_or_protocol_use(
    use_: UseDecl,
    component: WeakComponentInstance,
    program_input_dict: &Dict,
    blocking_task_group: TaskGroup,
) -> Box<Open> {
    // Bedrock routing.
    if let UseDecl::Protocol(use_protocol_decl) = &use_ {
        let request = Request {
            rights: None,
            relative_path: sandbox::Path::default(),
            availability: use_protocol_decl.availability.clone(),
            target: component.clone(),
        };
        let legacy_request = RouteRequest::UseProtocol(use_protocol_decl.clone());

        // When there are router errors, they are sent to the error handler,
        // which reports errors and may attempt routing again via legacy.
        let errors_fn = move |err: ErrorCapsule| {
            let legacy_request = legacy_request.clone();
            let Ok(target) = component.upgrade() else {
                return;
            };
            target.blocking_task_group().spawn(async move {
                let (error, mut open_request) = err.manually_handle();
                let open_options = OpenOptions {
                    flags: open_request.flags,
                    relative_path: open_request.relative_path.into_string(),
                    server_chan: &mut open_request.server_end,
                };
                if error.is_bedrock_error_authoritative() {
                    return routing::report_routing_failure(
                        &legacy_request,
                        &target,
                        error.clone().into(),
                        open_request.server_end,
                    )
                    .await;
                }

                // TODO(https://fxbug.dev/319546081): We can remove the fallback once bedrock
                // properly communicates routing errors with fidelity on par with legacy routing.
                if let Err(error) =
                    routing::route_and_open_capability(&legacy_request, &target, open_options).await
                {
                    routing::report_routing_failure(
                        &legacy_request,
                        &target,
                        error.into(),
                        open_request.server_end,
                    )
                    .await;
                }
            });
        };
        // TODO(https://fxbug.dev/324138478): We will be able to assert that the program input dict
        // must have the required capability if we always add a Router to the program input
        // dict for every protocol use declaration.
        let router = Router::from_routable(program_input_dict.clone())
            .with_path(use_protocol_decl.target_path.iter_segments());
        let open =
            router.into_open(request, fio::DirentType::Service, blocking_task_group, errors_fn);
        return Box::new(open);
    };

    // Legacy routing.
    let route_open_fn = move |_scope: ExecutionScope,
                              flags: fio::OpenFlags,
                              relative_path: Path,
                              mut server_end: zx::Channel| {
        let use_ = use_.clone();
        let component = match component.upgrade() {
            Ok(component) => component,
            Err(e) => {
                error!(
                    "failed to upgrade WeakComponentInstance routing use \
                            decl `{:?}`: {:?}",
                    &use_, e
                );
                return;
            }
        };
        let target = component.clone();
        let task = async move {
            let (route_request, open_options) = {
                match &use_ {
                    UseDecl::Service(use_service_decl) => {
                        (RouteRequest::UseService(use_service_decl.clone()),
                             OpenOptions{
                                 flags,
                                 relative_path: relative_path.into_string(),
                                 server_chan: &mut server_end
                             }
                         )
                    },
                    UseDecl::EventStream(stream)=> {
                        (RouteRequest::UseEventStream(stream.clone()),
                             OpenOptions{
                                 flags,
                                 relative_path: stream.target_path.to_string(),
                                 server_chan: &mut server_end,
                             }
                         )
                    },
                    _ => panic!("add_service_or_protocol_use called with non-service or protocol capability"),
                }
            };

            let res =
                routing::route_and_open_capability(&route_request, &target, open_options).await;
            if let Err(e) = res {
                routing::report_routing_failure(&route_request, &target, e.into(), server_end)
                    .await;
            }
        };
        component.blocking_task_group().spawn(task)
    };

    let open = Open::new(route_open_fn, fio::DirentType::Service);
    Box::new(open)
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
