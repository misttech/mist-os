// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource},
        model::{
            component::{ComponentInstance, ExtendedInstance, StartReason, WeakComponentInstance},
            error::ModelError,
            hooks::{Event, EventPayload},
            routing::{
                providers::{
                    DefaultComponentCapabilityProvider, DirectoryEntryCapabilityProvider,
                    NamespaceCapabilityProvider,
                },
                service::{
                    AggregateServiceDirectoryProvider, CollectionServiceDirectory,
                    CollectionServiceRoute, FilteredServiceProvider,
                },
                RouteRequest, RouteSource, RoutingError,
            },
            storage,
        },
    },
    ::routing::component_instance::ComponentInstanceInterface,
    cm_moniker::{InstancedExtendedMoniker, InstancedRelativeMoniker},
    cm_util::channel,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::lock::Mutex,
    moniker::RelativeMonikerBase,
    std::{path::PathBuf, sync::Arc},
};

/// Contains the options to use when opening a capability.
///
/// See [`fidl_fuchsia_io::Directory::Open`] for how the `flags`, `relative_path`,
/// and `server_chan` parameters are used in the open call.
pub enum OpenOptions<'a> {
    Directory(OpenDirectoryOptions<'a>),
    EventStream(OpenEventStreamOptions<'a>),
    Protocol(OpenProtocolOptions<'a>),
    Resolver(OpenResolverOptions<'a>),
    Runner(OpenRunnerOptions<'a>),
    Service(OpenServiceOptions<'a>),
    Storage(OpenStorageOptions<'a>),
}

impl<'a> OpenOptions<'a> {
    /// Creates an `OpenOptions` for a capability that can be installed in a namespace,
    /// or an error if `route_request` specifies a capability that cannot be installed
    /// in a namespace.
    pub fn for_namespace_capability(
        route_request: &RouteRequest,
        flags: fio::OpenFlags,
        relative_path: String,
        server_chan: &'a mut zx::Channel,
    ) -> Result<Self, ModelError> {
        match route_request {
            RouteRequest::UseDirectory(_) | RouteRequest::ExposeDirectory(_) => {
                Ok(Self::Directory(OpenDirectoryOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseProtocol(_) | RouteRequest::ExposeProtocol(_) => {
                Ok(Self::Protocol(OpenProtocolOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseService(_) | RouteRequest::ExposeService(_) => {
                Ok(Self::Service(OpenServiceOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseEventStream(_) => {
                Ok(Self::EventStream(OpenEventStreamOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseStorage(_) => {
                Ok(Self::Storage(OpenStorageOptions { flags, relative_path, server_chan }))
            }
            _ => Err(ModelError::unsupported("capability cannot be installed in a namespace")),
        }
    }
}

pub struct OpenDirectoryOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenProtocolOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenResolverOptions<'a> {
    pub flags: fio::OpenFlags,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenRunnerOptions<'a> {
    pub flags: fio::OpenFlags,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenServiceOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenStorageOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenEventStreamOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

/// A request to open a capability at its source.
pub enum OpenRequest<'a> {
    // Open a capability backed by a component's outgoing directory.
    OutgoingDirectory {
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        source: CapabilitySource,
        target: &'a Arc<ComponentInstance>,
        server_chan: &'a mut zx::Channel,
    },
    // Open a storage capability.
    Storage {
        flags: fio::OpenFlags,
        relative_path: String,
        source: storage::StorageCapabilitySource,
        target: &'a Arc<ComponentInstance>,
        server_chan: &'a mut zx::Channel,
    },
}

impl<'a> OpenRequest<'a> {
    /// Creates a request to open a capability with source `route_source` for `target`.
    pub fn new_from_route_source(
        route_source: RouteSource,
        target: &'a Arc<ComponentInstance>,
        options: OpenOptions<'a>,
    ) -> Self {
        match route_source {
            RouteSource::Directory(source, directory_state) => {
                if let OpenOptions::Directory(open_dir_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_dir_options.flags,
                        relative_path: directory_state
                            .make_relative_path(open_dir_options.relative_path),
                        source,
                        target,
                        server_chan: open_dir_options.server_chan,
                    };
                }
            }
            RouteSource::Protocol(source) => {
                if let OpenOptions::Protocol(open_protocol_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_protocol_options.flags,
                        relative_path: PathBuf::from(open_protocol_options.relative_path),
                        source,
                        target,
                        server_chan: open_protocol_options.server_chan,
                    };
                }
            }
            RouteSource::Service(source) => {
                if let OpenOptions::Service(open_service_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_service_options.flags,
                        relative_path: PathBuf::from(open_service_options.relative_path),
                        source,
                        target,
                        server_chan: open_service_options.server_chan,
                    };
                }
            }
            RouteSource::Resolver(source) => {
                if let OpenOptions::Resolver(open_resolver_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_resolver_options.flags,
                        relative_path: PathBuf::new(),
                        source,
                        target,
                        server_chan: open_resolver_options.server_chan,
                    };
                }
            }
            RouteSource::Runner(source) => {
                if let OpenOptions::Runner(open_runner_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_runner_options.flags,
                        relative_path: PathBuf::new(),
                        source,
                        target,
                        server_chan: open_runner_options.server_chan,
                    };
                }
            }
            RouteSource::EventStream(source) => {
                if let OpenOptions::EventStream(open_event_stream_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_event_stream_options.flags,
                        relative_path: PathBuf::from(open_event_stream_options.relative_path),
                        source,
                        target,
                        server_chan: open_event_stream_options.server_chan,
                    };
                }
            }
            RouteSource::StorageBackingDirectory(source) => {
                if let OpenOptions::Storage(open_storage_options) = options {
                    return Self::Storage {
                        flags: open_storage_options.flags,
                        relative_path: open_storage_options.relative_path,
                        source,
                        target,
                        server_chan: open_storage_options.server_chan,
                    };
                }
            }
            RouteSource::Storage(_) | RouteSource::Event(_) => panic!("unsupported route source"),
        }
        panic!("route source type did not match option type")
    }

    /// Directly creates a request to open a capability at `source`'s outgoing directory with
    /// with `flags` and `relative_path`.
    pub fn new_outgoing_directory(
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        source: CapabilitySource,
        target: &'a Arc<ComponentInstance>,
        server_chan: &'a mut zx::Channel,
    ) -> Self {
        Self::OutgoingDirectory { flags, relative_path, source, target, server_chan }
    }

    /// Opens the capability in `self`, triggering a `CapabilityRouted` event and binding
    /// to the source component instance if necessary.
    pub async fn open(self) -> Result<(), ModelError> {
        match self {
            Self::OutgoingDirectory { flags, relative_path, source, target, server_chan } => {
                Self::open_outgoing_directory(flags, relative_path, source, target, server_chan)
                    .await
            }
            Self::Storage { flags, relative_path, source, target, server_chan } => {
                Self::open_storage(flags, relative_path, &source, target, server_chan).await
            }
        }
    }

    async fn open_outgoing_directory(
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        source: CapabilitySource,
        target: &Arc<ComponentInstance>,
        server_chan: &mut zx::Channel,
    ) -> Result<(), ModelError> {
        let capability_provider =
            if let Some(provider) = Self::get_default_provider(target.as_weak(), &source).await? {
                Some(provider)
            } else {
                // Dispatch a CapabilityRouted event to get a capability provider
                let mutexed_provider = Arc::new(Mutex::new(None));

                let event = Event::new(
                    &target,
                    EventPayload::CapabilityRouted {
                        source: source.clone(),
                        capability_provider: mutexed_provider.clone(),
                    },
                );

                // Get a capability provider from the tree
                target.hooks.dispatch(&event).await;

                let provider = mutexed_provider.lock().await.take();
                provider
            };

        if let Some(capability_provider) = capability_provider {
            let source_instance = source.source_instance().upgrade()?;
            let task_scope = match source_instance {
                ExtendedInstance::AboveRoot(top) => top.task_scope(),
                ExtendedInstance::Component(component) => component.nonblocking_task_scope(),
            };
            capability_provider.open(task_scope, flags, relative_path, server_chan).await?;
            Ok(())
        } else {
            match &source {
                CapabilitySource::Component { .. } => {
                    unreachable!(
                        "Capability source is a component, which should have been caught by \
                    default_capability_provider: {:?}",
                        source
                    );
                }
                CapabilitySource::FilteredService { .. } => {
                    Err(ModelError::unsupported("filtered service"))
                }
                CapabilitySource::Framework { capability, component } => {
                    Err(RoutingError::capability_from_framework_not_found(
                        &component.abs_moniker,
                        capability.source_name().to_string(),
                    )
                    .into())
                }
                CapabilitySource::Capability { source_capability, component } => {
                    Err(RoutingError::capability_from_capability_not_found(
                        &component.abs_moniker,
                        source_capability.to_string(),
                    )
                    .into())
                }
                CapabilitySource::Builtin { capability, .. } => Err(ModelError::from(
                    RoutingError::capability_from_component_manager_not_found(
                        capability.source_name().to_string(),
                    ),
                )),
                CapabilitySource::Namespace { capability, .. } => Err(ModelError::from(
                    RoutingError::capability_from_component_manager_not_found(
                        capability.source_id(),
                    ),
                )),
                CapabilitySource::Collection { .. } => Err(ModelError::unsupported("collections")),
                CapabilitySource::Aggregate { .. } => {
                    Err(ModelError::unsupported("aggregate capability"))
                }
            }
        }
    }

    async fn open_storage(
        flags: fio::OpenFlags,
        relative_path: String,
        source: &storage::StorageCapabilitySource,
        target: &Arc<ComponentInstance>,
        server_chan: &mut zx::Channel,
    ) -> Result<(), ModelError> {
        // As of today, the storage component instance must contain the target. This is because it
        // is impossible to expose storage declarations up.
        let relative_moniker = InstancedRelativeMoniker::scope_down(
            &source.storage_source_moniker,
            &target.instanced_moniker(),
        )
        .unwrap();

        let dir_source = source.storage_provider.clone();
        let storage_dir_proxy = storage::open_isolated_storage(
            &source,
            target.persistent_storage,
            relative_moniker.clone(),
            target.instance_id().as_ref(),
        )
        .await
        .map_err(|e| ModelError::from(e))?;

        // Open the storage with the provided flags, mode, relative_path and server_chan.
        // We don't clone the directory because we can't specify the mode or path that way.
        let server_chan = channel::take_channel(server_chan);

        // If there is no relative path, assume it is the current directory. We use "."
        // because `fuchsia.io/Directory.Open` does not accept empty paths.
        let relative_path = if relative_path.is_empty() { "." } else { &relative_path };

        storage_dir_proxy
            .open(flags, fio::ModeType::empty(), relative_path, ServerEnd::new(server_chan))
            .map_err(|err| {
                let moniker = match &dir_source {
                    Some(r) => {
                        InstancedExtendedMoniker::ComponentInstance(r.instanced_moniker().clone())
                    }
                    None => InstancedExtendedMoniker::ComponentManager,
                };
                ModelError::OpenStorageFailed {
                    moniker,
                    relative_moniker,
                    path: relative_path.to_string(),
                    err,
                }
            })?;
        Ok(())
    }

    /// Returns an instance of the default capability provider for the capability at `source`, if
    /// supported.
    async fn get_default_provider(
        target: WeakComponentInstance,
        source: &CapabilitySource,
    ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
        match source {
            CapabilitySource::Component { capability, component } => {
                // Route normally for a component capability with a source path
                Ok(match capability.source_path() {
                    Some(path) => Some(Box::new(DefaultComponentCapabilityProvider {
                        target,
                        source: component.clone(),
                        name: capability
                            .source_name()
                            .expect("capability with source path should have a name")
                            .clone(),
                        path: path.clone(),
                    })),
                    _ => None,
                })
            }
            CapabilitySource::Namespace { capability, .. } => match capability.source_path() {
                Some(path) => {
                    Ok(Some(Box::new(NamespaceCapabilityProvider { path: path.clone() })))
                }
                _ => Ok(None),
            },
            CapabilitySource::FilteredService {
                capability,
                component,
                source_instance_filter,
                instance_name_source_to_target,
            } => {
                // First get the base service capability provider
                match capability.source_path() {
                    Some(path) => {
                        let base_capability_provider =
                            Box::new(DefaultComponentCapabilityProvider {
                                target,
                                source: component.clone(),
                                name: capability
                                    .source_name()
                                    .expect("capability with source path should have a name")
                                    .clone(),
                                path: path.clone(),
                            });

                        let source_component = component.upgrade()?;
                        let provider = FilteredServiceProvider::new(
                            &source_component,
                            source_instance_filter.clone(),
                            instance_name_source_to_target.clone(),
                            base_capability_provider,
                        )
                        .await?;
                        Ok(Some(Box::new(provider)))
                    }
                    _ => Ok(None),
                }
            }
            CapabilitySource::Aggregate { capability_provider, component, .. } => {
                Ok(Some(Box::new(
                    AggregateServiceDirectoryProvider::create(
                        component.clone(),
                        target,
                        capability_provider.clone(),
                    )
                    .await?,
                )))
            }
            CapabilitySource::Collection {
                capability,
                component,
                aggregate_capability_provider,
                collection_name,
            } => {
                let source_component_instance = component.upgrade()?;

                let route = CollectionServiceRoute {
                    source_moniker: source_component_instance.abs_moniker.clone(),
                    collection_name: collection_name.clone(),
                    service_name: capability.source_name().clone(),
                };

                source_component_instance
                    .start(&StartReason::AccessCapability {
                        target: target.abs_moniker.clone(),
                        name: capability.source_name().clone(),
                    })
                    .await?;

                // If there is an existing collection service directory, provide it.
                {
                    let state = source_component_instance.lock_resolved_state().await?;
                    if let Some(service_dir) = state.collection_services.get(&route) {
                        let provider = DirectoryEntryCapabilityProvider {
                            execution_scope: state.execution_scope().clone(),
                            entry: service_dir.dir_entry().await,
                        };
                        return Ok(Some(Box::new(provider)));
                    }
                }

                // Otherwise, create one. This must be done while the component ResolvedInstanceState
                // is unlocked because the AggregateCapabilityProvider uses locked state.
                let service_dir = Arc::new(CollectionServiceDirectory::new(
                    component.clone(),
                    route.clone(),
                    aggregate_capability_provider.clone_boxed(),
                ));

                source_component_instance.hooks.install(service_dir.hooks()).await;

                let provider = {
                    let mut state = source_component_instance.lock_resolved_state().await?;
                    let execution_scope = state.execution_scope().clone();
                    let entry = service_dir.dir_entry().await;

                    state.collection_services.insert(route, service_dir.clone());

                    DirectoryEntryCapabilityProvider { execution_scope, entry }
                };

                // Populate the service dir with service entries from children that may have been started before the service
                // capability had been routed from the collection.
                service_dir.add_entries_from_children().await?;

                Ok(Some(Box::new(provider)))
            }
            CapabilitySource::Framework { .. }
            | CapabilitySource::Capability { .. }
            | CapabilitySource::Builtin { .. } => {
                // There is no default provider for a framework or builtin capability
                Ok(None)
            }
        }
    }
}
