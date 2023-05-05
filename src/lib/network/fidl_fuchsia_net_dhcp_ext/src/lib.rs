// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for types in the `fidl_fuchsia_net_dhcp` crate.
#![deny(missing_docs)]
#![warn(clippy::all)]

use std::{collections::HashSet, fmt::Display, num::NonZeroU64};

use anyhow::anyhow;
use async_trait::async_trait;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_stack as fnet_stack;
use futures::{pin_mut, TryStreamExt as _};
use net_declare::fidl_subnet;
use net_types::{ip::Ipv4Addr, SpecifiedAddr, Witness as _};

/// The default `fnet_dhcp::NewClientParams`.
pub fn default_new_client_params() -> fnet_dhcp::NewClientParams {
    fnet_dhcp::NewClientParams {
        configuration_to_request: Some(fnet_dhcp::ConfigurationToRequest {
            routers: Some(true),
            dns_servers: Some(true),
            ..fnet_dhcp::ConfigurationToRequest::default()
        }),
        request_ip_address: Some(true),
        ..fnet_dhcp::NewClientParams::default()
    }
}

/// Configuration acquired by the DHCP client.
#[derive(Default, Debug)]
pub struct Configuration {
    /// The acquired address.
    pub address: Option<Address>,
    /// Acquired DNS servers.
    pub dns_servers: Vec<fnet::Ipv4Address>,
    /// Acquired routers.
    pub routers: Vec<SpecifiedAddr<Ipv4Addr>>,
}

/// Error while manipulating a forwarding entry.
#[derive(Debug, thiserror::Error)]
#[error("error {operation} route for DHCPv4-learned router {router} on interface {device_id}: {error:?}")]
pub struct ForwardingEntryError {
    operation: ForwardingEntryOperation,
    device_id: NonZeroU64,
    router: SpecifiedAddr<Ipv4Addr>,
    error: fnet_stack::Error,
}

/// Domain errors for this crate.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// A FIDL domain object was invalid.
    #[error("invalid FIDL domain object: {0:?}")]
    ApiViolation(anyhow::Error),
    /// Errors were encountered while manipulating forwarding entries.
    #[error("errors while manipulating forwarding entries: {0:?}")]
    ForwardingEntry(Vec<ForwardingEntryError>),
    /// A FIDL error was encountered.
    #[error("fidl error: {0:?}")]
    Fidl(fidl::Error),
    /// An invalid ClientExitReason was observed on the client's event stream.
    #[error("invalid exit reason: {0:?}")]
    WrongExitReason(fnet_dhcp::ClientExitReason),
    /// No ClientExitReason was provided, when one was expected.
    #[error("missing exit reason")]
    MissingExitReason,
}

/// The default subnet used as the destination while populating a
/// `fuchsia.net.stack.ForwardingEntry` while applying newly-discovered routers.
pub const DEFAULT_SUBNET: fnet::Subnet = fidl_subnet!("0.0.0.0/0");

#[derive(Copy, Clone, Debug)]
enum ForwardingEntryOperation {
    Adding,
    Deleting,
}

impl Display for ForwardingEntryOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardingEntryOperation::Adding => f.write_str("adding"),
            ForwardingEntryOperation::Deleting => f.write_str("deleting"),
        }
    }
}

/// Applies a new set of routers to a given `fuchsia.net.stack.Stack` and
/// set of configured routers by deleting forwarding entries for
/// newly-absent routers and adding forwarding entries for newly-present
/// ones.
pub async fn apply_new_routers(
    device_id: NonZeroU64,
    stack: &fnet_stack::StackProxy,
    configured_routers: &mut HashSet<SpecifiedAddr<Ipv4Addr>>,
    new_routers: impl IntoIterator<Item = SpecifiedAddr<Ipv4Addr>>,
) -> Result<(), Error> {
    let fwd_entry = |next_hop: &SpecifiedAddr<Ipv4Addr>| fnet_stack::ForwardingEntry {
        subnet: DEFAULT_SUBNET,
        device_id: device_id.get(),
        next_hop: Some(Box::new(fnet::IpAddress::Ipv4(fnet::Ipv4Address {
            addr: next_hop.get().ipv4_bytes(),
        }))),
        metric: fnet_stack::UNSPECIFIED_METRIC,
    };

    let mut errors = Vec::new();

    let new_routers = new_routers.into_iter().collect::<HashSet<_>>();
    for router in new_routers.difference(configured_routers) {
        match stack.add_forwarding_entry(&mut fwd_entry(router)).await.map_err(Error::Fidl)? {
            Ok(()) => (),
            Err(e) => {
                errors.push(ForwardingEntryError {
                    operation: ForwardingEntryOperation::Adding,
                    device_id,
                    router: *router,
                    error: e,
                });
            }
        }
    }

    for router in configured_routers.difference(&new_routers) {
        match stack.del_forwarding_entry(&mut fwd_entry(router)).await.map_err(Error::Fidl)? {
            Ok(()) => (),
            Err(e) => {
                errors.push(ForwardingEntryError {
                    operation: ForwardingEntryOperation::Deleting,
                    device_id,
                    router: *router,
                    error: e,
                });
            }
        }
    }

    *configured_routers = new_routers;
    Ok(())
}

impl TryFrom<fnet_dhcp::ClientWatchConfigurationResponse> for Configuration {
    type Error = Error;
    fn try_from(
        fnet_dhcp::ClientWatchConfigurationResponse {
            address,
            dns_servers,
            routers,
            ..
        }: fnet_dhcp::ClientWatchConfigurationResponse,
    ) -> Result<Self, Error> {
        let address = address
            .map(
                |fnet_dhcp::Address {
                     address, address_parameters, address_state_provider, ..
                 }| {
                    Ok(Address {
                        address: address
                            .ok_or(anyhow!("Ipv4AddressWithPrefix should be present"))?,
                        address_parameters: address_parameters
                            .ok_or(anyhow!("AddressParameters should be present"))?,
                        address_state_provider: address_state_provider
                            .ok_or(anyhow!("AddressStateProvider should be present"))?,
                    })
                },
            )
            .transpose()
            .map_err(Error::ApiViolation);
        Ok(Configuration {
            address: address?,
            dns_servers: dns_servers.unwrap_or_default(),
            routers: routers
                .unwrap_or_default()
                .into_iter()
                .flat_map(|addr| SpecifiedAddr::new(addr.into_ext()))
                .collect(),
        })
    }
}

/// An IPv4 address acquired by the DHCP client.
#[derive(Debug)]
pub struct Address {
    /// The acquired address and discovered prefix length.
    pub address: fnet::Ipv4AddressWithPrefix,
    /// Parameters for the acquired address.
    pub address_parameters: fnet_interfaces_admin::AddressParameters,
    /// The server end for the AddressStateProvider owned by the DHCP client.
    pub address_state_provider: ServerEnd<fnet_interfaces_admin::AddressStateProviderMarker>,
}

impl Address {
    /// Adds this address via `fuchsia.net.interfaces.admin.Control`.
    pub fn add_to(
        self,
        control: &fnet_interfaces_ext::admin::Control,
    ) -> Result<
        (),
        (
            fnet::Ipv4AddressWithPrefix,
            fnet_interfaces_ext::admin::TerminalError<
                fnet_interfaces_admin::InterfaceRemovedReason,
            >,
        ),
    > {
        let Self { address, address_parameters, address_state_provider } = self;
        control
            .add_address(&mut address.into_ext(), address_parameters, address_state_provider)
            .map_err(|e| (address, e))
    }
}

type ConfigurationStream = async_utils::hanging_get::client::HangingGetStream<
    fnet_dhcp::ClientProxy,
    fnet_dhcp::ClientWatchConfigurationResponse,
>;

/// Produces a stream of acquired DHCP configuration by executing the hanging
/// get on the provided DHCP client proxy.
pub fn configuration_stream(
    client: fnet_dhcp::ClientProxy,
) -> impl futures::Stream<Item = Result<Configuration, Error>> {
    ConfigurationStream::new_eager_with_fn_ptr(client, fnet_dhcp::ClientProxy::watch_configuration)
        .map_err(Error::Fidl)
        .and_then(|config| futures::future::ready(Configuration::try_from(config)))
}

/// Extension trait on `fidl_fuchsia_net_dhcp::ClientProviderProxy`.
pub trait ClientProviderExt {
    /// Construct a new DHCP client.
    fn new_client_ext(
        &self,
        interface_id: NonZeroU64,
        new_client_params: fnet_dhcp::NewClientParams,
    ) -> fnet_dhcp::ClientProxy;
}

impl ClientProviderExt for fnet_dhcp::ClientProviderProxy {
    fn new_client_ext(
        &self,
        interface_id: NonZeroU64,
        new_client_params: fnet_dhcp::NewClientParams,
    ) -> fnet_dhcp::ClientProxy {
        let (client, server) = fidl::endpoints::create_proxy::<fnet_dhcp::ClientMarker>()
            .expect("create DHCPv4 client fidl endpoints");
        self.new_client(interface_id.get(), new_client_params, server)
            .expect("create new DHCPv4 client");
        client
    }
}

/// Extension trait on `fidl_fuchsia_net_dhcp::ClientProxy`.
#[async_trait]
pub trait ClientExt {
    /// Shuts down the client, watching for the `GracefulShutdown` exit event.
    ///
    /// Returns an error if the `GracefulShutdown` exit event is not observed.
    async fn shutdown_ext(&self, event_stream: fnet_dhcp::ClientEventStream) -> Result<(), Error>;
}

#[async_trait]
impl ClientExt for fnet_dhcp::ClientProxy {
    async fn shutdown_ext(&self, event_stream: fnet_dhcp::ClientEventStream) -> Result<(), Error> {
        self.shutdown().map_err(Error::Fidl)?;

        let stream = event_stream.map_err(Error::Fidl).try_filter_map(|event| async move {
            match event {
                fnet_dhcp::ClientEvent::OnExit { reason } => Ok(match reason {
                    fnet_dhcp::ClientExitReason::ClientAlreadyExistsOnInterface
                    | fnet_dhcp::ClientExitReason::WatchConfigurationAlreadyPending
                    | fnet_dhcp::ClientExitReason::InvalidInterface
                    | fnet_dhcp::ClientExitReason::InvalidParams
                    | fnet_dhcp::ClientExitReason::NetworkUnreachable
                    | fnet_dhcp::ClientExitReason::AddressRemovedByUser
                    | fnet_dhcp::ClientExitReason::AddressStateProviderError
                    | fnet_dhcp::ClientExitReason::UnableToOpenSocket => {
                        return Err(Error::WrongExitReason(reason))
                    }
                    fnet_dhcp::ClientExitReason::GracefulShutdown => Some(()),
                }),
            }
        });

        pin_mut!(stream);
        stream.try_next().await.and_then(|option| match option {
            Some(()) => Ok(()),
            None => Err(Error::MissingExitReason),
        })
    }
}

#[cfg(test)]
mod test {
    use crate::{ClientExt as _, Error};

    use std::collections::HashSet;

    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_net as fnet;
    use fidl_fuchsia_net_dhcp as fnet_dhcp;
    use fidl_fuchsia_net_ext::IntoExt as _;
    use fidl_fuchsia_net_stack as fnet_stack;
    use fuchsia_async as fasync;
    use futures::{join, pin_mut, FutureExt as _, StreamExt as _};
    use net_declare::net_ip_v4;
    use net_types::{
        ip::{Ip, Ipv4, Ipv4Addr},
        SpecifiedAddr, SpecifiedAddress as _, Witness as _,
    };
    use nonzero_ext::nonzero;
    use proptest::prelude::*;
    use test_case::test_case;

    #[derive(proptest_derive::Arbitrary, Clone, Debug)]
    struct Address {
        include_address: bool,
        include_address_parameters: bool,
        include_address_state_provider: bool,
    }

    // For the purposes of this test, we only care about exercising the case
    // where addresses are specified or unspecified, with no need to be able
    // to distinguish between specified addresses.
    #[derive(proptest_derive::Arbitrary, Clone, Debug)]
    enum GeneratedIpv4Addr {
        Specified,
        Unspecified,
    }

    impl From<GeneratedIpv4Addr> for Ipv4Addr {
        fn from(value: GeneratedIpv4Addr) -> Self {
            match value {
                GeneratedIpv4Addr::Specified => net_ip_v4!("1.1.1.1"),
                GeneratedIpv4Addr::Unspecified => Ipv4::UNSPECIFIED_ADDRESS,
            }
        }
    }

    #[derive(proptest_derive::Arbitrary, Clone, Debug)]
    struct ClientWatchConfigurationResponse {
        address: Option<Address>,
        dns_servers: Option<Vec<GeneratedIpv4Addr>>,
        routers: Option<Vec<GeneratedIpv4Addr>>,
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            failure_persistence: Some(
                Box::<proptest::test_runner::MapFailurePersistence>::default()
            ),
            ..ProptestConfig::default()
        })]

        #[test]
        fn try_into_configuration(response: ClientWatchConfigurationResponse) {
            let make_fidl = |response: &ClientWatchConfigurationResponse| {
                let ClientWatchConfigurationResponse {
                    address,
                    dns_servers,
                    routers,
                } = response.clone();

                fnet_dhcp::ClientWatchConfigurationResponse {
                    address: address.map(
                        |Address {
                            include_address,
                            include_address_parameters,
                            include_address_state_provider
                        }| {
                        fnet_dhcp::Address {
                            address: include_address.then_some(
                                fidl_fuchsia_net::Ipv4AddressWithPrefix {
                                    addr: net_ip_v4!("1.1.1.1").into_ext(),
                                    prefix_len: 24,
                                }
                            ),
                            address_parameters: include_address_parameters.then_some(
                                fidl_fuchsia_net_interfaces_admin::AddressParameters::default()
                            ),
                            address_state_provider: include_address_state_provider.then_some({
                                let (_, server) = fidl::endpoints::create_endpoints();
                                server
                            }),
                            ..Default::default()
                        }
                    }),
                    dns_servers: dns_servers.map(
                        |list| list.into_iter().map(
                            |addr: GeneratedIpv4Addr| net_types::ip::Ipv4Addr::from(
                                addr
                            ).into_ext()
                        ).collect()),
                    routers: routers.map(
                        |list| list.into_iter().map(
                            |addr: GeneratedIpv4Addr| net_types::ip::Ipv4Addr::from(
                                addr
                            ).into_ext()
                        ).collect()),
                    ..Default::default()
                }
            };

            let result = crate::Configuration::try_from(make_fidl(&response));

            if let Some(crate::Configuration {
                address: result_address,
                dns_servers: result_dns_servers,
                routers: result_routers,
            }) = match response.address {
                Some(
                    Address {
                        include_address,
                        include_address_parameters,
                        include_address_state_provider,
                    }
                ) => {
                    prop_assert_eq!(
                        !(
                            include_address &&
                            include_address_parameters &&
                            include_address_state_provider
                        ),
                        result.is_err(),
                        "must reject partially-filled address object"
                    );

                    match result {
                        Err(_) => None,
                        Ok(configuration) => Some(configuration),
                    }
                }
                None => {
                    prop_assert!(result.is_ok(), "absent address is always accepted");
                    Some(result.unwrap())
                }
            } {
                let fnet_dhcp::ClientWatchConfigurationResponse {
                    dns_servers: fidl_dns_servers,
                    routers: fidl_routers,
                    address: fidl_address,
                    ..
                } = make_fidl(&response);
                let want_routers: Vec<net_types::ip::Ipv4Addr> = fidl_routers
                    .unwrap_or_default()
                    .into_iter()
                    .flat_map(
                        |addr| Some(addr.into_ext()).filter(net_types::ip::Ipv4Addr::is_specified)
                    )
                    .collect();
                prop_assert_eq!(
                    result_dns_servers,
                    fidl_dns_servers.unwrap_or_default()
                );
                prop_assert_eq!(
                    result_routers.into_iter().map(|addr| addr.get()).collect::<Vec<_>>(),
                    want_routers
                );

                if let Some(
                    crate::Address {
                        address: result_address,
                        address_parameters: result_address_parameters,
                        address_state_provider: _
                    }
                ) = result_address {
                    let fnet_dhcp::Address {
                        address: fidl_address,
                        address_parameters: fidl_address_parameters,
                        address_state_provider: _,
                        ..
                    } = fidl_address.expect("should be present");

                    prop_assert_eq!(Some(result_address), fidl_address);
                    prop_assert_eq!(Some(result_address_parameters), fidl_address_parameters);
                }
            }
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn apply_new_routers() {
        let (stack_proxy, stack_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_stack::StackMarker>()
                .expect("create stack proxy and stream");

        const REMOVED_ROUTER: Ipv4Addr = net_ip_v4!("1.1.1.1");
        const KEPT_ROUTER: Ipv4Addr = net_ip_v4!("2.2.2.2");
        const ADDED_ROUTER: Ipv4Addr = net_ip_v4!("3.3.3.3");

        let mut configured_routers = [REMOVED_ROUTER, KEPT_ROUTER]
            .into_iter()
            .map(|addr| SpecifiedAddr::new(addr).unwrap())
            .collect::<HashSet<_>>();

        let device_id = nonzero!(5u64);

        let apply_fut = crate::apply_new_routers(
            device_id,
            &stack_proxy,
            &mut configured_routers,
            vec![
                SpecifiedAddr::new(KEPT_ROUTER).unwrap(),
                SpecifiedAddr::new(ADDED_ROUTER).unwrap(),
            ],
        )
        .fuse();

        let stack_fut = async move {
            pin_mut!(stack_stream);
            let (forwarding_entry, responder) = stack_stream
                .next()
                .await
                .expect("should not have ended")
                .expect("should not have error")
                .into_add_forwarding_entry()
                .expect("should be add forwarding entry");
            assert_eq!(
                forwarding_entry,
                fnet_stack::ForwardingEntry {
                    subnet: crate::DEFAULT_SUBNET,
                    device_id: device_id.get(),
                    next_hop: Some(Box::new(net_types::ip::IpAddr::from(ADDED_ROUTER).into_ext())),
                    metric: fnet_stack::UNSPECIFIED_METRIC,
                }
            );
            responder.send(&mut Ok(())).expect("responder send");

            let (forwarding_entry, responder) = stack_stream
                .next()
                .await
                .expect("should not have ended")
                .expect("should not have error")
                .into_del_forwarding_entry()
                .expect("should be del forwarding entry");
            assert_eq!(
                forwarding_entry,
                fnet_stack::ForwardingEntry {
                    subnet: crate::DEFAULT_SUBNET,
                    device_id: device_id.get(),
                    next_hop: Some(Box::new(fnet::IpAddress::Ipv4(fnet::Ipv4Address {
                        addr: REMOVED_ROUTER.ipv4_bytes(),
                    }))),
                    metric: fnet_stack::UNSPECIFIED_METRIC,
                }
            );
            responder.send(&mut Ok(())).expect("responder send");
        }
        .fuse();

        pin_mut!(apply_fut, stack_fut);
        let (apply_result, ()) = join!(apply_fut, stack_fut);
        apply_result.expect("apply should succeed");
    }

    #[test_case(
        None => matches Err(Error::MissingExitReason) ; "no exit reason should cause error"
    )]
    #[test_case(
        Some(fnet_dhcp::ClientExitReason::NetworkUnreachable) => matches Err(Error::WrongExitReason(fnet_dhcp::ClientExitReason::NetworkUnreachable)) ;
        "wrong exit reason should cause error"
    )]
    #[test_case(
        Some(fnet_dhcp::ClientExitReason::GracefulShutdown) => matches Ok(()) ;
        "GracefulShutdown is correct exit reason"
    )]
    #[fasync::run_singlethreaded(test)]
    async fn shutdown_ext(exit_reason: Option<fnet_dhcp::ClientExitReason>) -> Result<(), Error> {
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_dhcp::ClientMarker>()
                .expect("create DHCP client proxy and stream");

        if let Some(exit_reason) = exit_reason {
            stream.control_handle().send_on_exit(exit_reason).expect("send on exit");
        }

        let shutdown_fut = client.shutdown_ext(client.take_event_stream()).fuse();
        let server_fut = async move {
            pin_mut!(stream);
            let _client_control_handle = stream
                .next()
                .await
                .expect("should not have ended")
                .expect("should not have FIDL error")
                .into_shutdown()
                .expect("should be shutdown request");
        }
        .fuse();

        let (shutdown_result, ()) = join!(shutdown_fut, server_fut);
        shutdown_result
    }
}
