// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use async_helpers::maybe_stream::MaybeStream;
use fidl::endpoints::{ControlHandle, RequestStream, Responder};
use fuchsia_bluetooth::profile::ProtocolDescriptor;
use fuchsia_bluetooth::types::PeerId;
use futures::stream::{FusedStream, FuturesUnordered};
use futures::{select, FutureExt, StreamExt};
use profile_client::{ProfileClient, ProfileEvent};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tracing::{debug, info, warn};
use {
    fidl_fuchsia_bluetooth_bredr as bredr, fidl_fuchsia_bluetooth_hfp as fidl_hfp,
    fuchsia_async as fasync,
};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::Peer;

#[cfg(test)]
mod tests;

pub const SEARCH_RESULT_CONNECT_DELAY_SECONDS: i64 = 1;
const SEARCH_RESULT_CONNECT_DELAY_DURATION: fasync::MonotonicDuration =
    fasync::MonotonicDuration::from_seconds(SEARCH_RESULT_CONNECT_DELAY_SECONDS);

type SearchResultTimer = Pin<Box<dyn Future<Output = (PeerId, Option<Vec<ProtocolDescriptor>>)>>>;

/// Toplevel struct containing the streams of incoming events that are not specific to a single
/// peer.
///
/// The Stream of incoming HandsFree FIDL protocol connections should be instantiated as a
/// ServiceFs for non-test code or with another stream implementation for testing.
pub struct Hfp<S>
where
    S: FusedStream<Item = fidl_hfp::HandsFreeRequestStream> + Unpin + 'static,
{
    /// Configuration for which HF features we support.
    features: HandsFreeFeatureSupport,
    /// Provides Hfp with a means to drive the `fuchsia.bluetooth.bredr` related APIs.
    profile_client: ProfileClient,
    /// The client connection to the `fuchsia.bluetooth.bredr.Profile` protocol.
    profile_proxy: bredr::ProfileProxy,
    /// Timers for asynchronously handling search result profile events.
    search_result_timers: FuturesUnordered<SearchResultTimer>,
    /// A stream of incoming HandsFree FIDL protocol connections. This should be instantiated as a
    /// ServiceFs for live code and as some other stream for testing.
    hands_free_connection_stream: S,
    /// Stream of incoming HandsFree FIDL protocol Requests
    hands_free_request_maybe_stream: MaybeStream<fidl_hfp::HandsFreeRequestStream>,
    /// A collection of discovered and/or connected Bluetooth peers that support the AG role.
    // TODO(https://fxbug.dev/42082435) Convert this to a FutureMap and await peer tasks finishing and clean up.
    peers: HashMap<PeerId, Peer>,
    // TODO(fxb/127364) Update HangingGet with peer, and delete this which just keeps the proxy
    // around to make tests pass.
    peer_handler_proxies: Vec<fidl_hfp::PeerHandlerProxy>,
}

impl<S> Hfp<S>
where
    S: FusedStream<Item = fidl_hfp::HandsFreeRequestStream> + Unpin + 'static,
{
    pub fn new(
        features: HandsFreeFeatureSupport,
        profile_client: ProfileClient,
        profile_proxy: bredr::ProfileProxy,
        hands_free_connection_stream: S,
    ) -> Self {
        let search_result_timers = FuturesUnordered::new();
        let hands_free_connection_stream = hands_free_connection_stream;
        let hands_free_request_maybe_stream = MaybeStream::default();
        let peers = HashMap::new();
        Self {
            features,
            profile_client,
            profile_proxy,
            hands_free_connection_stream,
            hands_free_request_maybe_stream,
            peers,
            search_result_timers,
            peer_handler_proxies: Vec::new(),
        }
    }

    /// Handle incoming profile events, HFP FIDL streams from new client connections and HandsFree
    /// FIDL protocol events. This is all the incoming events that are not specific to a single
    /// peer.
    pub async fn run(mut self) -> Result<()> {
        loop {
            select! {
                profile_event_result_option = self.profile_client.next() => {
                    debug!("Received profile event: {:?}", profile_event_result_option);
                    let profile_event_result = profile_event_result_option
                        .ok_or_else(|| format_err!("Profile client stream closed."))?;
                    let profile_event = profile_event_result?;
                    self.handle_profile_event(profile_event)?;
                },

                (peer_id, protocol) = self.search_result_timers.select_next_some() => {
                    debug!("Timer for search result from peer {} expired.", peer_id);
                    self.handle_search_result_timer_expiry(peer_id, protocol).await;
                },

                hands_free_request_stream_option = self.hands_free_connection_stream.next() => {
                    let stream_str = hands_free_request_stream_option
                     .as_ref().map(|_stream| "<stream>");
                    debug!("HandsFree FIDL protocol client connected: {:?}", stream_str);
                    let hands_free_request_stream = hands_free_request_stream_option
                        .ok_or_else(|| format_err!("HandsFree FIDL protocol connection stream closed."))?;
                    self.handle_hands_free_request_stream(hands_free_request_stream)
                },

                hands_free_request_option = self.hands_free_request_maybe_stream.next() => {
                    debug!("Received HandsFree FIDL protocol request: {:?}",
                        hands_free_request_option);
                    if let Some(Ok(hands_free_request)) = hands_free_request_option {
                        self.handle_hands_free_request(hands_free_request)
                    } else {
                        warn!("Dropping HandsFree FIDL protocol request stream");
                        let _old_stream =
                            MaybeStream::take(&mut self.hands_free_request_maybe_stream);
                    }
                }
            }
        }
    }

    fn handle_profile_event(&mut self, event: ProfileEvent) -> Result<()> {
        let peer_id = event.peer_id();

        let peer = self
            .peers
            .entry(peer_id)
            .or_insert_with(|| Peer::new(peer_id, self.features, self.profile_proxy.clone()));

        match event {
            ProfileEvent::PeerConnected { channel, .. } => {
                info!("Received peer_connected for peer {}.", peer_id);
                let peer_handler_proxy = peer.handle_peer_connected(channel);
                self.report_peer_handler(peer_handler_proxy)
            }
            ProfileEvent::SearchResult { protocol, .. } => {
                debug!("Received search results for peer {}", peer_id);

                if peer.task_exists() {
                    debug!(
                        "Peer task already created by previous profile event for peer {}",
                        peer_id
                    );
                } else {
                    debug!("Setting timer for peer task search results for peer {}", peer_id);

                    // Convert FIDL ProtocolDescriptor to BT ProtocolDescriptor.
                    let protocol = protocol.map_or(Ok(None), |p| {
                        p.iter()
                            .map(|p| ProtocolDescriptor::try_from(p))
                            .collect::<Result<Vec<_>, _>>()
                            .map(|p| Some(p))
                    })?;

                    let search_result_timer = Self::search_result_timer(peer_id, protocol);
                    self.search_result_timers.push(search_result_timer);
                }
            }
        }
        Ok(())
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
        let time = fasync::MonotonicInstant::after(SEARCH_RESULT_CONNECT_DELAY_DURATION);
        let timer = fasync::Timer::new(time);

        let fut = FutureExt::map(timer, move |_| (peer_id, protocol));

        Box::pin(fut)
    }

    async fn handle_search_result_timer_expiry(
        &mut self,
        peer_id: PeerId,
        protocol: Option<Vec<ProtocolDescriptor>>,
    ) {
        debug!("Handle search results timer expired for peer {:?}", peer_id);

        let peer_result = self.peers.get_mut(&peer_id);

        let peer_handler_proxy_result = match peer_result {
            None => {
                info!("Peer task for peer {} completed before handling search result.", peer_id);
                Ok(None)
            }
            Some(peer) => peer.handle_search_result(protocol).await,
        };

        let peer_handler_proxy_option = match peer_handler_proxy_result {
            Ok(proxy) => proxy,
            Err(err) => {
                // An error handling one peer should not be a fatal error.
                warn!("Error handling search result timer expiry for peer {:}: {:?}", peer_id, err);
                let _removed_peer = self.peers.remove(&peer_id);
                return; // Early return.
            }
        };

        if let Some(peer_handler_proxy) = peer_handler_proxy_option {
            self.report_peer_handler(peer_handler_proxy);
        }
    }

    /// Report the PeerHandlerProxy og a newly connected peer to the FIDL client of th HFP protocol
    fn report_peer_handler(&mut self, peer_handler_proxy: fidl_hfp::PeerHandlerProxy) {
        // TODO(fxb/127364) Update HangingGet with peer. Make sure to set the new
        // PeerProxy on the peer.  Be careful of races between the new PeerProxy and any
        // old ones.
        //
        // For now, just keep these around to prevent test failures caused by closing
        // streams.
        self.peer_handler_proxies.push(peer_handler_proxy);
    }

    fn handle_hands_free_request_stream(&mut self, stream: fidl_hfp::HandsFreeRequestStream) {
        if self.hands_free_request_maybe_stream.is_some() {
            info!("Got new HandsFree request stream while one already exists. Closing the new stream.");
            let control_handle = stream.control_handle();
            control_handle.shutdown_with_epitaph(zx::Status::ALREADY_BOUND);
        } else {
            self.hands_free_request_maybe_stream.set(stream);
            // TODO(fxb/127364) Update HangingGet with all peers.  Make sure to set the new PeerProxy
            // on each peer.  Be careful of races between the new PeerProxy and any old ones
        }
    }

    fn handle_hands_free_request(&mut self, request: fidl_hfp::HandsFreeRequest) {
        let fidl_hfp::HandsFreeRequest::WatchPeerConnected { responder } = request;
        // TODO(fxb/127364) Update HangingGet with new subscriber.
        // Handle FIDL calls here.
        responder.drop_without_shutdown();
    }
}
