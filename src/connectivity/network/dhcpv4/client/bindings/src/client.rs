// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{cell::RefCell, num::NonZeroU64};

use anyhow::Context as _;
use async_trait::async_trait;
use fidl::endpoints::{self, Proxy as _};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp::{
    self as fdhcp, ClientExitReason, ClientRequestStream, ClientWatchConfigurationResponse,
    ConfigurationToRequest, NewClientParams,
};
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{channel::mpsc, pin_mut, select, FutureExt as _, TryStreamExt as _};
use net_types::{
    ip::{Ipv4, PrefixLength},
    SpecifiedAddr, Witness as _,
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

impl Error {
    fn from_core(core_error: dhcp_client_core::client::Error) -> Self {
        match core_error {
            dhcp_client_core::client::Error::Socket(socket_error) => match socket_error {
                dhcp_client_core::deps::SocketError::NoInterface
                | dhcp_client_core::deps::SocketError::UnsupportedHardwareType => {
                    Self::Exit(ClientExitReason::InvalidInterface)
                }
                dhcp_client_core::deps::SocketError::FailedToOpen(e) => {
                    tracing::error!("error while trying to open socket: {:?}", e);
                    Self::Exit(ClientExitReason::UnableToOpenSocket)
                }
                dhcp_client_core::deps::SocketError::HostUnreachable
                | dhcp_client_core::deps::SocketError::Other(_) => {
                    Self::Core(dhcp_client_core::client::Error::Socket(socket_error))
                }
                dhcp_client_core::deps::SocketError::NetworkUnreachable => {
                    Self::Exit(ClientExitReason::NetworkUnreachable)
                }
            },
        }
    }
}

pub(crate) async fn serve_client(
    mac: net_types::ethernet::Mac,
    interface_id: NonZeroU64,
    provider: &crate::packetsocket::PacketSocketProviderImpl,
    udp_socket_provider: &crate::udpsocket::UdpSocketProviderImpl,
    params: NewClientParams,
    requests: ClientRequestStream,
) -> Result<(), Error> {
    let (stop_sender, stop_receiver) = mpsc::unbounded();
    let stop_sender = &stop_sender;
    let client = RefCell::new(Client::new(
        mac,
        interface_id,
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
                            .send(client.watch_configuration(provider, udp_socket_provider).await?)
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
    interface_id: NonZeroU64,
}

struct Lease {
    address_state_provider: fnet_interfaces_admin::AddressStateProviderProxy,
    event_stream: fnet_interfaces_admin::AddressStateProviderEventStream,
    ip_address: SpecifiedAddr<net_types::ip::Ipv4Addr>,
}

impl Lease {
    async fn watch_for_address_removal(
        &mut self,
    ) -> Result<fnet_interfaces_admin::AddressRemovalReason, anyhow::Error> {
        let Self { address_state_provider, event_stream, ip_address: _ } = self;

        let _: zx::Signals = address_state_provider
            .on_closed()
            .await
            .context("unexpected zx::Status while awaiting AddressStateProvider channel close")?;

        let fnet_interfaces_admin::AddressStateProviderEvent::OnAddressRemoved { error: reason } =
            event_stream
                .try_next()
                .await
                .context("AddressStateProviderEventStream FIDL error")?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "AddressStateProvider event stream ended without yielding removal reason"
                    )
                })?;

        Ok(reason)
    }
}

impl Client {
    fn new(
        mac: net_types::ethernet::Mac,
        interface_id: NonZeroU64,
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
            interface_id,
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
        let Self { core: _, rng: _, config: _, stop_receiver: _, current_lease, interface_id: _ } =
            self;

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
            tracing::warn!(
                "Received options from core that we didn't ask for: {:#?}",
                unrequested_options
            );
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

        let previous_lease = current_lease.replace(Lease {
            event_stream: asp_proxy.take_event_stream(),
            address_state_provider: asp_proxy,
            ip_address,
        });

        if let Some(Lease { event_stream: _, address_state_provider: _, ip_address }) =
            previous_lease
        {
            tracing::warn!("dropping previous lease for address {}", ip_address);
        }

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
            dns_servers: dns_servers.map(into_fidl_list),
            routers: routers.map(into_fidl_list),
            ..Default::default()
        })
    }

    async fn handle_lease_renewal(
        &mut self,
        dhcp_client_core::client::LeaseRenewal {
            start_time,
            lease_time,
            parameters,
        }: dhcp_client_core::client::LeaseRenewal<fasync::Time>,
    ) -> Result<ClientWatchConfigurationResponse, Error> {
        let Self { core: _, rng: _, config: _, stop_receiver: _, current_lease, interface_id: _ } =
            self;

        let mut dns_servers: Option<Vec<_>> = None;
        let mut routers: Option<Vec<_>> = None;
        let mut unrequested_options = Vec::new();

        for option in parameters {
            match option {
                dhcp_protocol::DhcpOption::SubnetMask(len) => {
                    tracing::info!("ignoring prefix length={:?} for renewed lease", len);
                }
                dhcp_protocol::DhcpOption::DomainNameServer(list) => {
                    assert_eq!(dns_servers.replace(list.into()), None);
                }
                dhcp_protocol::DhcpOption::Router(list) => {
                    assert_eq!(routers.replace(list.into()), None);
                }
                option => {
                    unrequested_options.push(option);
                }
            }
        }

        if !unrequested_options.is_empty() {
            tracing::warn!(
                "Received options from core that we didn't ask for: {:#?}",
                unrequested_options
            );
        }

        let Lease { event_stream: _, address_state_provider, ip_address: _ } =
            current_lease.as_mut().expect("should have current lease if we're handling a renewal");

        address_state_provider
            .update_address_properties(&fnet_interfaces_admin::AddressProperties {
                preferred_lifetime_info: None,
                valid_lifetime_end: Some(
                    zx::Time::from(start_time + lease_time.into()).into_nanos(),
                ),
                ..Default::default()
            })
            .await
            .map_err(Error::Fidl)?;

        Ok(ClientWatchConfigurationResponse {
            address: None,
            dns_servers: dns_servers.map(into_fidl_list),
            routers: routers.map(into_fidl_list),
            ..Default::default()
        })
    }

    async fn watch_configuration(
        &mut self,
        packet_socket_provider: &crate::packetsocket::PacketSocketProviderImpl,
        udp_socket_provider: &crate::udpsocket::UdpSocketProviderImpl,
    ) -> Result<ClientWatchConfigurationResponse, Error> {
        let clock = Clock;
        loop {
            let Self { core, rng, config, stop_receiver, current_lease, interface_id } = self;

            // Notice that we are `select`ing between the following two futures,
            // and when one of them completes we throw away progress on the
            // other one:
            // 1) Watching for a previously-acquired address to be removed
            // 2) Running the current state machine state
            //
            // If (1) completes, throwing away progress on (2) is okay because
            // we always need to either transition back to the Init state or
            // exit the client when an address is removed -- in both cases we're
            // fine with throwing away whatever state-execution progress we'd
            // previously made.
            //
            // If (2) completes, throwing away progress on (1) is okay because
            // we'll be in one of two scenarios:
            // a) We've transitioned to another state that maintains the same
            //    lease (e.g. going from Bound to Renewing for the same
            //    address). In this case, we won't modify the `current_lease`
            //    field, and we'll observe that the AddressStateProvider has
            //    been closed on the next iteration of this loop.
            // b) We've transitioned to a state that entails removing the lease
            //    (e.g. failing to Rebind and having to go back to Init), at
            //    which point `current_lease` will be cleared for the next
            //    iteration.
            let select_result = {
                let core_step_fut = core
                    .run(
                        config,
                        packet_socket_provider,
                        udp_socket_provider,
                        rng,
                        &clock,
                        stop_receiver,
                    )
                    .fuse();
                let address_removed_fut = async {
                    match current_lease {
                        Some(current_lease) => {
                            match current_lease.watch_for_address_removal().await {
                                Ok(reason) => (Some(reason), current_lease.ip_address),
                                Err(e) => {
                                    tracing::error!(
                                        "observed error {:?} while watching for removal \
                                         of address {} on interface {}; \
                                         removing address",
                                        e,
                                        *current_lease.ip_address,
                                        interface_id
                                    );
                                    (None, current_lease.ip_address)
                                }
                            }
                        }
                        None => futures::future::pending().await,
                    }
                }
                .fuse();

                pin_mut!(core_step_fut, address_removed_fut);

                select! {
                    reason = address_removed_fut => futures::future::Either::Left(reason),
                    core_step = core_step_fut => futures::future::Either::Right(core_step),
                }
            };

            match select_result {
                futures::future::Either::Left((reason, ip_address)) => {
                    // TODO(https://fxbug.dev/126747): Explicitly remove this
                    // address and observe the OnAddressRemoved event to ensure
                    // we don't race with netstack-side teardown if we end up
                    // re-adding the same address later.
                    *current_lease = None;
                    match reason {
                        None => {
                            return Err(Error::Exit(ClientExitReason::AddressStateProviderError))
                        }
                        Some(reason) => match reason {
                            fnet_interfaces_admin::AddressRemovalReason::Invalid => {
                                panic!("yielded invalid address")
                            }
                            fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved => {
                                tracing::warn!("interface removed; stopping");
                                return Err(Error::Exit(ClientExitReason::InvalidInterface));
                            }
                            fnet_interfaces_admin::AddressRemovalReason::UserRemoved => {
                                tracing::warn!("address administratively removed; stopping");
                                return Err(Error::Exit(ClientExitReason::AddressRemovedByUser));
                            }
                            fnet_interfaces_admin::AddressRemovalReason::AlreadyAssigned => {
                                tracing::warn!("address already assigned; notifying core");
                            }
                            fnet_interfaces_admin::AddressRemovalReason::DadFailed => {
                                tracing::warn!("duplicate address detected; notifying core");
                            }
                        },
                    };

                    match core
                        .on_address_rejection(config, packet_socket_provider, &clock, ip_address)
                        .await
                        .map_err(Error::from_core)?
                    {
                        dhcp_client_core::client::AddressRejectionOutcome::ShouldBeImpossible => {
                            unreachable!(
                                "should not observe address rejection without active lease"
                            );
                        }
                        dhcp_client_core::client::AddressRejectionOutcome::NextState(state) => {
                            *core = state;
                        }
                    }
                }
                futures::future::Either::Right(core_step) => {
                    match core_step {
                        Err(e) => return Err(Error::from_core(e)),
                        Ok(step) => match step {
                            dhcp_client_core::client::Step::NextState(transition) => {
                                let (next_core, effect) = core.apply(transition);
                                *core = next_core;
                                match effect {
                                Some(dhcp_client_core::client::TransitionEffect::DropLease) => {
                                    // TODO(https://fxbug.dev/126747):
                                    // Explicitly remove this address and
                                    // observe the OnAddressRemoved event to
                                    // ensure we don't race with netstack-side
                                    // teardown if we end up re-adding the same
                                    // address later.
                                    assert!(current_lease.take().is_some());
                                }
                                Some(dhcp_client_core::client::TransitionEffect::HandleNewLease(
                                    newly_acquired_lease,
                                )) => {
                                    return self.handle_newly_acquired_lease(newly_acquired_lease);
                                }
                                Some(dhcp_client_core::client::TransitionEffect::HandleRenewedLease(
                                    lease_renewal
                                )) => {
                                    return self.handle_lease_renewal(lease_renewal).await;
                                }
                                None => (),
                                }
                            }
                            dhcp_client_core::client::Step::Exit(reason) => match reason {
                                dhcp_client_core::client::ExitReason::GracefulShutdown => {
                                    return Err(Error::Exit(ClientExitReason::GracefulShutdown))
                                }
                            },
                        },
                    }
                }
            };
        }
    }
}

fn into_fidl_list(list: Vec<std::net::Ipv4Addr>) -> Vec<fidl_fuchsia_net::Ipv4Address> {
    list.into_iter().map(|addr| net_types::ip::Ipv4Addr::from(addr).into_ext()).collect()
}
