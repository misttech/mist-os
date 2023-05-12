// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of Linux's Netlink API for Fuchsia.
//!
//! Netlink is a socket-based API provided by Linux that user space applications
//! can use to interact with the kernel. The API is split up into several
//! protocol families each offering different functionality. This crate targets
//! the implementation of families related to networking.

#![deny(unused, missing_docs)]

mod client;
mod multicast_groups;
pub mod protocol_family;

use futures::{
    channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
    future::Future,
    StreamExt as _,
};

use crate::{
    client::InternalClient,
    protocol_family::route::{NetlinkRoute, NetlinkRouteClient},
};

/// The implementation of the Netlink protocol suite.
pub struct Netlink {
    /// Sender to attach new `NETLINK_ROUTE` clients to the event loop.
    route_client_sender: UnboundedSender<InternalClient<NetlinkRoute>>,
}

impl Netlink {
    /// Returns a newly instantiated [`Netlink`] and it's associated event loop.
    ///
    /// Callers are responsible for polling the event loop, which drives
    /// the Netlink implementation's asynchronous work. The event loop will
    /// never complete.
    pub fn new() -> (Self, impl Future<Output = ()> + Send) {
        let (route_client_sender, route_client_receiver) = mpsc::unbounded();
        (Netlink { route_client_sender }, run_event_loop(EventLoopParams { route_client_receiver }))
    }

    /// Creates a new client of the `NETLINK_ROUTE` protocol family.
    pub fn new_route_client(&self) -> Result<NetlinkRouteClient, NewClientError> {
        let (external_client, internal_client) = client::new_client_pair::<NetlinkRoute>();
        self.route_client_sender.unbounded_send(internal_client).map_err(|e| {
            // Sending on an `UnboundedSender` can never fail with `is_full()`.
            debug_assert!(e.is_disconnected());
            NewClientError::Disconnected
        })?;
        Ok(NetlinkRouteClient(external_client))
    }
}

/// The possible error types when instantiating a new client.
pub enum NewClientError {
    /// The [`Netlink`] is disconnected from its associated event loop, perhaps
    /// as a result of dropping the event loop.
    Disconnected,
}

/// Parameters used to start the event loop.
struct EventLoopParams {
    /// Receiver of newly created `NETLINK_ROUTE` clients.
    route_client_receiver: UnboundedReceiver<InternalClient<NetlinkRoute>>,
}

/// The event loop encompassing all asynchronous Netlink work.
///
/// The event loop is never expected to complete.
async fn run_event_loop(params: EventLoopParams) {
    let EventLoopParams { route_client_receiver } = params;

    // TODO(https://issuetracker.google.com/280483454): Notify route clients of
    // multicast group events.
    let _route_clients = route_client_receiver.collect::<Vec<_>>().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::FutureExt as _;

    // Placeholder test to ensure the build targets are setup properly.
    #[test]
    fn test_event_loop() {
        let (_route_client_sender, route_client_receiver) = mpsc::unbounded();
        assert_eq!(run_event_loop(EventLoopParams { route_client_receiver }).now_or_never(), None);
    }
}
