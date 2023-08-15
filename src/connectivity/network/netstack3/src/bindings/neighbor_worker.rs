// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{ProtocolMarker as _, ServerEnd};
use fidl_fuchsia_net_neighbor::{self as fnet_neighbor, ViewRequest, ViewRequestStream};

use futures::{FutureExt as _, TryStreamExt as _};
use tracing::warn;

use crate::bindings::Netstack;

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
