// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_bluetooth_map::{
    MessagingClientMarker, MessagingClientRequest, MessagingClientRequestStream,
};
use fuchsia_component::server::{ServiceFs, ServiceObj};
use futures::{channel::mpsc::Sender, SinkExt, StreamExt};
use tracing::{trace, warn};

use crate::messaging_client::WatchAccessorRequest;

/// The maximum number of FIDL service client connections that will be serviced concurrently.
const MAX_CONCURRENT_CONNECTIONS: usize = 10;

/// All FIDL services that are exposed by this component's ServiceFs.
pub enum Service {
    MessagingClient(MessagingClientRequestStream),
}

async fn run_messaging_client_server(
    mut stream: MessagingClientRequestStream,
    mut request_sender: Sender<WatchAccessorRequest>,
) {
    trace!("New {} client connection", MessagingClientMarker::PROTOCOL_NAME);
    while let Some(request) = stream.next().await {
        match request {
            Ok(MessagingClientRequest::WatchAccessor { responder }) => {
                if let Err(e) = request_sender.send(WatchAccessorRequest(responder)).await {
                    warn!("Could not relay Messaging Client watch accessor request to component: {:?}", e);
                };
            }
            Ok(unknown) => warn!("Unknown method received: {:?}", unknown),
            Err(e) => {
                warn!(
                    "Error in {} stream: {:?}. Closing connection",
                    MessagingClientMarker::PROTOCOL_NAME,
                    e
                );
                break;
            }
        }
    }
    trace!("{} client connection closed", MessagingClientMarker::PROTOCOL_NAME);
}

/// Run the FIDL service for Messaging Client.
pub async fn run_service(
    mut fs: ServiceFs<ServiceObj<'_, Service>>,
    request_sender: Sender<WatchAccessorRequest>,
) -> Result<(), Error> {
    let _ = fs.dir("svc").add_fidl_service(Service::MessagingClient);
    let _ = fs.take_and_serve_directory_handle().context("Failed to serve ServiceFs directory")?;

    trace!("Listening for incoming connections...");
    fs.for_each_concurrent(MAX_CONCURRENT_CONNECTIONS, |Service::MessagingClient(stream)| {
        run_messaging_client_server(stream, request_sender.clone())
    })
    .await;
    Ok(())
}
