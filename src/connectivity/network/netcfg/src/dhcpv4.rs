// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::pin::Pin;

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_name as fnet_name;
use fuchsia_zircon as zx;

use anyhow::Context as _;
use async_utils::{
    hanging_get::client::HangingGetStream,
    stream::{StreamMap, Tagged, WithTag as _},
};
use dns_server_watcher::{DnsServers, DnsServersUpdateSource, DEFAULT_DNS_PORT};
use futures::stream::TryStreamExt as _;
use tracing::{info, warn};

use crate::{dns, errors};

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

pub(super) type ConfigurationStreamMap = StreamMap<u64, InterfaceIdTaggedConfigurationStream>;
pub(super) type InterfaceIdTaggedConfigurationStream = Tagged<u64, ConfigurationStream>;
pub(super) type ConfigurationStream =
    HangingGetStream<fnet_dhcp::ClientProxy, fnet_dhcp::ClientWatchConfigurationResponse>;

pub(super) async fn update_configuration(
    interface_id: u64,
    ClientState { client: _ }: &mut ClientState,
    fnet_dhcp::ClientWatchConfigurationResponse {
        address: _,
        dns_servers: new_dns_servers,
        routers: _,
        ..
    }: fnet_dhcp::ClientWatchConfigurationResponse,
    dns_servers: &mut DnsServers,
    lookup_admin: &fnet_name::LookupAdminProxy,
) {
    dns::update_servers(
        lookup_admin,
        dns_servers,
        DnsServersUpdateSource::Dhcpv4 { interface_id },
        new_dns_servers
            .map(|servers| {
                servers.into_iter().map(|address| fnet_name::DnsServer_ {
                    address: Some(fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
                        address,
                        port: DEFAULT_DNS_PORT,
                    })),
                    source: Some(fnet_name::DnsServerSource::Dhcp(
                        fnet_name::DhcpDnsServerSource {
                            source_interface: Some(interface_id),
                            ..fnet_name::DhcpDnsServerSource::EMPTY
                        },
                    )),
                    ..fnet_name::DnsServer_::EMPTY
                })
            })
            .into_iter()
            .flatten()
            .collect(),
    )
    .await
}

pub(super) fn start_client(
    interface_id: u64,
    interface_name: &str,
    client_provider: &fnet_dhcp::ClientProviderProxy,
    configuration_streams: &mut ConfigurationStreamMap,
) -> ClientState {
    info!("starting DHCPv4 client for {} (id={})", interface_name, interface_id);

    let (client, server) = fidl::endpoints::create_proxy::<fnet_dhcp::ClientMarker>()
        .expect("create DHCPv4 client fidl endpoints");

    client_provider
        .new_client(interface_id, NEW_CLIENT_PARAMS, server)
        .expect("create new DHCPv4 client");

    if let Some(stream) = configuration_streams.insert(
        interface_id,
        ConfigurationStream::new_eager_with_fn_ptr(
            client.clone(),
            fnet_dhcp::ClientProxy::watch_configuration,
        )
        .tagged(interface_id),
    ) {
        let _: Pin<Box<InterfaceIdTaggedConfigurationStream>> = stream;
        unreachable!("only one DHCPv4 client may exist on {} (id={})", interface_name, interface_id)
    }

    ClientState { client }
}

pub(super) async fn stop_client(
    interface_id: u64,
    interface_name: &str,
    mut state: ClientState,
    configuration_streams: &mut ConfigurationStreamMap,
    dns_servers: &mut DnsServers,
    lookup_admin: &fnet_name::LookupAdminProxy,
) {
    info!("stopping DHCPv4 client for {} (id={})", interface_name, interface_id);

    update_configuration(
        interface_id,
        &mut state,
        fnet_dhcp::ClientWatchConfigurationResponse::EMPTY,
        dns_servers,
        lookup_admin,
    )
    .await;

    let ClientState { client } = state;

    let _: Pin<Box<InterfaceIdTaggedConfigurationStream>> =
        configuration_streams.remove(&interface_id).unwrap_or_else(|| {
            unreachable!(
                "DHCPv4 client must exist when stopping on {} (id={})",
                interface_name, interface_id,
            )
        });

    client.shutdown().unwrap_or_else(|e| {
        warn!(
            "error shutting down DHCPv4 client for {} (id={}): {:?}",
            interface_name, interface_id, e,
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
