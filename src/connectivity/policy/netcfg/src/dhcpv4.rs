// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{collections::HashSet, num::NonZeroU64, pin::Pin};

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_dhcp_ext::{self as fnet_dhcp_ext, ClientExt as _, ClientProviderExt as _};
use fidl_fuchsia_net_ext::FromExt as _;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_name as fnet_name;
use fidl_fuchsia_net_stack as fnet_stack;
use fuchsia_zircon as zx;

use anyhow::Context as _;
use async_utils::stream::{StreamMap, Tagged, WithTag as _};
use dns_server_watcher::{DnsServers, DnsServersUpdateSource, DEFAULT_DNS_PORT};
use futures::stream::StreamExt as _;
use net_types::{ip::Ipv4Addr, SpecifiedAddr};
use tracing::{info, warn};

use crate::{dns, errors};

#[derive(Debug)]
pub(super) struct ClientState {
    client: fnet_dhcp::ClientProxy,
    routers: HashSet<SpecifiedAddr<Ipv4Addr>>,
}

pub(super) fn new_client_params() -> fnet_dhcp::NewClientParams {
    fnet_dhcp_ext::default_new_client_params()
}

pub(super) type ConfigurationStreamMap =
    StreamMap<NonZeroU64, InterfaceIdTaggedConfigurationStream>;
pub(super) type InterfaceIdTaggedConfigurationStream = Tagged<NonZeroU64, ConfigurationStream>;
pub(super) type ConfigurationStream =
    futures::stream::BoxStream<'static, Result<fnet_dhcp_ext::Configuration, fnet_dhcp_ext::Error>>;

pub(super) async fn probe_for_presence(provider: &fnet_dhcp::ClientProviderProxy) -> bool {
    match provider.check_presence().await {
        Ok(()) => true,
        Err(fidl::Error::ClientChannelClosed { status: _, protocol_name: _ }) => false,
        Err(e) => panic!("unexpected error while probing: {e}"),
    }
}

pub(super) async fn update_configuration(
    interface_id: NonZeroU64,
    ClientState { client: _, routers: configured_routers }: &mut ClientState,
    configuration: fnet_dhcp_ext::Configuration,
    dns_servers: &mut DnsServers,
    control: &fnet_interfaces_ext::admin::Control,
    stack: &fnet_stack::StackProxy,
    lookup_admin: &fnet_name::LookupAdminProxy,
) {
    let fnet_dhcp_ext::Configuration {
        address,
        dns_servers: new_dns_servers,
        routers: new_routers,
        ..
    } = configuration;
    if let Some(address) = address {
        match address.add_to(control) {
            Ok(()) => {}
            Err((address, e)) => {
                let fnet::Ipv4AddressWithPrefix { addr, prefix_len } = address;
                warn!(
                    "error adding DHCPv4-acquired address {}/{} for interface {}: {}",
                    // Make sure address is pretty-printed so that PII filtering
                    // is properly applied on addresses.
                    Ipv4Addr::from_ext(addr),
                    prefix_len,
                    interface_id,
                    e,
                )
            }
        }
    }

    dns::update_servers(
        lookup_admin,
        dns_servers,
        DnsServersUpdateSource::Dhcpv4 { interface_id: interface_id.get() },
        new_dns_servers
            .iter()
            .map(|&address| fnet_name::DnsServer_ {
                address: Some(fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
                    address,
                    port: DEFAULT_DNS_PORT,
                })),
                source: Some(fnet_name::DnsServerSource::Dhcp(fnet_name::DhcpDnsServerSource {
                    source_interface: Some(interface_id.get()),
                    ..fnet_name::DhcpDnsServerSource::default()
                })),
                ..fnet_name::DnsServer_::default()
            })
            .collect(),
    )
    .await;

    fnet_dhcp_ext::apply_new_routers(interface_id, stack, configured_routers, new_routers)
        .await
        .unwrap_or_else(|e| {
            warn!("error applying new DHCPv4-acquired router configuration: {:?}", e);
        })
}

pub(super) fn start_client(
    interface_id: NonZeroU64,
    interface_name: &str,
    client_provider: &fnet_dhcp::ClientProviderProxy,
    configuration_streams: &mut ConfigurationStreamMap,
) -> ClientState {
    info!("starting DHCPv4 client for {} (id={})", interface_name, interface_id);

    let client = client_provider.new_client_ext(interface_id, new_client_params());

    if let Some(stream) = configuration_streams.insert(
        interface_id,
        fnet_dhcp_ext::configuration_stream(client.clone()).boxed().tagged(interface_id),
    ) {
        let _: Pin<Box<InterfaceIdTaggedConfigurationStream>> = stream;
        unreachable!("only one DHCPv4 client may exist on {} (id={})", interface_name, interface_id)
    }

    ClientState { client, routers: Default::default() }
}

pub(super) async fn stop_client(
    interface_id: NonZeroU64,
    interface_name: &str,
    mut state: ClientState,
    configuration_streams: &mut ConfigurationStreamMap,
    dns_servers: &mut DnsServers,
    control: &fnet_interfaces_ext::admin::Control,
    stack: &fnet_stack::StackProxy,
    lookup_admin: &fnet_name::LookupAdminProxy,
) {
    info!("stopping DHCPv4 client for {} (id={})", interface_name, interface_id);

    update_configuration(
        interface_id,
        &mut state,
        fnet_dhcp_ext::Configuration::default(),
        dns_servers,
        control,
        stack,
        lookup_admin,
    )
    .await;

    let ClientState { client, routers: _ } = state;

    let _: Pin<Box<InterfaceIdTaggedConfigurationStream>> =
        configuration_streams.remove(&interface_id).unwrap_or_else(|| {
            unreachable!(
                "DHCPv4 client must exist when stopping on {} (id={})",
                interface_name, interface_id,
            )
        });

    client.shutdown_ext(client.take_event_stream()).await.unwrap_or_else(|e| {
        warn!(
            "error shutting down DHCPv4 client for {} (id={}): {:?}",
            interface_name, interface_id, e,
        )
    });
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
