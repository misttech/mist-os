// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl_fuchsia_net_dhcp::{
    ClientExitReason, ClientRequestStream, ClientWatchConfigurationResponse,
    ConfigurationToRequest, NewClientParams,
};
use futures::{channel::mpsc, TryStreamExt as _};
use rand::SeedableRng as _;
use std::cell::RefCell;

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
    type Instant = fuchsia_async::Time;

    fn now(&self) -> Self::Instant {
        fuchsia_async::Time::now()
    }

    async fn wait_until(&self, time: Self::Instant) {
        fuchsia_async::Timer::new(time).await
    }
}

/// Encapsulates all DHCP client state.
struct Client {
    config: dhcp_client_core::client::ClientConfig,
    core: dhcp_client_core::client::State<fuchsia_async::Time>,
    rng: rand::rngs::StdRng,
    stop_receiver: mpsc::UnboundedReceiver<()>,
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
            configuration_to_request.unwrap_or(ConfigurationToRequest::EMPTY);

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
        Ok(Self { core: dhcp_client_core::client::State::default(), rng, config, stop_receiver })
    }

    async fn watch_configuration(
        &mut self,
        packet_socket_provider: &crate::packetsocket::PacketSocketProviderImpl,
    ) -> Result<ClientWatchConfigurationResponse, Error> {
        let clock = Clock;
        loop {
            let Self { core, rng, config, stop_receiver } = self;
            *core = match core.run(config, packet_socket_provider, rng, &clock, stop_receiver).await
            {
                Err(e) => return Err(Error::Core(e)),
                Ok(step) => match step {
                    dhcp_client_core::client::Step::NextState(core) => core,
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
