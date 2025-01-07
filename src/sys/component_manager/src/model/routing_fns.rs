// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::WeakComponentInstance;
use crate::model::routing::{self};
use ::routing::RouteRequest;
use fidl_fuchsia_io as fio;
use log::error;
use router_error::Explain;
use std::sync::Arc;
use vfs::directory::entry::{
    DirectoryEntry, DirectoryEntryAsync, EntryInfo, GetEntryInfo, OpenRequest,
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
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.spawn(self);
        Ok(())
    }
}

impl GetEntryInfo for RouteEntry {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
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

        routing::route_and_open_capability_with_reporting(&self.request, &component, request)
            .await
            .map_err(|e| e.as_zx_status())
    }
}
