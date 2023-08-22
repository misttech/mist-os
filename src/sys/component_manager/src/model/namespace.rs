// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        constants::PKG_PATH,
        model::{
            component::{ComponentInstance, Package, WeakComponentInstance},
            error::CreateNamespaceError,
            routing::{self, route_and_open_capability, OpenOptions},
        },
        runner::{namespace::Entry as NamespaceEntry, Namespace},
    },
    ::routing::{
        capability_source::ComponentCapability, component_instance::ComponentInstanceInterface,
        mapper::NoopRouteMapper, rights::Rights, route_to_storage_decl,
        verify_instance_in_component_id_index, RouteRequest,
    },
    cm_rust::{self, ComponentDecl, UseDecl},
    fidl::{
        endpoints::{create_endpoints, ClientEnd, ServerEnd},
        prelude::*,
    },
    fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::{collections::HashMap, sync::Arc},
    tracing::{error, warn},
    vfs::{
        directory::entry::DirectoryEntry, directory::helper::DirectlyMutable,
        directory::immutable::simple as pfs, execution_scope::ExecutionScope, path::Path,
        remote::remote,
    },
};

type Directory = Arc<pfs::Simple>;

/// Creates a component's namespace.
pub async fn create_namespace(
    package: Option<&Package>,
    component: &Arc<ComponentInstance>,
    decl: &ComponentDecl,
    additional_entries: Vec<NamespaceEntry>,
) -> Result<Namespace, CreateNamespaceError> {
    let mut namespace = Namespace::new();
    if let Some(package) = package {
        let pkg_dir = fuchsia_fs::directory::clone_no_describe(&package.package_dir, None)
            .map_err(CreateNamespaceError::ClonePkgDirFailed)?;
        add_pkg_directory(&mut namespace, pkg_dir);
    }
    add_use_decls(&mut namespace, component, decl).await?;
    namespace.merge(additional_entries).map_err(CreateNamespaceError::InvalidAdditionalEntries)?;
    Ok(namespace)
}

/// Adds the package directory to the namespace under the path "/pkg".
fn add_pkg_directory(namespace: &mut Namespace, pkg_dir: fio::DirectoryProxy) {
    // TODO(https://fxbug.dev/108786): Use Proxy::into_client_end when available.
    let client_end = ClientEnd::new(pkg_dir.into_channel().unwrap().into_zx_channel());
    namespace.add(PKG_PATH.to_str().unwrap().to_string(), client_end);
}

/// Adds namespace entries for a component's use declarations.
///
/// This also serves all service directories.
async fn add_use_decls(
    namespace: &mut Namespace,
    component: &Arc<ComponentInstance>,
    decl: &ComponentDecl,
) -> Result<(), CreateNamespaceError> {
    // Populate the namespace from uses, using the component manager's namespace.
    // svc_dirs will hold (path,directory) pairs. Each pair holds a path in the
    // component's namespace and a directory that ComponentMgr will host for the component.
    let mut svc_dirs = HashMap::new();

    // directory_waiters will hold Future<Output=()> objects that will wait for activity on
    // a channel and then route the channel to the appropriate component's out directory.
    let mut directory_waiters = Vec::new();

    for use_ in &decl.uses {
        match use_ {
            cm_rust::UseDecl::Directory(_) => {
                add_directory_helper(namespace, &mut directory_waiters, &use_, component.as_weak());
            }
            cm_rust::UseDecl::Protocol(s) => {
                add_service_or_protocol_use(
                    &mut svc_dirs,
                    UseDecl::Protocol(s.clone()),
                    &s.target_path,
                    component.as_weak(),
                );
            }
            cm_rust::UseDecl::Service(s) => {
                add_service_or_protocol_use(
                    &mut svc_dirs,
                    UseDecl::Service(s.clone()),
                    &s.target_path,
                    component.as_weak(),
                );
            }
            cm_rust::UseDecl::Storage(_) => {
                add_storage_use(namespace, &mut directory_waiters, &use_, component).await?;
            }
            cm_rust::UseDecl::EventStream(s) => {
                add_service_or_protocol_use(
                    &mut svc_dirs,
                    UseDecl::EventStream(s.clone()),
                    &s.target_path,
                    component.as_weak(),
                );
            }
        }
    }

    // Start hosting the services directories and add them to the namespace
    serve_and_add_svc_dirs(namespace, svc_dirs);

    // The directory waiter will run in the component's nonblocking task scope, but
    // when it gets a readable signal it will spawn the routing task in the blocking scope as
    // that it blocks destruction, like service and protocol routing.
    //
    // TODO(fxbug.dev/76579): It would probably be more correct to run this in an execution_scope
    // attached to the namespace (but that requires a bigger refactor)
    let task_scope = component.nonblocking_task_scope();
    for waiter in directory_waiters {
        // The future for a directory waiter will only terminate once the directory channel is
        // first used. Run the future in a task bound to the component's scope instead of
        // calling await on it directly.
        task_scope.add_task(waiter).await;
    }

    Ok(())
}

/// Adds a directory waiter to `waiters` and updates `ns` to contain a handle for the
/// storage described by `use_`. Once the channel is readable, the future calls
/// `route_storage` to forward the channel to the source component's outgoing directory and
/// terminates.
async fn add_storage_use(
    namespace: &mut Namespace,
    waiters: &mut Vec<BoxFuture<'_, ()>>,
    use_: &UseDecl,
    component: &Arc<ComponentInstance>,
) -> Result<(), CreateNamespaceError> {
    // Prevent component from using storage capability if it is restricted to the component ID
    // index, and the component isn't in the index.
    match use_ {
        UseDecl::Storage(use_storage_decl) => {
            // To check that the storage capability is restricted to the storage decl, we have
            // to resolve the storage source capability. Because storage capabilities are only
            // ever `offer`d down the component tree, and we always resolve parents before
            // children, this resolution will walk the cache-happy path.
            // TODO(dgonyeo): Eventually combine this logic with the general-purpose startup
            // capability check.
            if let Ok(source) =
                route_to_storage_decl(use_storage_decl.clone(), &component, &mut NoopRouteMapper)
                    .await
            {
                verify_instance_in_component_id_index(&source, &component)
                    .map_err(CreateNamespaceError::InstanceNotInInstanceIdIndex)?;
            }
        }
        _ => unreachable!("unexpected storage decl"),
    }

    add_directory_helper(namespace, waiters, use_, component.as_weak());

    Ok(())
}

/// Adds a directory waiter to `waiters` and updates `ns` to contain a handle for the
/// directory described by `use_`. Once the channel is readable, the future calls
/// `route_directory` to forward the channel to the source component's outgoing directory and
/// terminates.
///
/// `component` is a weak pointer, which is important because we don't want the directory waiter
/// closure to hold a strong pointer to this component lest it create a reference cycle.
fn add_directory_helper(
    namespace: &mut Namespace,
    waiters: &mut Vec<BoxFuture<'_, ()>>,
    use_: &UseDecl,
    component: WeakComponentInstance,
) {
    let target_path =
        use_.path().expect("use decl without path used in add_directory_helper").to_string();
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
    let (client_end, server_end) = create_endpoints();
    let route_on_usage = async move {
        // Wait for the channel to become readable.
        let server_end = fasync::Channel::from_channel(server_end.into_channel())
            .expect("failed to convert server_end into async channel");
        let on_signal_fut = fasync::OnSignals::new(&server_end, zx::Signals::CHANNEL_READABLE);
        on_signal_fut.await.unwrap();
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
        let server_end = server_end.into_zx_channel();
        // Spawn a separate task to perform routing in the blocking scope. This way it won't
        // block namespace teardown, but it will block component destruction.
        target
            .blocking_task_scope()
            .add_task(route_directory(target, use_, flags, server_end))
            .await;
    };

    waiters.push(Box::pin(route_on_usage));
    namespace.add(target_path.clone(), client_end);
}

async fn route_directory(
    target: Arc<ComponentInstance>,
    use_: UseDecl,
    flags: fio::OpenFlags,
    mut server_end: zx::Channel,
) {
    let (route_request, open_options) = match &use_ {
        UseDecl::Directory(use_dir_decl) => (
            RouteRequest::UseDirectory(use_dir_decl.clone()),
            OpenOptions {
                flags: flags | fio::OpenFlags::DIRECTORY,
                relative_path: String::new(),
                server_chan: &mut server_end,
            },
        ),
        UseDecl::Storage(use_storage_decl) => (
            RouteRequest::UseStorage(use_storage_decl.clone()),
            OpenOptions {
                flags: flags | fio::OpenFlags::DIRECTORY,
                relative_path: ".".into(),
                server_chan: &mut server_end,
            },
        ),
        _ => panic!("not a directory or storage capability"),
    };
    if let Err(e) = route_and_open_capability(route_request, &target, open_options).await {
        routing::report_routing_failure(
            &target,
            &ComponentCapability::Use(use_),
            e.into(),
            server_end,
        )
        .await;
    }
}

/// Adds a service broker in `svc_dirs` for service described by `use_`. The service will be
/// proxied to the outgoing directory of the source component.
///
/// `component` is a weak pointer, which is important because we don't want the VFS
/// closure to hold a strong pointer to this component lest it create a reference cycle.
fn add_service_or_protocol_use(
    svc_dirs: &mut HashMap<String, Directory>,
    use_: UseDecl,
    capability_path: &cm_types::Path,
    component: WeakComponentInstance,
) {
    let not_found_component_copy = component.clone();
    let use_clone = use_.clone();
    let route_open_fn = move |scope: ExecutionScope,
                              flags: fio::OpenFlags,
                              relative_path: Path,
                              server_end: ServerEnd<fio::NodeMarker>| {
        let use_ = use_.clone();
        let component = component.clone();
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
            let mut server_end = server_end.into_channel();
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
                    UseDecl::Protocol(use_protocol_decl) => {
                        (RouteRequest::UseProtocol(use_protocol_decl.clone()),
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
                routing::route_and_open_capability(route_request, &target, open_options).await;
            if let Err(e) = res {
                routing::report_routing_failure(
                    &target,
                    &ComponentCapability::Use(use_),
                    e.into(),
                    server_end,
                )
                .await;
            }
        };
        scope.spawn(async move { component.blocking_task_scope().add_task(task).await })
    };

    let service_dir =
        svc_dirs.entry(capability_path.dirname().clone().into()).or_insert_with(|| {
            make_dir_with_not_found_logging(
                capability_path.dirname().clone().into(),
                not_found_component_copy,
            )
        });
    // NOTE: UseEventStream is special, in that we can route a single stream from multiple
    // sources (merging them).
    if matches!(use_clone, UseDecl::EventStream(_)) {
        // Ignore duplication error if already exists
        service_dir.clone().add_entry(capability_path.basename(), remote(route_open_fn)).ok();
    } else {
        service_dir
            .clone()
            .add_entry(capability_path.basename(), remote(route_open_fn))
            .expect("could not add service to directory");
    }
}

/// Serves the pseudo-directories in `svc_dirs` and adds their client ends to the namespace.
fn serve_and_add_svc_dirs(namespace: &mut Namespace, svc_dirs: HashMap<String, Directory>) {
    for (target_dir_path, pseudo_dir) in svc_dirs {
        let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
        pseudo_dir.clone().open(
            ExecutionScope::new(),
            // TODO(https://fxbug.dev/129636): Remove RIGHT_READABLE when `opendir` no longer
            // requires READABLE.
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            server_end.into_channel().into(),
        );

        namespace.add(target_dir_path, client_end);
    }
}

fn make_dir_with_not_found_logging(
    root_path: String,
    component_for_logger: WeakComponentInstance,
) -> Arc<pfs::Simple> {
    let new_dir = pfs::simple();
    // Grab a copy of the directory path, it will be needed if we log a
    // failed open request.
    new_dir.clone().set_not_found_handler(Box::new(move |path| {
        // Clone the component pointer and pass the copy into the logger.
        let component_for_logger = component_for_logger.clone();
        let requested_path = format!("{}/{}", root_path, path);

        // Spawn a task which logs the error. It would be nicer to not
        // spawn a task, but locking the component is async and this
        // closure is not.
        fasync::Task::spawn(async move {
            match component_for_logger.upgrade() {
                Ok(target) => {
                    target
                        .with_logger_as_default(|| {
                            warn!(
                                "No capability available at path {} for component {}, \
                                verify the component has the proper `use` declaration.",
                                requested_path, target.moniker
                            );
                        })
                        .await;
                }
                Err(_) => {}
            }
        })
        .detach();
    }));
    new_dir
}
