// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL Worker for the `fuchsia.net.multicast.admin` API.

use derivative::Derivative;
use fidl::endpoints::ProtocolMarker as _;
use fidl_fuchsia_net_multicast_admin as fnet_multicast_admin;
use fidl_fuchsia_net_multicast_ext::{
    FidlMulticastAdminIpExt, FidlResponder as _, TableControllerRequest,
    UnicastSourceAndMulticastDestination,
};
use futures::TryStreamExt as _;
use log::{error, warn};

use crate::bindings::util::ResultExt as _;
use crate::bindings::{BindingsCtx, Ctx};

/// Serves the multicast routing table controller FIDL for ip version `I`.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
pub(crate) async fn serve_table_controller<I: FidlMulticastAdminIpExt + netstack3_core::IpExt>(
    mut ctx: Ctx,
    request_stream: I::TableControllerRequestStream,
) {
    // TODO(https://fxbug.dev/353330225): Associate the lifetime of this channel
    // with whether multicast forwarding is enabled in core.
    let _newly_enabled = ctx.api().multicast_forwarding::<I>().enable();
    let mut watcher = MulticastRoutingEventsWatcher::<I>::default();
    request_stream
        .try_for_each(|request| {
            match request.into() {
                TableControllerRequest::AddRoute { addresses, route, responder } => {
                    handle_add_route(addresses, route, responder)
                }
                TableControllerRequest::DelRoute { addresses, responder } => {
                    handle_del_route(addresses, responder)
                }
                TableControllerRequest::GetRouteStats { addresses, responder } => {
                    handle_get_route_stats(addresses, responder)
                }
                TableControllerRequest::WatchRoutingEvents { responder } => {
                    handle_watch_routing_events::<I>(responder, &mut watcher)
                }
            }
            futures::future::ready(Ok(()))
        })
        .await
        .unwrap_or_else(|e| {
            if !e.is_closed() {
                error!("{} request error {e:?}", I::TableControllerMarker::DEBUG_NAME);
            }
        })
}

/// State associated with the hanging-get watcher for multicast routing events.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
struct MulticastRoutingEventsWatcher<I: FidlMulticastAdminIpExt> {
    parked_watch_request: Option<I::WatchRoutingEventsResponder>,
}

/// Add a multicast route to the Netstack.
fn handle_add_route<I: FidlMulticastAdminIpExt>(
    addresses: UnicastSourceAndMulticastDestination<I>,
    route: fnet_multicast_admin::Route,
    responder: I::AddRouteResponder,
) {
    // TODO(https://fxbug.dev/323052525): Support adding multicast routes.
    warn!("not adding multicast route; unimplemented: addresses={addresses:?}, route={route:?}");
    responder.try_send(Ok(())).unwrap_or_log("failed to respond")
}

/// Remove a multicast route from the Netstack.
fn handle_del_route<I: FidlMulticastAdminIpExt>(
    addresses: UnicastSourceAndMulticastDestination<I>,
    responder: I::DelRouteResponder,
) {
    // TODO(https://fxbug.dev/323052525): Support deleting multicast routes.
    warn!("not deleting multicast route; unimplemented: addresses={addresses:?}");
    responder.try_send(Ok(())).unwrap_or_log("failed to respond")
}

/// Get usage statistics for a multicast route.
fn handle_get_route_stats<I: FidlMulticastAdminIpExt>(
    _addresses: UnicastSourceAndMulticastDestination<I>,
    responder: I::GetRouteStatsResponder,
) {
    // TODO(https://fxbug.dev/323052525): Support getting multicast route stats.
    let stats = fnet_multicast_admin::RouteStats::default();
    responder.try_send(Ok(&stats)).unwrap_or_log("failed to respond")
}

/// Watch for multicast routing events.
fn handle_watch_routing_events<I: FidlMulticastAdminIpExt>(
    responder: I::WatchRoutingEventsResponder,
    watcher: &mut MulticastRoutingEventsWatcher<I>,
) {
    // TODO(https://fxbug.dev/323052525): Support watching multicast routing
    // events.
    warn!("not publishing multicast routing events; unimplemented");
    let _old_parked_request = watcher.parked_watch_request.replace(responder);
}
