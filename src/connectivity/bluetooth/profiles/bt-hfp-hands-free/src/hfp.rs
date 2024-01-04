// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use fidl_fuchsia_bluetooth_bredr as bredr;
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_async as fasync;
use fuchsia_bluetooth::profile::ProtocolDescriptor;
use fuchsia_bluetooth::types::PeerId;
use futures::channel::mpsc;
use futures::select;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use profile_client::{ProfileClient, ProfileEvent};
use std::collections::HashMap;
use std::future::Future;
use std::marker::Unpin;
use tracing::{debug, info};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::Peer;

#[cfg(test)]
mod tests;

pub const SEARCH_RESULT_CONNECT_DELAY_SECONDS: i64 = 1;
const SEARCH_RESULT_CONNECT_DELAY_DURATION: fasync::Duration =
    fasync::Duration::from_seconds(SEARCH_RESULT_CONNECT_DELAY_SECONDS);

type SearchResultTimer =
    Box<dyn Future<Output = (PeerId, Option<Vec<ProtocolDescriptor>>)> + Unpin>;

pub struct Hfp {
    config: HandsFreeFeatureSupport,
    /// Provides Hfp with a means to drive the `fuchsia.bluetooth.bredr` related APIs.
    profile_client: ProfileClient,
    /// The client connection to the `fuchsia.bluetooth.bredr.Profile` protocol.
    profile_svc: bredr::ProfileProxy,
    /// A collection of discovered and/or connected Bluetooth peers that support the AG role.
    // TODO(https://fxbug.dev/132337) Convert this to a FutureMap and await peer tasks finishing and clean up.
    peers: HashMap<PeerId, Peer>,
    /// Timers for asynchronously handling search result profile events.
    search_result_timers: FuturesUnordered<SearchResultTimer>,
}

impl Hfp {
    pub fn new(
        profile_client: ProfileClient,
        profile_svc: bredr::ProfileProxy,
        config: HandsFreeFeatureSupport,
        _hfp_stream_receiver: mpsc::Receiver<fidl_hfp::HandsFreeRequestStream>,
    ) -> Self {
        let peers = HashMap::new();
        let search_result_timers = FuturesUnordered::new();
        Self { profile_client, profile_svc, config, peers, search_result_timers }
    }

    /// Handle incoming profile events until the stream finishes or has an error.
    pub async fn run(mut self) -> Result<(), Error> {
        loop {
            select! {
                profile_event_result_option = self.profile_client.next() => {
                    debug!("Received profile event: {:?}", profile_event_result_option);
                    let profile_event_result = profile_event_result_option
                        .ok_or(format_err!("Profile client stream closed."))?;
                    let profile_event = profile_event_result?;
                    self.handle_profile_event(profile_event);
                },
                (peer_id, protocol) = self.search_result_timers.select_next_some() => {
                    debug!("Timer for search result from peer {} expired.", peer_id);
                    self.handle_search_result_timer_expiry(peer_id, protocol).await;
                }
            }
        }
    }

    fn handle_profile_event(&mut self, event: ProfileEvent) {
        let peer_id = event.peer_id();

        let peer = self
            .peers
            .entry(peer_id)
            .or_insert_with(|| Peer::new(peer_id, self.config, self.profile_svc.clone()));

        match event {
            ProfileEvent::PeerConnected { channel, .. } => {
                info!("Received peer_connected for peer {}.", peer_id);
                peer.handle_peer_connected(channel);
            }
            ProfileEvent::SearchResult { protocol, .. } => {
                info!("Received search results for peer {}", peer_id);

                if peer.task_exists() {
                    debug!(
                        "Peer task already created by previous profile event for peer {}",
                        peer_id
                    );
                } else {
                    debug!("Creating peer task for peer {}", peer_id);

                    // Convert FIDL ProtocolDescriptor to BT ProtocolDescriptor.
                    let protocol =
                        protocol.map(|p| p.iter().map(ProtocolDescriptor::from).collect());

                    let search_result_timer = Self::search_result_timer(peer_id, protocol);
                    self.search_result_timers.push(search_result_timer);
                }
            }
        }
    }

    // We expect peers to connect to us.  If they don't connect to us but we get
    // a search result, we should connect to them.  To prevent races where both
    // we and the remote peer attempt to connect to the other simultaneously, we
    // delay connecting after receiving a search result and see if the remote
    // peer has connected first.
    fn search_result_timer(
        peer_id: PeerId,
        protocol: Option<Vec<ProtocolDescriptor>>,
    ) -> SearchResultTimer {
        let time = fasync::Time::after(SEARCH_RESULT_CONNECT_DELAY_DURATION);
        let timer = fasync::Timer::new(time);

        let fut = FutureExt::map(timer, move |_| (peer_id, protocol));

        Box::new(fut)
    }

    async fn handle_search_result_timer_expiry(
        &mut self,
        peer_id: PeerId,
        protocol: Option<Vec<ProtocolDescriptor>>,
    ) {
        let peer_result = self.peers.get_mut(&peer_id);

        match peer_result {
            None => {
                info!("Peer task for peer {} completed before handling search result.", peer_id)
            }
            Some(peer) => peer.handle_search_result(protocol).await,
        }
    }
}
