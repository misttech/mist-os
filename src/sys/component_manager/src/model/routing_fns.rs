// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::WeakComponentInstance,
        routing::{self, OpenOptions, RouteRequest},
    },
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    tracing::error,
    vfs::{execution_scope::ExecutionScope, path::Path},
};

pub type RoutingFn = Box<dyn Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) + Send + Sync>;

pub fn route_fn(component: WeakComponentInstance, request: RouteRequest) -> RoutingFn {
    Box::new(
        move |scope: ExecutionScope,
              flags: fio::OpenFlags,
              path: Path,
              mut server_end: zx::Channel| {
            let component = component.clone();
            let request = request.clone();
            scope.spawn(async move {
                let component = match component.upgrade() {
                    Ok(component) => component,
                    Err(e) => {
                        // This can happen if the component instance tree topology changes such
                        // that the captured `component` no longer exists.
                        error!(
                            "failed to upgrade WeakComponentInstance while routing {}: {:?}",
                            request, e
                        );
                        return;
                    }
                };

                let open_options = OpenOptions {
                    flags,
                    relative_path: path.into_string(),
                    server_chan: &mut server_end,
                };
                let res =
                    routing::route_and_open_capability(&request, &component, open_options).await;
                if let Err(e) = res {
                    routing::report_routing_failure(&request, &component, e, server_end).await;
                }
            });
        },
    )
}
