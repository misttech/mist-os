// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::model::Model;
use crate::model::namespace::create_namespace;
use crate::model::storage::admin_protocol::StorageAdmin;
use ::routing::capability_source::InternalCapability;
use async_trait::async_trait;
use cm_rust::NativeIntoFidl;
use cm_types::{Name, Url};
use errors::OpenExposedDirError;
use fidl::endpoints::{ClientEnd, ServerEnd};
use fidl::prelude::*;
use futures::StreamExt;
use lazy_static::lazy_static;
use measure_tape_for_instance::Measurable;
use moniker::Moniker;
use router_error::Explain;
use routing::component_instance::{ComponentInstanceInterface, ResolvedInstanceInterface};
use routing::resolving::ComponentAddress;
use std::sync::{Arc, Weak};
use tracing::warn;
use vfs::directory::entry::OpenRequest;
use vfs::directory::entry_container::Directory;
use vfs::ToObjectRequest;
use zx::sys::ZX_CHANNEL_MAX_MSG_BYTES;
use {
    fidl_fuchsia_component_decl as fcdecl, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fsys::RealmQueryMarker::PROTOCOL_NAME.parse().unwrap();
}

// Number of bytes the header of a vector occupies in a fidl message.
// TODO(https://fxbug.dev/42181010): This should be a constant in a FIDL library.
const FIDL_VECTOR_HEADER_BYTES: usize = 16;

// Number of bytes the header of a fidl message occupies.
// TODO(https://fxbug.dev/42181010): This should be a constant in a FIDL library.
const FIDL_HEADER_BYTES: usize = 16;

// Number of bytes of a manifest that can fit in a single message
// sent on a zircon channel.
const FIDL_MANIFEST_MAX_MSG_BYTES: usize =
    (ZX_CHANNEL_MAX_MSG_BYTES as usize) - (FIDL_HEADER_BYTES + FIDL_VECTOR_HEADER_BYTES);

impl RealmQuery {
    pub fn new(model: Weak<Model>) -> Self {
        Self { model }
    }

    async fn serve(self, scope_moniker: Moniker, mut stream: fsys::RealmQueryRequestStream) {
        loop {
            let request = match stream.next().await {
                Some(Ok(request)) => request,
                Some(Err(error)) => {
                    warn!(?error, "Could not get next RealmQuery request");
                    break;
                }
                None => break,
            };
            let Some(model) = self.model.upgrade() else {
                break;
            };
            let result = match request {
                fsys::RealmQueryRequest::GetInstance { moniker, responder } => {
                    let result = get_instance(&model, &scope_moniker, &moniker).await;
                    responder.send(result.as_ref().map_err(|e| *e))
                }
                fsys::RealmQueryRequest::GetManifest { moniker, responder } => {
                    let result = get_resolved_declaration(&model, &scope_moniker, &moniker).await;
                    responder.send(result)
                }
                fsys::RealmQueryRequest::GetResolvedDeclaration { moniker, responder } => {
                    let result = get_resolved_declaration(&model, &scope_moniker, &moniker).await;
                    responder.send(result)
                }
                fsys::RealmQueryRequest::ResolveDeclaration {
                    parent,
                    child_location,
                    url,
                    responder,
                } => {
                    let result =
                        resolve_declaration(&model, &scope_moniker, &parent, &child_location, &url)
                            .await;
                    responder.send(result)
                }
                fsys::RealmQueryRequest::GetStructuredConfig { moniker, responder } => {
                    let result = get_structured_config(&model, &scope_moniker, &moniker).await;
                    responder.send(result.as_ref().map_err(|e| *e))
                }
                fsys::RealmQueryRequest::GetAllInstances { responder } => {
                    let result = get_all_instances(&model, &scope_moniker).await;
                    responder.send(result)
                }
                fsys::RealmQueryRequest::ConstructNamespace { moniker, responder } => {
                    let result = construct_namespace(&model, &scope_moniker, &moniker).await;
                    responder.send(result)
                }
                #[cfg(any(
                    fuchsia_api_level_less_than = "25",
                    fuchsia_api_level_at_least = "PLATFORM"
                ))]
                fsys::RealmQueryRequest::Open {
                    moniker,
                    dir_type,
                    flags,
                    mode,
                    path,
                    object,
                    responder,
                } => {
                    let result = open_deprecated(
                        &model,
                        &scope_moniker,
                        &moniker,
                        dir_type,
                        flags,
                        mode,
                        &path,
                        object,
                    )
                    .await;
                    responder.send(result)
                }
                #[cfg(fuchsia_api_level_at_least = "25")]
                fsys::RealmQueryRequest::OpenDirectory { moniker, dir_type, object, responder } => {
                    let result =
                        open_directory(&model, &scope_moniker, &moniker, dir_type, object).await;
                    responder.send(result)
                }
                fsys::RealmQueryRequest::ConnectToStorageAdmin {
                    moniker,
                    storage_name,
                    server_end,
                    responder,
                } => {
                    let result = connect_to_storage_admin(
                        &model,
                        &scope_moniker,
                        &moniker,
                        storage_name,
                        server_end,
                    )
                    .await;
                    responder.send(result)
                }
            };
            if let Err(error) = result {
                warn!(?error, "Could not respond to RealmQuery request");
                break;
            }
        }
    }
}

#[derive(Clone)]
pub struct RealmQuery {
    model: Weak<Model>,
}

impl FrameworkCapability for RealmQuery {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(RealmQueryCapabilityProvider { query: self.clone(), scope_moniker: scope.moniker })
    }
}

struct RealmQueryCapabilityProvider {
    query: RealmQuery,
    scope_moniker: Moniker,
}

#[async_trait]
impl InternalCapabilityProvider for RealmQueryCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsys::RealmQueryMarker>::new(server_end);
        self.query.serve(self.scope_moniker, server_end.into_stream()).await;
    }
}

/// Create the state matching the given moniker string in this scope
async fn get_instance(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
) -> Result<fsys::Instance, fsys::GetInstanceError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker = Moniker::try_from(moniker_str).map_err(|_| fsys::GetInstanceError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance =
        model.root().find(&moniker).await.ok_or(fsys::GetInstanceError::InstanceNotFound)?;
    let instance_id = model.component_id_index().id_for_moniker(&instance.moniker).cloned();

    let resolved_info = {
        let state = instance.lock_state().await;

        if let Some(resolved_state) = state.get_resolved_state() {
            let resolved_url = Some(resolved_state.address().url().to_string());
            let execution_info =
                state.get_started_state().map(|started_state| fsys::ExecutionInfo {
                    start_reason: Some(started_state.start_reason.to_string()),
                    ..Default::default()
                });
            Some(fsys::ResolvedInfo { resolved_url, execution_info, ..Default::default() })
        } else {
            None
        }
    };

    Ok(fsys::Instance {
        moniker: Some(moniker.to_string()),
        url: Some(instance.component_url.to_string()),
        environment: instance.environment().name().map(|n| n.to_string()),
        instance_id: instance_id.map(|id| id.to_string()),
        resolved_info,
        ..Default::default()
    })
}

/// Encode the component manifest of an instance into a standalone persistable FIDL format.
async fn get_resolved_declaration(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
) -> Result<ClientEnd<fsys::ManifestBytesIteratorMarker>, fsys::GetDeclarationError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker =
        Moniker::try_from(moniker_str).map_err(|_| fsys::GetDeclarationError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance =
        model.root().find(&moniker).await.ok_or(fsys::GetDeclarationError::InstanceNotFound)?;

    let state = instance.lock_state().await;

    let decl = state
        .get_resolved_state()
        .ok_or(fsys::GetDeclarationError::InstanceNotResolved)?
        .decl()
        .clone()
        .native_into_fidl();

    let bytes = fidl::persist(&decl).map_err(|error| {
        warn!(%moniker, %error, "RealmQuery failed to encode manifest");
        fsys::GetDeclarationError::EncodeFailed
    })?;

    // Attach the iterator task to the scope root.
    let scope_root = model
        .root()
        .find(scope_moniker)
        .await
        .ok_or(fsys::GetDeclarationError::InstanceNotFound)?;

    let (client_end, server_end) =
        fidl::endpoints::create_endpoints::<fsys::ManifestBytesIteratorMarker>();

    // Attach the iterator task to the scope root.
    let task_group = scope_root.nonblocking_task_group();
    task_group.spawn(serve_manifest_bytes_iterator(server_end, bytes));

    Ok(client_end)
}

/// Encode the component manifest of a potential instance into a standalone persistable FIDL format.
async fn resolve_declaration(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    parent_moniker_str: &str,
    child_location: &fsys::ChildLocation,
    url: &str,
) -> Result<ClientEnd<fsys::ManifestBytesIteratorMarker>, fsys::GetDeclarationError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let parent_moniker =
        Moniker::try_from(parent_moniker_str).map_err(|_| fsys::GetDeclarationError::BadMoniker)?;
    let parent_moniker = scope_moniker.concat(&parent_moniker);

    let collection = match child_location {
        fsys::ChildLocation::Collection(coll) => coll.to_owned(),
        _ => return Err(fsys::GetDeclarationError::BadChildLocation),
    };

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance = model
        .root()
        .find(&parent_moniker)
        .await
        .ok_or(fsys::GetDeclarationError::InstanceNotFound)?;

    let (address, collection_input) = {
        // this lock needs to be dropped before we try to call resolve, since routing the resolver
        // may also need to take this lock
        let state = instance.lock_state().await;
        let resolved_state =
            state.get_resolved_state().ok_or(fsys::GetDeclarationError::InstanceNotResolved)?;
        let address = if url.starts_with("#") {
            resolved_state
                .address_for_relative_url(url)
                .map_err(|_| fsys::GetDeclarationError::BadUrl)?
        } else {
            Url::new(url)
                .ok()
                .and_then(|url| ComponentAddress::from_absolute_url(&url).ok())
                .ok_or(fsys::GetDeclarationError::BadUrl)?
        };
        let collection_input = resolved_state
            .sandbox
            .collection_inputs
            .get(
                &Name::new(collection)
                    .map_err(|_| fsys::GetDeclarationError::InstanceNotResolved)?,
            )
            .ok_or(fsys::GetDeclarationError::InstanceNotResolved)?;
        (address, collection_input)
    };

    let resolved = instance
        .perform_resolve(Some(collection_input), &address)
        .await
        .map_err(|_| fsys::GetDeclarationError::InstanceNotResolved)?;

    let bytes = fidl::persist(&resolved.decl.native_into_fidl()).map_err(|error| {
        warn!(parent=%parent_moniker, %error, "RealmQuery failed to encode manifest");
        fsys::GetDeclarationError::EncodeFailed
    })?;

    // Attach the iterator task to the scope root.
    let scope_root = model
        .root()
        .find(scope_moniker)
        .await
        .ok_or(fsys::GetDeclarationError::InstanceNotFound)?;

    let (client_end, server_end) =
        fidl::endpoints::create_endpoints::<fsys::ManifestBytesIteratorMarker>();

    // Attach the iterator task to the scope root.
    let task_group = scope_root.nonblocking_task_group();
    task_group.spawn(serve_manifest_bytes_iterator(server_end, bytes));
    Ok(client_end)
}

/// Get the structured config of an instance
async fn get_structured_config(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
) -> Result<fcdecl::ResolvedConfig, fsys::GetStructuredConfigError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker =
        Moniker::try_from(moniker_str).map_err(|_| fsys::GetStructuredConfigError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance = model
        .root()
        .find(&moniker)
        .await
        .ok_or(fsys::GetStructuredConfigError::InstanceNotFound)?;

    let state = instance.lock_state().await;
    let config = state
        .get_resolved_state()
        .ok_or(fsys::GetStructuredConfigError::InstanceNotResolved)?
        .config()
        .ok_or(fsys::GetStructuredConfigError::NoConfig)?
        .clone()
        .into();

    Ok(config)
}

async fn construct_namespace(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
) -> Result<Vec<fcrunner::ComponentNamespaceEntry>, fsys::ConstructNamespaceError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker =
        Moniker::try_from(moniker_str).map_err(|_| fsys::ConstructNamespaceError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance =
        model.root().find(&moniker).await.ok_or(fsys::ConstructNamespaceError::InstanceNotFound)?;
    let state = instance.lock_state().await;
    let resolved_state =
        state.get_resolved_state().ok_or(fsys::ConstructNamespaceError::InstanceNotResolved)?;
    let namespace = create_namespace(
        resolved_state.package(),
        &instance,
        resolved_state.decl(),
        &resolved_state.sandbox.program_input.namespace(),
        instance.execution_scope.clone(),
    )
    .await
    .unwrap();
    let ns = namespace.serve().unwrap();
    Ok(ns.into())
}

#[cfg(any(fuchsia_api_level_less_than = "25", fuchsia_api_level_at_least = "PLATFORM"))]
async fn open_deprecated(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
    dir_type: fsys::OpenDirType,
    flags: fio::OpenFlags,
    mode: fio::ModeType,
    path: &str,
    object: ServerEnd<fio::NodeMarker>,
) -> Result<(), fsys::OpenError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker = Moniker::try_from(moniker_str).map_err(|_| fsys::OpenError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance = model.root().find(&moniker).await.ok_or(fsys::OpenError::InstanceNotFound)?;

    match dir_type {
        fsys::OpenDirType::OutgoingDir => {
            let mut object_request = flags.to_object_request(object);
            if let Err(e) = instance
                .open_outgoing(OpenRequest::new(
                    instance.execution_scope.clone(),
                    flags,
                    path.try_into().map_err(|_| fsys::OpenError::BadPath)?,
                    &mut object_request,
                ))
                .await
            {
                object_request.shutdown(e.as_zx_status());
                Err(fsys::OpenError::FidlError)
            } else {
                Ok(())
            }
        }
        fsys::OpenDirType::RuntimeDir => {
            let state = instance.lock_state().await;
            let dir = state
                .get_started_state()
                .ok_or(fsys::OpenError::InstanceNotRunning)?
                .runtime_dir()
                .ok_or(fsys::OpenError::NoSuchDir)?;
            dir.open(flags, mode, path, object).map_err(|_| fsys::OpenError::FidlError)
        }
        fsys::OpenDirType::PackageDir => {
            let mut state = instance.lock_state().await;
            match state.get_resolved_state_mut() {
                Some(r) => {
                    let pkg = r.package().ok_or(fsys::OpenError::NoSuchDir)?;
                    pkg.package_dir
                        .open(flags, mode, path, object)
                        .map_err(|_| fsys::OpenError::FidlError)
                }
                None => Err(fsys::OpenError::InstanceNotResolved),
            }
        }
        fsys::OpenDirType::ExposedDir => {
            let mut object_request = flags.to_object_request(object);
            if let Err(e) = instance
                .open_exposed(OpenRequest::new(
                    instance.execution_scope.clone(),
                    flags,
                    path.try_into().map_err(|_| fsys::OpenError::BadPath)?,
                    &mut object_request,
                ))
                .await
            {
                object_request.shutdown(e.as_zx_status());
                Err(match e {
                    OpenExposedDirError::InstanceNotResolved => {
                        fsys::OpenError::InstanceNotResolved
                    }
                    _ => fsys::OpenError::FidlError,
                })
            } else {
                Ok(())
            }
        }
        fsys::OpenDirType::NamespaceDir => {
            let path =
                vfs::path::Path::validate_and_split(path).map_err(|_| fsys::OpenError::BadPath)?;

            let state = instance.lock_state().await;
            let resolved_state =
                state.get_resolved_state().ok_or(fsys::OpenError::InstanceNotResolved)?;

            resolved_state.namespace_dir().await.map_err(|_| fsys::OpenError::NoSuchDir)?.open(
                instance.execution_scope.clone(),
                flags,
                path,
                object,
            );

            Ok(())
        }
        _ => Err(fsys::OpenError::BadDirType),
    }
}

#[cfg(fuchsia_api_level_at_least = "25")]
async fn open_directory(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
    dir_type: fsys::OpenDirType,
    object: ServerEnd<fio::DirectoryMarker>,
) -> Result<(), fsys::OpenError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker = Moniker::try_from(moniker_str).map_err(|_| fsys::OpenError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance = model.root().find(&moniker).await.ok_or(fsys::OpenError::InstanceNotFound)?;

    // The intention is that this is only used for component introspection, so we only request
    // readable/executable rights when creating a connection to the desired directory.
    const FLAGS: fio::Flags = fio::PERM_READABLE.union(fio::Flags::PERM_INHERIT_EXECUTE);
    let mut request = FLAGS.to_object_request(object);
    let path = vfs::path::Path::dot();

    match dir_type {
        fsys::OpenDirType::OutgoingDir => {
            let scope: package_directory::ExecutionScope = instance.execution_scope.clone();
            instance
                .open_outgoing(OpenRequest::new(scope, FLAGS, path, &mut request))
                .await
                .map_err(|e| {
                    request.shutdown(e.as_zx_status());
                    e.into()
                })
        }
        fsys::OpenDirType::RuntimeDir => {
            let state = instance.lock_state().await;
            let runtime_dir = state
                .get_started_state()
                .ok_or(fsys::OpenError::InstanceNotRunning)?
                .runtime_dir()
                .ok_or(fsys::OpenError::NoSuchDir)?;
            runtime_dir
                .open3(path.as_ref(), FLAGS, &Default::default(), request.into_channel())
                .map_err(|_| fsys::OpenError::FidlError)
        }
        fsys::OpenDirType::PackageDir => {
            let state = instance.lock_state().await;
            let resolved =
                state.get_resolved_state().ok_or(fsys::OpenError::InstanceNotResolved)?;
            let package_dir = &resolved.package().ok_or(fsys::OpenError::NoSuchDir)?.package_dir;
            package_dir
                .open3(path.as_ref(), FLAGS, &Default::default(), request.into_channel())
                .map_err(|_| fsys::OpenError::FidlError)
        }
        fsys::OpenDirType::ExposedDir => {
            let scope: package_directory::ExecutionScope = instance.execution_scope.clone();
            instance.open_exposed(OpenRequest::new(scope, FLAGS, path, &mut request)).await.map_err(
                |e| {
                    request.shutdown(e.as_zx_status());
                    e.into()
                },
            )
        }
        fsys::OpenDirType::NamespaceDir => {
            let state = instance.lock_state().await;
            let resolved =
                state.get_resolved_state().ok_or(fsys::OpenError::InstanceNotResolved)?;
            let namespace_dir =
                resolved.namespace_dir().await.map_err(|_| fsys::OpenError::NoSuchDir)?;
            let scope: package_directory::ExecutionScope = instance.execution_scope.clone();
            namespace_dir
                .open3(scope, path, FLAGS, &mut request)
                .map_err(|_| fsys::OpenError::FidlError)
        }
        _ => Err(fsys::OpenError::BadDirType),
    }
}

async fn connect_to_storage_admin(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    moniker_str: &str,
    storage_name: String,
    server_end: ServerEnd<fsys::StorageAdminMarker>,
) -> Result<(), fsys::ConnectToStorageAdminError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker =
        Moniker::try_from(moniker_str).map_err(|_| fsys::ConnectToStorageAdminError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // TODO(https://fxbug.dev/42059901): Close the connection if the scope root cannot be found.
    let instance = model
        .root()
        .find(&moniker)
        .await
        .ok_or(fsys::ConnectToStorageAdminError::InstanceNotFound)?;

    let storage_admin = StorageAdmin::new();
    let task_group = instance.nonblocking_task_group();

    let storage_decl = {
        let state = instance.lock_state().await;
        state
            .get_resolved_state()
            .ok_or(fsys::ConnectToStorageAdminError::InstanceNotResolved)?
            .decl()
            .find_storage_source(
                &storage_name
                    .parse()
                    .map_err(|_| fsys::ConnectToStorageAdminError::BadCapability)?,
            )
            .ok_or(fsys::ConnectToStorageAdminError::StorageNotFound)?
            .clone()
    };

    task_group.spawn(async move {
        if let Err(error) =
            storage_admin.serve(storage_decl, instance.as_weak(), server_end.into_stream()).await
        {
            warn!(
                %moniker, %error, "StorageAdmin created by LifecycleController failed to serve",
            );
        };
    });
    Ok(())
}

/// Take a snapshot of all instances in the given scope and serves an instance iterator
/// over the snapshots.
async fn get_all_instances(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
) -> Result<ClientEnd<fsys::InstanceIteratorMarker>, fsys::GetAllInstancesError> {
    let mut instances = vec![];

    // Only take instances contained within the scope realm
    let scope_root = model
        .root()
        .find(scope_moniker)
        .await
        .ok_or(fsys::GetAllInstancesError::InstanceNotFound)?;

    let mut queue = vec![scope_root.clone()];

    while !queue.is_empty() {
        let cur = queue.pop().unwrap();

        let (instance, mut children) =
            get_fidl_instance_and_children(model, scope_moniker, &cur).await;
        instances.push(instance);
        queue.append(&mut children);
    }

    let (client_end, server_end) =
        fidl::endpoints::create_endpoints::<fsys::InstanceIteratorMarker>();

    // Attach the iterator task to the scope root.
    let task_group = scope_root.nonblocking_task_group();
    task_group.spawn(serve_instance_iterator(server_end, instances));

    Ok(client_end)
}

/// Create the detailed instance info matching the given moniker string in this scope
/// and return all live children of the instance.
async fn get_fidl_instance_and_children(
    model: &Arc<Model>,
    scope_moniker: &Moniker,
    instance: &Arc<ComponentInstance>,
) -> (fsys::Instance, Vec<Arc<ComponentInstance>>) {
    let moniker = instance
        .moniker
        .strip_prefix(scope_moniker)
        .expect("instance must have been a child of scope root");
    let instance_id = model.component_id_index().id_for_moniker(&instance.moniker).cloned();

    let (resolved_info, children) = {
        let state = instance.lock_state().await;

        if let Some(resolved_state) = state.get_resolved_state() {
            let resolved_url = Some(resolved_state.address().url().to_string());
            let children = resolved_state.children().map(|(_, c)| c.clone()).collect();
            let execution_info =
                state.get_started_state().map(|started_state| fsys::ExecutionInfo {
                    start_reason: Some(started_state.start_reason.to_string()),
                    ..Default::default()
                });
            (
                Some(fsys::ResolvedInfo { resolved_url, execution_info, ..Default::default() }),
                children,
            )
        } else {
            (None, vec![])
        }
    };

    (
        fsys::Instance {
            moniker: Some(moniker.to_string()),
            url: Some(instance.component_url.to_string()),
            environment: instance.environment().name().map(|n| n.to_string()),
            instance_id: instance_id.map(|id| id.to_string()),
            resolved_info,
            ..Default::default()
        },
        children,
    )
}

async fn serve_instance_iterator(
    server_end: ServerEnd<fsys::InstanceIteratorMarker>,
    instances: Vec<fsys::Instance>,
) {
    let mut remaining_instances = &instances[..];
    let mut stream: fsys::InstanceIteratorRequestStream = server_end.into_stream();
    while let Some(Ok(fsys::InstanceIteratorRequest::Next { responder })) = stream.next().await {
        let mut bytes_used: usize = FIDL_HEADER_BYTES + FIDL_VECTOR_HEADER_BYTES;
        let mut instance_count = 0;

        // Determine how many info objects can be sent in a single FIDL message.
        // TODO(https://fxbug.dev/42181010): This logic should be handled by FIDL.
        for instance in remaining_instances {
            bytes_used += instance.measure().num_bytes;
            if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
                break;
            }
            instance_count += 1;
        }

        let result = responder.send(&remaining_instances[..instance_count]);
        remaining_instances = &remaining_instances[instance_count..];
        if let Err(error) = result {
            warn!(?error, "RealmQuery encountered error sending instance batch");
            break;
        }

        // Close the iterator because all the data was sent.
        if instance_count == 0 {
            break;
        }
    }
}

async fn serve_manifest_bytes_iterator(
    server_end: ServerEnd<fsys::ManifestBytesIteratorMarker>,
    mut bytes: Vec<u8>,
) {
    let mut stream: fsys::ManifestBytesIteratorRequestStream = server_end.into_stream();

    while let Some(Ok(fsys::ManifestBytesIteratorRequest::Next { responder })) = stream.next().await
    {
        let bytes_to_drain = std::cmp::min(FIDL_MANIFEST_MAX_MSG_BYTES, bytes.len());
        let batch: Vec<u8> = bytes.drain(0..bytes_to_drain).collect();
        let batch_size = batch.len();

        let result = responder.send(&batch);
        if let Err(error) = result {
            warn!(?error, "RealmQuery encountered error sending manifest bytes");
            break;
        }

        // Close the iterator because all the data was sent.
        if batch_size == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability;
    use crate::model::component::StartReason;
    use crate::model::start::Start;
    use crate::model::testing::test_helpers::{
        config_override, new_config_decl, TestEnvironmentBuilder, TestModelResult,
    };
    use assert_matches::assert_matches;
    use cm_rust::*;
    use cm_rust_testing::*;
    use component_id_index::InstanceId;
    use fidl::endpoints;
    use fidl::endpoints::{create_endpoints, create_proxy};
    use routing_test_helpers::component_id_index::make_index_file;
    use {fidl_fuchsia_component_decl as fcdecl, fidl_fuchsia_io as fio};

    fn is_closed(handle: impl fidl::AsHandleRef) -> bool {
        handle
            .wait_handle(zx::Signals::OBJECT_PEER_CLOSED, zx::MonotonicInstant::from_nanos(0))
            .is_ok()
    }

    async fn realm_query(test: &TestModelResult) -> (fsys::RealmQueryProxy, RealmQuery) {
        let host = {
            let env = test.builtin_environment.lock().await;
            env.realm_query.clone().unwrap()
        };
        let (proxy, server) = endpoints::create_proxy::<fsys::RealmQueryMarker>().unwrap();
        capability::open_framework(&host, test.model.root(), server.into()).await.unwrap();
        (proxy, host)
    }

    #[fuchsia::test]
    async fn get_instance_test() {
        // Create index.
        let iid = format!("1234{}", "5".repeat(60)).parse::<InstanceId>().unwrap();
        let index = {
            let mut index = component_id_index::Index::default();
            index.insert(Moniker::parse_str("/").unwrap(), iid.clone()).unwrap();
            index
        };
        let index_file = make_index_file(index).unwrap();

        let components = vec![("root", ComponentDeclBuilder::new().build())];

        let test = TestEnvironmentBuilder::new()
            .set_components(components)
            .set_component_id_index_path(index_file.path().to_owned().try_into().unwrap())
            .build()
            .await;
        let (query, _host) = realm_query(&test).await;

        test.model.start().await;

        let instance = query.get_instance(".").await.unwrap().unwrap();

        assert_eq!(instance.moniker.unwrap(), ".");
        assert_eq!(instance.url.unwrap(), "test:///root");
        assert_eq!(instance.instance_id.unwrap().parse::<InstanceId>().unwrap(), iid);

        let resolved = instance.resolved_info.unwrap();
        assert_eq!(resolved.resolved_url.unwrap(), "test:///root");

        let execution = resolved.execution_info.unwrap();
        assert_eq!(execution.start_reason.unwrap(), StartReason::Root.to_string());
    }

    #[fuchsia::test]
    async fn manifest_test() {
        // Try to create a manifest that will exceed the size of a Zircon channel message.
        let mut manifest = ComponentDeclBuilder::new();

        for i in 0..10000 {
            let use_name = format!("use_{}", i);
            let expose_name = format!("expose_{}", i);
            let capability_path = format!("/svc/capability_{}", i);

            let use_decl = UseBuilder::protocol()
                .source(UseSource::Framework)
                .name(&use_name)
                .path(&capability_path)
                .build();
            let expose_decl =
                ExposeBuilder::protocol().source(ExposeSource::Self_).name(&expose_name).build();
            let capability_decl = CapabilityBuilder::protocol().name(&expose_name).build();

            manifest = manifest.use_(use_decl).expose(expose_decl).capability(capability_decl);
        }

        let components = vec![("root", manifest.build())];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (query, _host) = realm_query(&test).await;

        test.model.start().await;

        let iterator = query.get_resolved_declaration("./").await.unwrap().unwrap();
        let iterator = iterator.into_proxy();

        let mut bytes = vec![];

        loop {
            let mut batch = iterator.next().await.unwrap();
            if batch.is_empty() {
                break;
            }
            bytes.append(&mut batch);
        }

        let manifest = fidl::unpersist::<fcdecl::Component>(&bytes).unwrap();

        // Component should have 10000 use and expose decls
        let uses = manifest.uses.unwrap();
        let exposes = manifest.exposes.unwrap();
        assert_eq!(uses.len(), 10000);

        for use_ in uses {
            let use_ = use_.fidl_into_native();
            assert!(use_.source_name().as_str().starts_with("use_"));
            assert!(use_.path().unwrap().to_string().starts_with("/svc/capability_"));
        }

        assert_eq!(exposes.len(), 10000);

        for expose in exposes {
            let expose = expose.fidl_into_native();
            assert!(expose.source_name().as_str().starts_with("expose_"));
        }
    }

    #[fuchsia::test]
    async fn structured_config_test() {
        let (config, config_values, checksum) = new_config_decl();

        let components = vec![("root", ComponentDeclBuilder::new().config(config).build())];

        let test = TestEnvironmentBuilder::new()
            .set_components(components)
            .set_config_values(vec![("meta/root.cvf", config_values)])
            .build()
            .await;
        let (query, _host) = realm_query(&test).await;

        test.model.start().await;

        let config = query.get_structured_config("./").await.unwrap().unwrap();

        // Component should have one config field with right value
        assert_eq!(config.fields.len(), 1);
        let field = &config.fields[0];
        assert_eq!(field.key, "my_field");
        assert_matches!(
            field.value,
            fcdecl::ConfigValue::Single(fcdecl::ConfigSingleValue::Bool(true))
        );
        assert_eq!(config.checksum, checksum.native_into_fidl());
    }

    #[fuchsia::test]
    async fn override_structured_config_test() {
        let (config, config_values, checksum) = new_config_decl();

        let components = vec![("root", ComponentDeclBuilder::new().config(config).build())];

        let test = TestEnvironmentBuilder::new()
            .set_components(components)
            .set_config_values(vec![("meta/root.cvf", config_values)])
            .build()
            .await;
        let (query, _host) = realm_query(&test).await;
        let config_override_proxy = config_override(&test).await;

        test.model.start().await;

        let config = query.get_structured_config("./").await.unwrap().unwrap();

        // Component should have one config field with right value
        assert_eq!(config.fields.len(), 1);
        let field = &config.fields[0];
        assert_eq!(field.key, "my_field");
        assert_matches!(
            field.value,
            fcdecl::ConfigValue::Single(fcdecl::ConfigSingleValue::Bool(true))
        );
        assert_eq!(config.checksum, checksum.clone().native_into_fidl());

        // Override the config field value
        config_override_proxy
            .set_structured_config(
                "./",
                &[fcdecl::ConfigOverride {
                    key: Some("my_field".to_string()),
                    value: Some(fcdecl::ConfigValue::Single(fcdecl::ConfigSingleValue::Bool(
                        false,
                    ))),
                    ..Default::default()
                }],
            )
            .await
            .unwrap()
            .unwrap();
        // Unresolve and restart the component so that the configuration override will take effect.
        test.model.root().unresolve().await.unwrap();
        test.model.root().ensure_started(&StartReason::Root).await.unwrap();
        let config = query.get_structured_config("./").await.unwrap().unwrap();

        // Component should have one config field with the override value
        assert_eq!(config.fields.len(), 1);
        let field = &config.fields[0];
        assert_eq!(field.key, "my_field");
        assert_matches!(
            field.value,
            fcdecl::ConfigValue::Single(fcdecl::ConfigSingleValue::Bool(false))
        );
        assert_eq!(config.checksum, checksum.native_into_fidl());
    }

    #[fuchsia::test]
    async fn open_test() {
        let use_decl = UseBuilder::protocol().source(UseSource::Framework).name("foo").build();
        let expose_decl = ExposeBuilder::protocol().source(ExposeSource::Self_).name("bar").build();

        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .use_(use_decl)
                .expose(expose_decl)
                .protocol_default("bar")
                .build(),
        )];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (query, _host) = realm_query(&test).await;

        test.model.start().await;

        let (outgoing_dir, server_end) = create_endpoints::<fio::DirectoryMarker>();
        let server_end = ServerEnd::new(server_end.into_channel());
        query
            .open(
                "./",
                fsys::OpenDirType::OutgoingDir,
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end,
            )
            .await
            .unwrap()
            .unwrap();
        // The test runner has not been configured to serve the outgoing dir, so this directory
        // should just be closed.
        assert!(is_closed(outgoing_dir));

        let (runtime_dir, server_end) = create_endpoints::<fio::DirectoryMarker>();
        let server_end = ServerEnd::new(server_end.into_channel());
        query
            .open(
                "./",
                fsys::OpenDirType::RuntimeDir,
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end,
            )
            .await
            .unwrap()
            .unwrap();
        // The test runner has not been configured to serve the runtime dir, so this directory
        // should just be closed.
        assert!(is_closed(runtime_dir));

        let (pkg_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end = ServerEnd::new(server_end.into_channel());
        query
            .open(
                "./",
                fsys::OpenDirType::PackageDir,
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end,
            )
            .await
            .unwrap()
            .unwrap();

        let (exposed_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end = ServerEnd::new(server_end.into_channel());
        query
            .open(
                "./",
                fsys::OpenDirType::ExposedDir,
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end,
            )
            .await
            .unwrap()
            .unwrap();

        let (svc_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end = ServerEnd::new(server_end.into_channel());
        query
            .open(
                "./",
                fsys::OpenDirType::NamespaceDir,
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                "svc",
                server_end,
            )
            .await
            .unwrap()
            .unwrap();

        // Test resolvers provide a pkg dir with a fake file
        let entries = fuchsia_fs::directory::readdir(&pkg_dir).await.unwrap();
        assert_eq!(
            entries,
            vec![fuchsia_fs::directory::DirEntry {
                name: "fake_file".to_string(),
                kind: fuchsia_fs::directory::DirentKind::File
            }]
        );

        // Component Manager serves the exposed dir with the `bar` protocol
        let entries = fuchsia_fs::directory::readdir(&exposed_dir).await.unwrap();
        assert_eq!(
            entries,
            vec![fuchsia_fs::directory::DirEntry {
                name: "bar".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Service
            }]
        );

        // Component Manager serves the namespace dir with the `foo` protocol.
        let entries = fuchsia_fs::directory::readdir(&svc_dir).await.unwrap();
        assert_eq!(
            entries,
            vec![fuchsia_fs::directory::DirEntry {
                name: "foo".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Service
            }]
        );
    }

    #[fuchsia::test]
    async fn construct_namespace_test() {
        let use_decl = UseBuilder::protocol().source(UseSource::Framework).name("foo").build();

        let components = vec![("root", ComponentDeclBuilder::new().use_(use_decl.clone()).build())];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (query, _host) = realm_query(&test).await;

        test.model.start().await;

        let mut ns = query.construct_namespace("./").await.unwrap().unwrap();

        assert_eq!(ns.len(), 2);
        ns.sort_by_key(|entry| entry.path.as_ref().unwrap().clone());

        // Test resolvers provide a pkg dir with a fake file
        let pkg_entry = ns.remove(0);
        assert_eq!(pkg_entry.path.unwrap(), "/pkg");
        let pkg_dir = pkg_entry.directory.unwrap().into_proxy();

        let entries = fuchsia_fs::directory::readdir(&pkg_dir).await.unwrap();
        assert_eq!(
            entries,
            vec![fuchsia_fs::directory::DirEntry {
                name: "fake_file".to_string(),
                kind: fuchsia_fs::directory::DirentKind::File
            }]
        );

        // The component requested the `foo` protocol.
        let svc_entry = ns.remove(0);
        assert_eq!(svc_entry.path.unwrap(), "/svc");
        let svc_dir = svc_entry.directory.unwrap().into_proxy();

        let entries = fuchsia_fs::directory::readdir(&svc_dir).await.unwrap();
        assert_eq!(
            entries,
            vec![fuchsia_fs::directory::DirEntry {
                name: "foo".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Service
            }]
        );
    }

    #[fuchsia::test]
    async fn get_storage_admin_test() {
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .child_default("a")
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("fs")
                            .source(StorageDirectorySource::Child("a".into()))
                            .subdir("persistent"),
                    )
                    .build(),
            ),
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("fs")
                            .path("/fs/data")
                            .rights(fio::Operations::all()),
                    )
                    .expose(ExposeBuilder::directory().name("fs").source(ExposeSource::Self_))
                    .build(),
            ),
        ];

        let test = TestEnvironmentBuilder::new().set_components(components).build().await;
        let (query, _host) = realm_query(&test).await;

        test.model.start().await;

        let (storage_admin, server_end) = create_proxy::<fsys::StorageAdminMarker>().unwrap();

        query.connect_to_storage_admin("./", "data", server_end).await.unwrap().unwrap();

        let (it_proxy, it_server) =
            create_proxy::<fsys::StorageIteratorMarker>().expect("create iterator");

        storage_admin.list_storage_in_realm("./", it_server).await.unwrap().unwrap();

        let res = it_proxy.next().await.unwrap();
        assert!(res.is_empty());
    }
}
