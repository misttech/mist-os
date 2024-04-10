// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "1024"]

use anyhow::{format_err, Context, Error};
use fidl_fuchsia_bluetooth_bredr as bredr;
use fuchsia_bluetooth::profile::psm_from_protocol;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use futures::{channel::mpsc, FutureExt};
use profile_client::ProfileEvent;
use std::pin::pin;
use tracing::{error, info, trace, warn};

mod fidl_service;
mod messaging_client;
mod profile;

use fidl_service::run_service;
use messaging_client::MessagingClient;
use profile::MasConfig;

#[fuchsia::main(logging_tags = ["bt-map-mce"])]
async fn main() -> Result<(), Error> {
    // Connect to Profile service.
    let profile_svc = fuchsia_component::client::connect_to_protocol::<bredr::ProfileMarker>()
        .context("Failed to connect to Bluetooth Profile service")?;
    let mut profile_client = profile::connect_and_advertise(profile_svc)
        .context("Unable to connect to BrEdr Profile Service")?;

    // Run the Message Client server.
    let (fidl_request_sender, mut fidl_request_receiver) = mpsc::channel(1);
    let messaging_client = MessagingClient::new();

    // Run the fidl service to accept incoming fidl requests.
    let fs = ServiceFs::new();
    let mut service_fut = pin!(run_service(fs, fidl_request_sender).fuse());

    // Process requests.
    loop {
        futures::select! {
            request = profile_client.next() => {
                let request = match request {
                    None => return Err(format_err!("BR/EDR Profile unexpectedly closed")),
                    Some(Err(e)) => return Err(format_err!("Profile client error: {e:?}")),
                    Some(Ok(r)) => r,
                };
                match request {
                    ProfileEvent::PeerConnected { id, protocol, .. } => {
                        let protocol = protocol.iter().map(Into::into).collect();
                        let psm = match psm_from_protocol(&protocol) {
                            None => {
                                warn!(peer_id = %id, "Received peer connect request with no PSM");
                                continue;
                            }
                            Some(psm) => psm,
                        };
                        trace!(peer_id = %id, "Incoming connection request with protocol: {protocol:?} and PSM {psm:?}");
                    },
                    ProfileEvent::SearchResult {id, protocol, attributes } => {
                        let Some(protocol) = protocol else {
                            info!(peer_id = %id, "Received search result with no protocol, ignoring..");
                            continue;
                        };

                        let Ok(mas_config) = MasConfig::from_search_result(protocol, attributes) else {
                            warn!(peer_id = %id, "Received invalid mas config");
                            continue;
                        };
                        info!(peer_id = %id, "Found MSE peer wth config: {mas_config:?}");
                    },
                }
            }
            request = fidl_request_receiver.select_next_some() => {
                messaging_client.queue_watch_accessor(request);
            }
            service_result = service_fut => {
                error!("Service task finished unexpectedly: {service_result:?}");
                break;
            },
            complete => break,
        }
    }
    Ok(())
}
