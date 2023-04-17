// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fuchsia_zircon as zx;

use anyhow::Context as _;
use futures::stream::TryStreamExt as _;
use tracing::{info, warn};

use crate::errors;

#[derive(Debug)]
pub(super) struct ClientState {
    client: fnet_dhcp::ClientProxy,
}

pub(super) const NEW_CLIENT_PARAMS: fnet_dhcp::NewClientParams = fnet_dhcp::NewClientParams {
    configuration_to_request: Some(fnet_dhcp::ConfigurationToRequest {
        routers: Some(true),
        dns_servers: Some(true),
        ..fnet_dhcp::ConfigurationToRequest::EMPTY
    }),
    request_ip_address: Some(true),
    ..fnet_dhcp::NewClientParams::EMPTY
};

pub(super) fn start_client(
    interface_id: u64,
    interface_name: &str,
    client_provider: &fnet_dhcp::ClientProviderProxy,
) -> ClientState {
    info!("starting DHCPv4 client for {} (id={})", interface_name, interface_id);

    let (client, server) = fidl::endpoints::create_proxy::<fnet_dhcp::ClientMarker>()
        .expect("create DHCPv4 client fidl endpoints");

    client_provider
        .new_client(interface_id, NEW_CLIENT_PARAMS, server)
        .expect("create new DHCPv4 client");

    ClientState { client }
}

pub(super) async fn stop_client(
    interface_id: u64,
    interface_name: &str,
    ClientState { client }: ClientState,
) {
    info!("stopping DHCPv4 client for {} (id={})", interface_name, interface_id);

    client.shutdown().unwrap_or_else(|e| {
        warn!(
            "error shutting down DHCPv4 client for {} (id={}): {:?}",
            interface_name, interface_id, e
        )
    });

    let stream = client.take_event_stream().try_filter_map(|event| async move {
        match event {
            fnet_dhcp::ClientEvent::OnExit { reason } => Ok(match reason {
                fnet_dhcp::ClientExitReason::ClientAlreadyExistsOnInterface
                | fnet_dhcp::ClientExitReason::WatchConfigurationAlreadyPending
                | fnet_dhcp::ClientExitReason::InvalidInterface
                | fnet_dhcp::ClientExitReason::InvalidParams
                | fnet_dhcp::ClientExitReason::NetworkUnreachable
                | fnet_dhcp::ClientExitReason::UnableToOpenSocket => {
                    warn!(
                        "unexpected exit reason when shutting down DHCPv4 client \
                                 for {} (id={}); got = {:?}",
                        interface_name, interface_id, reason,
                    );
                    None
                }
                fnet_dhcp::ClientExitReason::GracefulShutdown => Some(()),
            }),
        }
    });
    futures::pin_mut!(stream);
    stream.try_next().await.expect("wait for client to gracefully exit").unwrap_or_else(|| {
        warn!(
            "DHCPv4 client for {} (id={}) exited without graceful shutdown event",
            interface_name, interface_id,
        )
    })
}

/// Start the DHCP server.
pub(super) async fn start_server(
    dhcp_server: &fnet_dhcp::Server_Proxy,
) -> Result<(), errors::Error> {
    dhcp_server
        .start_serving()
        .await
        .context("error sending start DHCP server request")
        .map_err(errors::Error::NonFatal)?
        .map_err(zx::Status::from_raw)
        .context("error starting DHCP server")
        .map_err(errors::Error::NonFatal)
}

/// Stop the DHCP server.
pub(super) async fn stop_server(
    dhcp_server: &fnet_dhcp::Server_Proxy,
) -> Result<(), errors::Error> {
    dhcp_server
        .stop_serving()
        .await
        .context("error sending stop DHCP server request")
        .map_err(errors::Error::NonFatal)
}
