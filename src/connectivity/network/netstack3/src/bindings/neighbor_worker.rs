// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{ProtocolMarker as _, ServerEnd};
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_neighbor::{
    self as fnet_neighbor, ControllerRequest, ControllerRequestStream, ViewRequest,
    ViewRequestStream,
};
use fuchsia_zircon as zx;

use futures::{FutureExt as _, TryStreamExt as _};
use net_types::ip::{IpAddr, Ipv4, Ipv6};
use tracing::warn;

use crate::bindings::{devices::BindingId, BindingsNonSyncCtxImpl, Ctx, Netstack};
use netstack3_core::{
    device::{insert_static_neighbor_entry, remove_neighbor_table_entry},
    error::{NeighborRemovalError, NotFoundError, NotSupportedError, StaticNeighborInsertionError},
};

pub(super) async fn serve(ns: Netstack, stream: ViewRequestStream) -> Result<(), fidl::Error> {
    stream
        .try_for_each_concurrent(None, |request| async {
            match request {
                ViewRequest::OpenEntryIterator { it, options, control_handle: _ } => {
                    let fnet_neighbor::EntryIteratorOptions { .. } = options;
                    serve_watcher(ns.clone(), it)
                        .map(|result| {
                            Ok(result.unwrap_or_else(|e| {
                                warn!(
                                    "error serving {}: {:?}",
                                    fnet_neighbor::EntryIteratorMarker::DEBUG_NAME,
                                    e
                                )
                            }))
                        })
                        .await
                }
                ViewRequest::GetUnreachabilityConfig { interface, ip_version, responder } => {
                    warn!("not responding to GetUnreachabilityConfig");
                    let _ = (interface, ip_version, responder);
                    Ok(())
                }
            }
        })
        .await
}

async fn serve_watcher(
    _netstack: Netstack,
    _it: ServerEnd<fnet_neighbor::EntryIteratorMarker>,
) -> Result<(), fidl::Error> {
    warn!("blocking forever serving {}", fnet_neighbor::EntryIteratorMarker::DEBUG_NAME);
    futures::future::pending().await
}

pub(super) async fn serve_controller(
    ctx: Ctx,
    stream: ControllerRequestStream,
) -> Result<(), fidl::Error> {
    stream
        .try_for_each(|request| async {
            let mut ctx = ctx.clone();
            let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();

            match request {
                ControllerRequest::AddEntry { interface, neighbor, mac, responder } => {
                    let Some(device_id) = BindingId::new(interface)
                        .and_then(|id| non_sync_ctx.devices.get_core_id(id))
                    else {
                        return responder.send(Err(zx::Status::NOT_FOUND.into_raw()));
                    };
                    let mac = mac.into_ext();
                    let result = match neighbor.into_ext() {
                        IpAddr::V4(v4) => insert_static_neighbor_entry::<
                            Ipv4,
                            BindingsNonSyncCtxImpl,
                        >(
                            sync_ctx, non_sync_ctx, &device_id, v4, mac
                        ),
                        IpAddr::V6(v6) => insert_static_neighbor_entry::<
                            Ipv6,
                            BindingsNonSyncCtxImpl,
                        >(
                            sync_ctx, non_sync_ctx, &device_id, v6, mac
                        ),
                    }
                    .map_err(|e| match e {
                        StaticNeighborInsertionError::MacAddressNotUnicast
                        | StaticNeighborInsertionError::IpAddressInvalid => {
                            zx::Status::INVALID_ARGS.into_raw()
                        }
                        StaticNeighborInsertionError::NotSupported(NotSupportedError) => {
                            zx::Status::NOT_SUPPORTED.into_raw()
                        }
                    });
                    responder.send(result)
                }
                ControllerRequest::RemoveEntry { interface, neighbor, responder } => {
                    let Some(device_id) = BindingId::new(interface)
                        .and_then(|id| non_sync_ctx.devices.get_core_id(id))
                    else {
                        return responder.send(Err(zx::Status::NOT_FOUND.into_raw()));
                    };
                    let result =
                        match neighbor.into_ext() {
                            IpAddr::V4(v4) => remove_neighbor_table_entry::<
                                Ipv4,
                                BindingsNonSyncCtxImpl,
                            >(
                                sync_ctx, non_sync_ctx, &device_id, v4
                            ),
                            IpAddr::V6(v6) => remove_neighbor_table_entry::<
                                Ipv6,
                                BindingsNonSyncCtxImpl,
                            >(
                                sync_ctx, non_sync_ctx, &device_id, v6
                            ),
                        }
                        .map_err(|e| {
                            match e {
                                NeighborRemovalError::IpAddressInvalid => zx::Status::INVALID_ARGS,
                                NeighborRemovalError::NotFound(NotFoundError) => {
                                    zx::Status::NOT_FOUND
                                }
                                NeighborRemovalError::NotSupported(NotSupportedError) => {
                                    zx::Status::NOT_SUPPORTED
                                }
                            }
                            .into_raw()
                        });
                    responder.send(result)
                }
                ControllerRequest::ClearEntries { interface: _, ip_version: _, responder } => {
                    warn!(
                        "TODO(https://fxbug.dev/124960): \
                            Implement fuchsia.net.neighbor/Controller.ClearEntries"
                    );
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                }
                ControllerRequest::UpdateUnreachabilityConfig {
                    interface: _,
                    ip_version: _,
                    config: _,
                    responder,
                } => {
                    warn!(
                        "TODO(https://fxbug.dev/124960): \
                            Implement fuchsia.net.neighbor/Controller.UpdateUnreachabilityConfig"
                    );
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                }
            }
        })
        .await
}
