// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_bluetooth_bredr as bredr;
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_bluetooth::types::PeerId;
use futures::channel::mpsc;
use futures::StreamExt;
use profile_client::{ProfileClient, ProfileEvent};
use std::collections::hash_map::HashMap;

use crate::config::HandsFreeFeatureSupport;
use crate::peer::Peer;

pub struct Hfp {
    config: HandsFreeFeatureSupport,
    /// Provides Hfp with a means to drive the `fuchsia.bluetooth.bredr` related APIs.
    profile_client: ProfileClient,
    /// The client connection to the `fuchsia.bluetooth.bredr.Profile` protocol.
    profile_svc: bredr::ProfileProxy,
    /// A collection of discovered and/or connected Bluetooth peers that support the AG role.
    peers: HashMap<PeerId, Peer>,
}

impl Hfp {
    pub fn new(
        profile_client: ProfileClient,
        profile_svc: bredr::ProfileProxy,
        config: HandsFreeFeatureSupport,
        _hfp_stream_receiver: mpsc::Receiver<fidl_hfp::HandsFreeRequestStream>,
    ) -> Self {
        Self { profile_client, profile_svc, config, peers: HashMap::new() }
    }

    /// Run the Hfp object to completion. Runs until an unrecoverable error occurs or there is no
    /// more work to perform because all managed resources have been closed.
    pub async fn run(mut self) -> Result<(), Error> {
        while let Some(event_result) = self.profile_client.next().await {
            let event = event_result?;
            self.handle_profile_event(event).await?;
        }
        Ok(())
    }

    async fn handle_profile_event(&mut self, event: ProfileEvent) -> Result<(), Error> {
        let id = event.peer_id();
        let peer = self
            .peers
            .entry(id)
            .or_insert_with(|| Peer::new(id, self.config, self.profile_svc.clone()));
        peer.handle_profile_event(event).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_bluetooth_bredr::ProfileRequest::Advertise;

    use crate::profile::register;

    #[fuchsia::test]
    async fn run_loop_early_termination_of_connection_stream() {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<bredr::ProfileMarker>()
                .expect("Create new profile connection");
        let profile = register(proxy.clone(), HandsFreeFeatureSupport::default())
            .expect("ProfileClient Success.");
        let (_call_client_sender, call_client_receiver) = mpsc::channel(0);
        let hfp =
            Hfp::new(profile, proxy, HandsFreeFeatureSupport::default(), call_client_receiver);
        let request = stream.next().await.unwrap().expect("FIDL request is OK");
        let (_responder, receiver) = match request {
            Advertise { receiver, responder, .. } => (responder, receiver.into_proxy().unwrap()),
            _ => panic!("Not an advertisement"),
        };

        drop(receiver);
        let result = hfp.run().await;
        assert_matches!(result, Ok(_));
    }

    #[fuchsia::test]
    async fn run_loop_early_termination_of_advertisement() {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<bredr::ProfileMarker>()
                .expect("Create new profile connection");
        let profile = register(proxy.clone(), HandsFreeFeatureSupport::default())
            .expect("ProfileClient Success.");
        let (_call_client_sender, call_client_receiver) = mpsc::channel(0);
        let hfp =
            Hfp::new(profile, proxy, HandsFreeFeatureSupport::default(), call_client_receiver);
        let request = stream.next().await.unwrap().expect("FIDL request is OK");
        match request {
            Advertise { responder, .. } => {
                let _ = responder.send(Ok(())).unwrap();
            }
            _ => panic!("Not an advertisement"),
        };
        let result = hfp.run().await;
        assert_matches!(result, Err(_));
    }
}
