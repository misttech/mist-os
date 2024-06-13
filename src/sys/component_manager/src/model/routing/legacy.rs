// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::routing;
use crate::model::routing_fns::RouteEntry;
use ::routing::capability_source::ComponentCapability;
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::rights::Rights;
use ::routing::RouteRequest;
use cm_rust::{
    CapabilityTypeName, ExposeDecl, UseDirectoryDecl, UseEventStreamDecl, UseStorageDecl,
};
use router_error::Explain;
use sandbox::{Capability, Directory, Open};
use std::sync::Arc;
use tracing::*;
use vfs::directory::entry::{
    serve_directory, DirectoryEntry, DirectoryEntryAsync, EntryInfo, OpenRequest,
};
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_io as fio, fuchsia_zircon as zx};

pub trait RouteRequestExt {
    fn into_capability(self, target: &Arc<ComponentInstance>) -> Capability;
}

enum UseDirectoryOrStorage {
    Directory(UseDirectoryDecl),
    Storage(UseStorageDecl),
}

impl From<UseDirectoryOrStorage> for RouteRequest {
    fn from(r: UseDirectoryOrStorage) -> Self {
        match r {
            UseDirectoryOrStorage::Directory(d) => Self::UseDirectory(d),
            UseDirectoryOrStorage::Storage(d) => Self::UseStorage(d),
        }
    }
}

impl RouteRequestExt for RouteRequest {
    fn into_capability(self, target: &Arc<ComponentInstance>) -> Capability {
        match self {
            Self::UseService(decl) => use_service(decl, target),
            Self::UseDirectory(decl) => {
                use_directory_or_storage(UseDirectoryOrStorage::Directory(decl), target)
            }
            Self::UseStorage(decl) => {
                use_directory_or_storage(UseDirectoryOrStorage::Storage(decl), target)
            }
            Self::UseEventStream(decl) => use_event_stream(decl, target),
            Self::UseProtocol(_) => {
                panic!("Protocols should use bedrock instead");
            }
            Self::ExposeProtocol(ref e) => {
                let cap = ComponentCapability::Expose(ExposeDecl::Protocol(e.clone()));
                expose_any(self, target, cap.type_name())
            }
            Self::ExposeService(ref e) => {
                let cap = ComponentCapability::Expose(ExposeDecl::Service(
                    e.iter().next().unwrap().clone(),
                ));
                expose_any(self, target, cap.type_name())
            }
            Self::ExposeDirectory(ref e) => {
                let cap = ComponentCapability::Expose(ExposeDecl::Directory(e.clone()));
                expose_any(self, target, cap.type_name())
            }
            _ => {
                panic!("Capability conversion is not supported for {:?}", self);
            }
        }
    }
}

fn expose_any(
    request: RouteRequest,
    target: &Arc<ComponentInstance>,
    type_name: CapabilityTypeName,
) -> Capability {
    Open::new(RouteEntry::new(target.as_weak(), request, type_name.into())).into()
}

fn use_service(decl: cm_rust::UseServiceDecl, target: &Arc<ComponentInstance>) -> Capability {
    struct Service {
        target: WeakComponentInstance,
        scope: ExecutionScope,
        decl: cm_rust::UseServiceDecl,
    }
    impl DirectoryEntry for Service {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }

        fn open_entry(self: Arc<Self>, mut request: OpenRequest<'_>) -> Result<(), zx::Status> {
            // Move this request from the namespace scope to the component's scope so that
            // we don't block namespace teardown.
            request.set_scope(self.scope.clone());
            request.spawn(self);
            Ok(())
        }
    }
    impl DirectoryEntryAsync for Service {
        async fn open_entry_async(
            self: Arc<Self>,
            request: OpenRequest<'_>,
        ) -> Result<(), zx::Status> {
            if request.path().is_empty() {
                if !request.wait_till_ready().await {
                    return Ok(());
                }
            }

            let target = match self.target.upgrade() {
                Ok(component) => component,
                Err(e) => {
                    error!(
                        "failed to upgrade WeakComponentInstance routing use \
                                 decl `{:?}`: {:?}",
                        self.decl, e
                    );
                    return Err(e.as_zx_status());
                }
            };

            // Hold a guard to prevent this task from being dropped during component
            // destruction.
            let _guard = request.scope().active_guard();

            let route_request = RouteRequest::UseService(self.decl.clone());

            routing::route_and_open_capability_with_reporting(&route_request, &target, request)
                .await
                .map_err(|e| e.as_zx_status())
        }
    }
    Open::new(Arc::new(Service {
        target: target.as_weak(),
        scope: target.execution_scope.clone(),
        decl,
    }))
    .into()
}

/// Makes a capability representing the directory described by `use_`. Once the
/// channel is readable, the future calls `route_directory` to forward the channel to the
/// source component's outgoing directory and terminates.
///
/// `component` is a weak pointer, which is important because we don't want the task
/// waiting for channel readability to hold a strong pointer to this component lest it
/// create a reference cycle.
fn use_directory_or_storage(
    request: UseDirectoryOrStorage,
    target: &Arc<ComponentInstance>,
) -> Capability {
    let flags = match &request {
        UseDirectoryOrStorage::Directory(decl) => Rights::from(decl.rights).into_legacy(),
        UseDirectoryOrStorage::Storage(_) => {
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE
        }
    };

    // Specify that the capability must be opened as a directory. In particular, this affects
    // how a devfs-based capability will handle the open call. If this flag is not specified,
    // devfs attempts to open the directory as a service, which is not what is desired here.
    let flags = flags | fio::OpenFlags::DIRECTORY;

    struct RouteDirectory {
        target: WeakComponentInstance,
        request: RouteRequest,
    }

    impl DirectoryEntry for RouteDirectory {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }

        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
            request.spawn(self);
            Ok(())
        }
    }

    impl DirectoryEntryAsync for RouteDirectory {
        async fn open_entry_async(
            self: Arc<Self>,
            request: OpenRequest<'_>,
        ) -> Result<(), zx::Status> {
            if request.path().is_empty() {
                if !request.wait_till_ready().await {
                    return Ok(());
                }
            }

            // Hold a guard to prevent this task from being dropped during component destruction.
            let _guard = request.scope().active_guard();

            let target = match self.target.upgrade() {
                Ok(component) => component,
                Err(e) => {
                    error!(
                        "failed to upgrade WeakComponentInstance routing use \
                         decl `{:?}`: {:?}",
                        self.request, e
                    );
                    return Err(e.as_zx_status());
                }
            };

            routing::route_and_open_capability_with_reporting(&self.request, &target, request)
                .await
                .map_err(|e| e.as_zx_status())
        }
    }

    // Serve this directory on the component's execution scope rather than the namespace execution
    // scope so that requests don't block block namespace teardown, but they will block component
    // destruction.
    Directory::new(
        serve_directory(
            Arc::new(RouteDirectory { request: request.into(), target: target.as_weak() }),
            &target.execution_scope.clone(),
            flags,
        )
        .unwrap(),
    )
    .into()
}

fn use_event_stream(decl: UseEventStreamDecl, target: &Arc<ComponentInstance>) -> Capability {
    struct UseEventStream {
        component: WeakComponentInstance,
        decl: UseEventStreamDecl,
    }
    impl DirectoryEntry for UseEventStream {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Service)
        }
        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
            if !request.path().is_empty() {
                return Err(zx::Status::NOT_DIR);
            }
            request.spawn(self);
            Ok(())
        }
    }
    impl DirectoryEntryAsync for UseEventStream {
        async fn open_entry_async(
            self: Arc<Self>,
            mut request: OpenRequest<'_>,
        ) -> Result<(), zx::Status> {
            let component = match self.component.upgrade() {
                Ok(component) => component,
                Err(e) => {
                    error!(
                        "failed to upgrade WeakComponentInstance routing use \
                                 decl `{:?}`: {:?}",
                        self.decl, e
                    );
                    return Err(e.as_zx_status());
                }
            };

            request.prepend_path(&self.decl.target_path.to_string().try_into()?);
            let route_request = RouteRequest::UseEventStream(self.decl.clone());
            routing::route_and_open_capability_with_reporting(&route_request, &component, request)
                .await
                .map_err(|e| e.as_zx_status())
        }
    }
    Open::new(Arc::new(UseEventStream { component: target.as_weak(), decl })).into()
}
