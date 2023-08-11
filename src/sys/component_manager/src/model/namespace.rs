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
    },
    ::routing::{
        capability_source::ComponentCapability, component_instance::ComponentInstanceInterface,
        mapper::NoopRouteMapper, rights::Rights, route_to_storage_decl,
        verify_instance_in_component_id_index, RouteRequest,
    },
    cm_logger::scoped::ScopedLogger,
    cm_runner::{namespace::Entry as NamespaceEntry, Namespace},
    cm_rust::{self, ComponentDecl, UseDecl, UseProtocolDecl},
    fidl::{
        endpoints::{create_endpoints, ClientEnd, ServerEnd},
        prelude::*,
    },
    fidl_fuchsia_io as fio,
    fidl_fuchsia_logger::LogSinkMarker,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::{collections::HashMap, sync::Arc},
    tracing::{error, info, warn},
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
) -> Result<(Namespace, Option<ScopedLogger>), CreateNamespaceError> {
    let mut namespace = Namespace::new();
    if let Some(package) = package {
        let pkg_dir = fuchsia_fs::directory::clone_no_describe(&package.package_dir, None)
            .map_err(CreateNamespaceError::ClonePkgDirFailed)?;
        add_pkg_directory(&mut namespace, pkg_dir);
    }
    let logger = add_use_decls(&mut namespace, component, decl).await?;
    namespace.merge(additional_entries).map_err(CreateNamespaceError::InvalidAdditionalEntries)?;
    Ok((namespace, logger))
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
) -> Result<Option<ScopedLogger>, CreateNamespaceError> {
    // Populate the namespace from uses, using the component manager's namespace.
    // svc_dirs will hold (path,directory) pairs. Each pair holds a path in the
    // component's namespace and a directory that ComponentMgr will host for the component.
    let mut svc_dirs = HashMap::new();

    // directory_waiters will hold Future<Output=()> objects that will wait for activity on
    // a channel and then route the channel to the appropriate component's out directory.
    let mut directory_waiters = Vec::new();

    let mut log_sink_decl: Option<UseProtocolDecl> = None;
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
                if s.source_name == LogSinkMarker::PROTOCOL_NAME {
                    log_sink_decl = Some(s.clone());
                }
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

    let logger = log_sink_decl
        .map(|decl| {
            let ns = std::mem::replace(&mut namespace.entries, Vec::new());
            let (ns_, logger) = get_logger_from_ns(ns, &decl);
            let _ = std::mem::replace(&mut namespace.entries, ns_);
            logger
        })
        .flatten();

    Ok(logger)
}

/// Given the set of namespace entries and a LogSink protocol's
/// `UseProtocolDecl`, look through the namespace for where to connect
/// to the LogSink protocol. The log connection, if any, is stored in the
/// IncomingNamespace.
fn get_logger_from_ns(
    ns: Vec<NamespaceEntry>,
    log_sink_decl: &UseProtocolDecl,
) -> (Vec<NamespaceEntry>, Option<ScopedLogger>) {
    // A new set of namespace entries is returned because when the entry
    // used to connect to LogSink is found, that entry is consumed. A
    // matching entry is created and placed in the set of entries returned
    // by this function. `self` is taken as mutable so the
    // logger connection can be stored when found.
    let mut new_ns = vec![];
    let mut log_entry: Option<(NamespaceEntry, String)> = None;
    let mut logger = None;
    // Find namespace directory specified in the log_sink_decl
    for entry in ns {
        // Check if this namespace path is a stem of the decl's path
        if log_entry.is_none() {
            if let Ok(path_remainder) =
                is_subpath_of(log_sink_decl.target_path.to_string(), entry.path.to_string())
            {
                log_entry = Some((entry, path_remainder));
                continue;
            }
        }
        new_ns.push(entry);
    }

    // If we found a matching namespace entry, try to open the log proxy
    if let Some((mut entry, remaining_path)) = log_entry {
        let _str = log_sink_decl.target_path.to_string();
        let (restored_dir, logger_) = get_logger_from_dir(entry.directory, &remaining_path);
        entry.directory = restored_dir;
        logger = logger_;
        new_ns.push(entry);
    }
    (new_ns, logger)
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

/// Determines if the `full` is a subpath of the `stem`. Returns the
/// remaining portion of the path if `full` us a subpath. Returns Error if
/// `stem` and `full` are the same.
fn is_subpath_of(full: String, stem: String) -> Result<String, ()> {
    let stem_path = std::path::Path::new(&stem);
    let full_path = std::path::Path::new(&full);

    let remainder = full_path
        .strip_prefix(stem_path)
        // Unwrapping the `to_str` conversion should be safe here since we
        // started with a Unicode value, put it into a path and now are
        // extracting a portion of that value.
        .map(|path| path.to_str().unwrap().to_string())
        .map_err(|_| ())?;

    if remainder.is_empty() {
        Err(())
    } else {
        Ok(remainder)
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

/// Given a Directory, connect to the LogSink protocol at the default
/// location.
fn get_logger_from_dir(
    dir: ClientEnd<fio::DirectoryMarker>,
    at_path: &str,
) -> (ClientEnd<fio::DirectoryMarker>, Option<ScopedLogger>) {
    let mut logger = None;
    match dir.into_proxy() {
        Ok(dir_proxy) => {
            match ScopedLogger::from_directory(&dir_proxy, at_path) {
                Ok(ns_logger) => {
                    logger = Some(ns_logger);
                }
                Err(error) => {
                    info!(%error, "LogSink.Connect() failed, logs will be attributed to component manager");
                }
            }

            // Now that we have the LogSink and socket, put the LogSink
            // protocol directory back where we found it.
            (
                dir_proxy.into_channel().map_or_else(
                    |error| {
                        error!(?error, "LogSink proxy could not be converted back to channel");
                        let (client, _server) = create_endpoints::<fio::DirectoryMarker>();
                        client
                    },
                    |chan| ClientEnd::<fio::DirectoryMarker>::new(chan.into()),
                ),
                logger,
            )
        }
        Err(error) => {
            info!(%error, "Directory client channel could not be turned into proxy");
            let (client, _server) = create_endpoints::<fio::DirectoryMarker>();
            (client, logger)
        }
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

#[cfg(test)]
pub mod test {
    use {
        super::*,
        crate::model::testing::test_helpers::MockServiceRequest,
        cm_rust::{Availability, DependencyType, UseProtocolDecl, UseSource},
        fidl::endpoints,
        fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest},
        fuchsia_component::server::ServiceFs,
        futures::StreamExt,
        std::sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        },
    };

    #[fuchsia::test]
    fn test_subpath_handling() {
        let mut stem = "/".to_string();
        let mut full = "/".to_string();
        assert_eq!(is_subpath_of(full, stem), Err(()));

        stem = "/".to_string();
        full = "/subdir".to_string();
        assert_eq!(is_subpath_of(full, stem), Ok("subdir".to_string()));

        stem = "/subdir1/subdir2".to_string();
        full = "/subdir1/file.txt".to_string();
        assert_eq!(is_subpath_of(full, stem), Err(()));

        stem = "/this/path/has/a/typ0".to_string();
        full = "/this/path/has/a/typo/not/exclamation/point".to_string();
        assert_eq!(is_subpath_of(full, stem), Err(()));

        stem = "/subdir1".to_string();
        full = "/subdir1/subdir2/subdir3/file.txt".to_string();
        assert_eq!(is_subpath_of(full, stem), Ok("subdir2/subdir3/file.txt".to_string()));
    }

    #[fuchsia::test]
    /// Tests that the logger is connected to when it is in a subdirectory of a
    /// namespace entry.
    async fn test_logger_at_root_of_entry() {
        let log_decl = UseProtocolDecl {
            source: UseSource::Parent,
            source_name: "logsink".parse().unwrap(),
            target_path: "/fuchsia.logger.LogSink".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        };

        let (dir_client, dir_server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut root_dir = ServiceFs::new_local();
        root_dir.add_fidl_service_at(LogSinkMarker::PROTOCOL_NAME, MockServiceRequest::LogSink);
        let _sub_dir = root_dir.dir("subdir");
        root_dir.serve_connection(dir_server).expect("failed to add serving channel");

        let ns_entries = vec![NamespaceEntry { path: "/".to_string(), directory: dir_client }];

        verify_logger_connects_in_namespace(Some(&mut root_dir), ns_entries, log_decl, true).await;
    }

    #[fuchsia::test]
    /// Tests that the logger is connected to when it is in a subdirectory of a
    /// namespace entry.
    async fn test_logger_at_subdir_of_entry() {
        let log_decl = UseProtocolDecl {
            source: UseSource::Parent,
            source_name: "logsink".parse().unwrap(),
            target_path: "/arbitrary-dir/fuchsia.logger.LogSink".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        };

        let (dir_client, dir_server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut root_dir = ServiceFs::new_local();
        let mut svc_dir = root_dir.dir("arbitrary-dir");
        svc_dir.add_fidl_service_at(LogSinkMarker::PROTOCOL_NAME, MockServiceRequest::LogSink);
        let _sub_dir = root_dir.dir("subdir");
        root_dir.serve_connection(dir_server).expect("failed to add serving channel");

        let ns_entries = vec![NamespaceEntry { path: "/".to_string(), directory: dir_client }];

        verify_logger_connects_in_namespace(Some(&mut root_dir), ns_entries, log_decl, true).await;
    }

    #[fuchsia::test]
    async fn test_multiple_namespace_entries() {
        let log_decl = UseProtocolDecl {
            source: UseSource::Parent,
            source_name: "logsink".parse().unwrap(),
            target_path: "/svc/fuchsia.logger.LogSink".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        };

        let (dir_client, dir_server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut root_dir = ServiceFs::new_local();
        root_dir.add_fidl_service_at(LogSinkMarker::PROTOCOL_NAME, MockServiceRequest::LogSink);
        let _sub_dir = root_dir.dir("subdir");
        root_dir.serve_connection(dir_server).expect("failed to add serving channel");

        // Create a directory for another namespace entry which we don't
        // actually expect to be accessed.
        let (extra_dir_client, extra_dir_server) =
            endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut extra_dir = ServiceFs::new_local();
        extra_dir.add_fidl_service(MockServiceRequest::LogSink);
        extra_dir.serve_connection(extra_dir_server).expect("serving channel failed");

        let ns_entries = vec![
            NamespaceEntry { path: "/svc".to_string(), directory: dir_client },
            NamespaceEntry { path: "/sv".to_string(), directory: extra_dir_client },
        ];

        verify_logger_connects_in_namespace(Some(&mut root_dir), ns_entries, log_decl, true).await;
    }

    #[fuchsia::test]
    async fn test_no_connect_on_empty_namespace() {
        let log_decl = UseProtocolDecl {
            source: UseSource::Parent,
            source_name: "logsink".parse().unwrap(),
            target_path: "/svc/fuchsia.logger.LogSink".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        };

        let ns_entries = vec![];

        verify_logger_connects_in_namespace(
            Option::<
                &mut ServiceFs<fuchsia_component::server::ServiceObjLocal<'_, MockServiceRequest>>,
            >::None,
            ns_entries,
            log_decl,
            false,
        )
        .await;
    }

    #[fuchsia::test]
    async fn test_logsink_dir_not_in_namespace() {
        let log_decl = UseProtocolDecl {
            source: UseSource::Parent,
            source_name: "logsink".parse().unwrap(),
            target_path: "/svc/fuchsia.logger.LogSink".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        };

        let (dir_client, dir_server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut root_dir = ServiceFs::new_local();
        root_dir.add_fidl_service_at(LogSinkMarker::PROTOCOL_NAME, MockServiceRequest::LogSink);
        root_dir.serve_connection(dir_server).expect("failed to add serving channel");

        let ns_entries =
            vec![NamespaceEntry { path: "/not-the-svc-dir".to_string(), directory: dir_client }];

        verify_logger_connects_in_namespace(Some(&mut root_dir), ns_entries, log_decl, false).await;
    }

    /// Verify the expected logger connection behavior and that the logger is
    /// set or not in the namespace.
    async fn verify_logger_connects_in_namespace<
        T: fuchsia_component::server::ServiceObjTrait<Output = MockServiceRequest>,
    >(
        root_dir: Option<&mut ServiceFs<T>>,
        ns_entries: Vec<NamespaceEntry>,
        proto_decl: UseProtocolDecl,
        connects: bool,
    ) {
        let connection_count = if connects { 1u8 } else { 0u8 };

        // Create a task that will access the namespace by calling
        // `get_logger_from_ns`. This task won't complete until the VFS backing
        // the namespace starts responding to requests. This VFS is served by
        // code in the next stanza.
        fuchsia_async::Task::spawn(async move {
            let ns_size = ns_entries.len();
            let (procesed_ns, logger) = get_logger_from_ns(ns_entries, &proto_decl);
            assert_eq!(logger.is_some(), connects);
            assert_eq!(ns_size, procesed_ns.len());
        })
        .detach();

        if let Some(dir) = root_dir {
            // Serve the directory and when the LogSink service is requested
            // provide a closure that counts number of calls to the ConnectStructured and
            // WaitForInterestChange methods. Serving stops when the spawned task drops the
            // IncomingNamespace, which holds the other side of the VFS directory handle.
            let connect_count = Arc::new(AtomicU8::new(0));
            dir.for_each_concurrent(10usize, |request: MockServiceRequest| match request {
                MockServiceRequest::LogSink(mut r) => {
                    let connect_count2 = connect_count.clone();
                    async move {
                        while let Some(Ok(req)) = r.next().await {
                            match req {
                                LogSinkRequest::Connect { .. } => {
                                    panic!("Unexpected call to `Connect`");
                                }
                                LogSinkRequest::ConnectStructured { .. } => {
                                    connect_count2.fetch_add(1, Ordering::SeqCst);
                                }
                                LogSinkRequest::WaitForInterestChange { .. } => {
                                    // ideally we'd also assert this was received but it's racy
                                    // since the request is sent by the above detached task
                                }
                            }
                        }
                    }
                }
            })
            .await;
            assert_eq!(connect_count.load(Ordering::SeqCst), connection_count);
        }
    }
}
