// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;

use assert_matches::assert_matches;
use async_trait::async_trait;
use fidl::endpoints;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp::{
    self as fdhcp, ClientExitReason, ClientRequestStream, ClientWatchConfigurationResponse,
    ConfigurationToRequest, NewClientParams,
};
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{channel::mpsc, TryStreamExt as _};
use net_types::{
    ip::{Ipv4, PrefixLength},
    Witness as _,
};
use rand::SeedableRng as _;

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("DHCP client exiting: {0:?}")]
    Exit(ClientExitReason),

    #[error("error observed by DHCP client core: {0:?}")]
    Core(dhcp_client_core::client::Error),

    #[error("fidl error: {0}")]
    Fidl(fidl::Error),
}

pub(crate) async fn serve_client(
    mac: net_types::ethernet::Mac,
    provider: &crate::packetsocket::PacketSocketProviderImpl,
    params: NewClientParams,
    requests: ClientRequestStream,
) -> Result<(), Error> {
    let (stop_sender, stop_receiver) = mpsc::unbounded();
    let stop_sender = &stop_sender;
    let client = RefCell::new(Client::new(
        mac,
        params,
        rand::rngs::StdRng::seed_from_u64(rand::random()),
        stop_receiver,
    )?);
    requests
        .map_err(Error::Fidl)
        .try_for_each_concurrent(None, |request| {
            let client = &client;
            async move {
                match request {
                    fidl_fuchsia_net_dhcp::ClientRequest::WatchConfiguration { responder } => {
                        let mut client = client.try_borrow_mut().map_err(|_| {
                            Error::Exit(ClientExitReason::WatchConfigurationAlreadyPending)
                        })?;
                        responder
                            .send(client.watch_configuration(provider).await?)
                            .map_err(Error::Fidl)?;
                        Ok(())
                    }
                    fidl_fuchsia_net_dhcp::ClientRequest::Shutdown { control_handle: _ } => {
                        match stop_sender.unbounded_send(()) {
                            Ok(()) => stop_sender.close_channel(),
                            Err(try_send_error) => {
                                // Note that `try_send_error` cannot be exhaustively matched on.
                                if try_send_error.is_disconnected() {
                                    tracing::warn!(
                                        "tried to send shutdown request on \
                                        already-closed channel to client core"
                                    );
                                } else {
                                    tracing::error!(
                                        "error while sending shutdown request \
                                        to client core: {:?}",
                                        try_send_error
                                    );
                                }
                            }
                        }
                        Ok(())
                    }
                }
            }
        })
        .await
}

struct Clock;

#[async_trait(?Send)]
impl dhcp_client_core::deps::Clock for Clock {
    type Instant = fasync::Time;

    fn now(&self) -> Self::Instant {
        fasync::Time::now()
    }

    async fn wait_until(&self, time: Self::Instant) {
        fasync::Timer::new(time).await
    }
}

/// Encapsulates all DHCP client state.
struct Client {
    config: dhcp_client_core::client::ClientConfig,
    core: dhcp_client_core::client::State<fasync::Time>,
    rng: rand::rngs::StdRng,
    stop_receiver: mpsc::UnboundedReceiver<()>,
    current_lease: Option<Lease>,
}

#[derive(Debug, Clone)]
struct Lease {
    // TODO(https://fxbug.dev/126084): Handle duplicate address detection and
    // address-already-exists errors.
    _address_state_provider: fnet_interfaces_admin::AddressStateProviderProxy,
}

impl Client {
    fn new(
        mac: net_types::ethernet::Mac,
        NewClientParams { configuration_to_request, request_ip_address, .. }: NewClientParams,
        rng: rand::rngs::StdRng,
        stop_receiver: mpsc::UnboundedReceiver<()>,
    ) -> Result<Self, Error> {
        if !request_ip_address.unwrap_or(false) {
            tracing::error!("client creation failed: DHCPINFORM is unimplemented");
            return Err(Error::Exit(ClientExitReason::InvalidParams));
        }
        let ConfigurationToRequest { routers, dns_servers, .. } =
            configuration_to_request.unwrap_or(ConfigurationToRequest::default());

        let config = dhcp_client_core::client::ClientConfig {
            client_hardware_address: mac,
            client_identifier: None,
            requested_parameters: std::iter::once((
                dhcp_protocol::OptionCode::SubnetMask,
                dhcp_client_core::parse::OptionRequested::Required,
            ))
            .chain(routers.unwrap_or(false).then_some((
                dhcp_protocol::OptionCode::Router,
                dhcp_client_core::parse::OptionRequested::Optional,
            )))
            .chain(dns_servers.unwrap_or(false).then_some((
                dhcp_protocol::OptionCode::DomainNameServer,
                dhcp_client_core::parse::OptionRequested::Optional,
            )))
            .collect::<dhcp_client_core::parse::OptionCodeMap<_>>(),
            preferred_lease_time_secs: None,
            requested_ip_address: None,
        };
        Ok(Self {
            core: dhcp_client_core::client::State::default(),
            rng,
            config,
            stop_receiver,
            current_lease: None,
        })
    }

    fn handle_newly_acquired_lease(
        &mut self,
        dhcp_client_core::client::NewlyAcquiredLease {
            ip_address,
            start_time,
            lease_time,
            parameters,
        }: dhcp_client_core::client::NewlyAcquiredLease<fasync::Time>,
    ) -> Result<ClientWatchConfigurationResponse, Error> {
        let Self { core: _, rng: _, config: _, stop_receiver: _, current_lease } = self;

        let mut dns_servers: Option<Vec<_>> = None;
        let mut routers: Option<Vec<_>> = None;
        let mut prefix_len: Option<PrefixLength<Ipv4>> = None;
        let mut unrequested_options = Vec::new();

        for option in parameters {
            match option {
                dhcp_protocol::DhcpOption::SubnetMask(len) => {
                    assert_eq!(prefix_len.replace(len), None);
                }
                dhcp_protocol::DhcpOption::DomainNameServer(list) => {
                    assert_eq!(dns_servers.replace(list.into()), None);
                }
                dhcp_protocol::DhcpOption::Router(list) => {
                    assert_eq!(routers.replace(list.into()), None);
                }
                _ => {
                    unrequested_options.push(option);
                }
            }
        }

        if !unrequested_options.is_empty() {
            panic!("Received options from core that we didn't ask for: {unrequested_options:#?}");
        }

        let prefix_len = prefix_len
            .expect(
                "subnet mask should be present \
                because it was specified to core as required",
            )
            .get();

        let (asp_proxy, asp_server_end) = endpoints::create_proxy::<
            fnet_interfaces_admin::AddressStateProviderMarker,
        >()
        .expect("should never get FIDL error while creating AddressStateProvider endpoints");

        assert_matches!(
            current_lease.replace(Lease { _address_state_provider: asp_proxy }),
            None,
            "should not currently have a lease"
        );

        Ok(ClientWatchConfigurationResponse {
            address: Some(fdhcp::Address {
                address: Some(fnet::Ipv4AddressWithPrefix {
                    addr: ip_address.get().into_ext(),
                    prefix_len,
                }),
                address_parameters: Some(fnet_interfaces_admin::AddressParameters {
                    initial_properties: Some(fnet_interfaces_admin::AddressProperties {
                        preferred_lifetime_info: None,
                        valid_lifetime_end: Some(
                            zx::Time::from(start_time + lease_time.into()).into_nanos(),
                        ),
                        ..Default::default()
                    }),
                    add_subnet_route: Some(true),
                    ..Default::default()
                }),
                address_state_provider: Some(asp_server_end),
                ..Default::default()
            }),
            dns_servers: dns_servers.map(|list| {
                list.into_iter()
                    .map(|addr| net_types::ip::Ipv4Addr::from(addr).into_ext())
                    .collect()
            }),
            routers: routers.map(|list| {
                list.into_iter()
                    .map(|addr| net_types::ip::Ipv4Addr::from(addr).into_ext())
                    .collect()
            }),
            ..Default::default()
        })
    }

    async fn watch_configuration(
        &mut self,
        packet_socket_provider: &crate::packetsocket::PacketSocketProviderImpl,
    ) -> Result<ClientWatchConfigurationResponse, Error> {
        let clock = Clock;
        loop {
            let Self { core, rng, config, stop_receiver, current_lease } = self;
            match core.run(config, packet_socket_provider, rng, &clock, stop_receiver).await {
                Err(e) => return Err(Error::Core(e)),
                Ok(step) => match step {
                    dhcp_client_core::client::Step::NextState(transition) => {
                        let (next_core, effect) = core.apply(transition);
                        *core = next_core;
                        match effect {
                            dhcp_client_core::client::TransitionEffect::DropLease => {
                                // TODO(https://fxbug.dev/126747): Explicitly remove this address
                                // and observe the OnAddressRemoved event to ensure we don't race
                                // with netstack-side teardown if we end up re-adding the same
                                // address later.
                                assert!(current_lease.take().is_some());
                            }
                            dhcp_client_core::client::TransitionEffect::HandleNewLease(
                                newly_acquired_lease,
                            ) => {
                                return self.handle_newly_acquired_lease(newly_acquired_lease);
                            }
                            dhcp_client_core::client::TransitionEffect::None => (),
                        }
                    }
                    dhcp_client_core::client::Step::Exit(reason) => match reason {
                        dhcp_client_core::client::ExitReason::GracefulShutdown => {
                            return Err(Error::Exit(ClientExitReason::GracefulShutdown))
                        }
                    },
                },
            };
        }
    }
}
