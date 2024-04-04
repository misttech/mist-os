// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::WeakComponentInstance,
        routing::{self, RouteRequest},
    },
    bedrock_error::Explain,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::sync::Arc,
    tracing::error,
    vfs::directory::entry::{DirectoryEntry, DirectoryEntryAsync, EntryInfo, OpenRequest},
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
        request.spawn(self);
        Ok(())
    }
}

impl DirectoryEntryAsync for RouteEntry {
    async fn open_entry_async(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        let component = match self.component.upgrade() {
            Ok(component) => component,
            Err(e) => {
                // This can happen if the component instance tree topology changes such
                // that the captured `component` no longer exists.
                error!(
                    "failed to upgrade WeakComponentInstance while routing {}: {:?}",
                    self.request, e
                );
                return Err(e.as_zx_status());
            }
        };

        if let Err(e) = routing::route_and_open_capability(&self.request, &component, request).await
        {
            routing::report_routing_failure(&self.request, &component, &e).await;
            Err(e.as_zx_status())
        } else {
            Ok(())
        }
    }
}
