// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::WeakComponentInstance,
        routing::{self, OpenOptions, RouteRequest},
    },
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::sync::Arc,
    tracing::error,
    vfs::{
        directory::entry::{DirectoryEntry, EntryInfo, OpenRequest},
        execution_scope::ExecutionScope,
        path::Path,
        remote::RemoteLike,
        ObjectRequestRef, ToObjectRequest,
    },
};

pub struct RouteEntry {
    component: WeakComponentInstance,
    request: RouteRequest,
    entry_type: fio::DirentType,
}

impl RouteEntry {
    pub fn new(
        component: WeakComponentInstance,
        request: RouteRequest,
        entry_type: fio::DirentType,
    ) -> Arc<Self> {
        Arc::new(Self { component, request, entry_type })
    }
}

impl DirectoryEntry for RouteEntry {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_remote(self)
    }
}

impl RemoteLike for RouteEntry {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let this = self.clone();
        scope.clone().spawn(async move {
            flags.to_object_request(server_end).handle(|object_request| {
                let component = match this.component.upgrade() {
                    Ok(component) => component,
                    Err(e) => {
                        // This can happen if the component instance tree topology changes such
                        // that the captured `component` no longer exists.
                        error!(
                            "failed to upgrade WeakComponentInstance while routing {}: {:?}",
                            this.request, e
                        );
                        return Err(e.as_zx_status());
                    }
                };

                let mut server_end = object_request.take().into_channel();

                scope.spawn(async move {
                    let open_options = OpenOptions {
                        flags,
                        relative_path: path.into_string(),
                        server_chan: &mut server_end,
                    };
                    let res =
                        routing::route_and_open_capability(&this.request, &component, open_options)
                            .await;
                    if let Err(e) = res {
                        routing::report_routing_failure(&this.request, &component, e, server_end)
                            .await;
                    }
                });

                Ok(())
            });
        });
    }

    fn open2(
        self: Arc<Self>,
        _scope: ExecutionScope,
        _path: Path,
        _protocols: fio::ConnectionProtocols,
        _object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}
