// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net_dhcp::{ClientExitReason, ClientProviderRequest, ClientProviderRequestStream};
use fidl_fuchsia_posix_socket_packet as fpacket;
use futures::{StreamExt as _, TryStreamExt as _};
use std::{cell::RefCell, collections::HashSet};

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("tried to create multiple DHCP clients with interface_id={0}; exiting")]
    MultipleClientsOnSameInterface(u64),
}

struct InterfacesInUse {
    set: RefCell<HashSet<u64>>,
}

impl InterfacesInUse {
    fn new() -> Self {
        Self { set: RefCell::new(HashSet::new()) }
    }

    fn mark_interface_in_use(
        &self,
        interface_id: u64,
    ) -> Result<InterfaceInUseHandle<'_>, AlreadyInUse> {
        let Self { set } = self;
        if set.borrow_mut().insert(interface_id) {
            Ok(InterfaceInUseHandle { parent: self, interface_id })
        } else {
            Err(AlreadyInUse)
        }
    }

    fn remove(&self, interface_id: u64) {
        let Self { set } = self;
        assert!(set.borrow_mut().remove(&interface_id));
    }
}

#[must_use]
struct InterfaceInUseHandle<'a> {
    parent: &'a InterfacesInUse,
    interface_id: u64,
}

impl<'a> Drop for InterfaceInUseHandle<'a> {
    fn drop(&mut self) {
        let Self { parent, interface_id } = self;
        parent.remove(*interface_id);
    }
}

struct AlreadyInUse;

pub(crate) async fn serve_client_provider(
    stream: ClientProviderRequestStream,
    provider: fpacket::ProviderProxy,
) -> Result<(), Error> {
    let provider = &provider;
    let interfaces_in_use = &InterfacesInUse::new();

    stream
        .filter_map(|result| {
            futures::future::ready(result.map(Some).unwrap_or_else(|error| match error {
                fidl::Error::ClientChannelClosed { status: _, protocol_name: _ } => None,
                error => {
                    panic!("unexpected FIDL error in client provider request stream: {error:?}")
                }
            }))
        })
        .map(Ok)
        .try_for_each_concurrent(None, |request| async move {
            match request {
                ClientProviderRequest::NewClient {
                    interface_id,
                    params,
                    request,
                    control_handle: _,
                } => {
                    let (client_requests_stream, control_handle) =
                        request.into_stream_and_control_handle().expect("fidl error");
                    let _handle: InterfaceInUseHandle<'_> = {
                        match interfaces_in_use.mark_interface_in_use(interface_id) {
                            Ok(handle) => handle,
                            Err(AlreadyInUse) => {
                                control_handle
                                    .send_on_exit(ClientExitReason::ClientAlreadyExistsOnInterface)
                                    .unwrap_or_else(|e| {
                                        tracing::error!(
                                            "FIDL error while sending on_exit event: {:?}",
                                            e
                                        );
                                    });
                                return Err(Error::MultipleClientsOnSameInterface(interface_id));
                            }
                        }
                    };

                    let provider = &crate::packetsocket::PacketSocketProviderImpl::new(
                        provider.clone(),
                        interface_id,
                    );
                    let mac = match provider.get_mac().await {
                        Ok(mac) => mac,
                        Err(e) => {
                            tracing::error!("error while getting MAC address: {:?}", e);
                            control_handle
                                .send_on_exit({
                                    match e {
                                        dhcp_client_core::SocketError::UnsupportedHardwareType
                                        | dhcp_client_core::SocketError::NoInterface => {
                                            ClientExitReason::InvalidInterface
                                        }
                                        dhcp_client_core::SocketError::FailedToOpen(_)
                                        | dhcp_client_core::SocketError::Other(_) => {
                                            ClientExitReason::UnableToOpenSocket
                                        }
                                    }
                                })
                                .unwrap_or_else(|e| {
                                    tracing::error!(
                                        "FIDL error while sending on_exit event: {:?}",
                                        e
                                    );
                                });
                            return Ok(());
                        }
                    };

                    crate::client::serve_client(mac, provider, params, client_requests_stream)
                        .await
                        .unwrap_or_else(|error| match error {
                            crate::client::Error::Exit(reason) => {
                                tracing::info!("client exiting: {:?}", reason);
                                control_handle.send_on_exit(reason).unwrap_or_else(|e| {
                                    tracing::error!(
                                        "FIDL error while sending on_exit event: {:?}",
                                        e
                                    );
                                });
                            }
                            crate::client::Error::Fidl(e) => {
                                tracing::error!("FIDL error while serving client: {:?}", e);
                            }
                            crate::client::Error::Core(e) => {
                                tracing::error!("error while serving client: {:?}", e);
                            }
                        });
                    Ok(())
                }
            }
        })
        .await
}

#[cfg(test)]
// TODO(https://fxbug.dev/124935): Move component-level integration tests to
// src/connectivity/network/tests/integration.
mod test {
    use assert_matches::assert_matches;
    use fidl::endpoints;
    use fidl_fuchsia_net_dhcp::{
        ClientEvent, ClientExitReason, ClientMarker, ClientProviderMarker, NewClientParams,
    };
    use fidl_fuchsia_net_ext as fnet_ext;
    use fidl_fuchsia_netemul as fnetemul;
    use fidl_fuchsia_netemul_network as fnetemul_network;
    use fuchsia_async as fasync;
    use futures::{join, FutureExt, TryStreamExt};
    use netstack_testing_common::realms::TestSandboxExt as _;

    const MAC: net_types::ethernet::Mac = net_declare::net_mac!("00:00:00:00:00:01");

    struct SingleInterfaceTestRealm<'a> {
        realm: netemul::TestRealm<'a>,
        iface: netemul::TestInterface<'a>,
    }

    async fn create_single_interface_test_realm(
        sandbox: &netemul::TestSandbox,
    ) -> SingleInterfaceTestRealm<'_> {
        let network = sandbox
            .create_network("dhcp-test-network")
            .await
            .expect("create network should succeed");
        let realm: netemul::TestRealm<'_> = sandbox
            .create_netstack_realm_with::<netstack_testing_common::realms::Netstack2, _, _>(
                "dhcp-test-realm-a",
                // TODO(https://fxbug.dev/124935): Add a `KnownServiceProvider`
                // variant for the DHCP client to `netstack_testing_common`.
                std::iter::once(fnetemul::ChildDef {
                    source: Some(fnetemul::ChildSource::Component(
                        "#meta/dhcp-client.cm".to_owned(),
                    )),
                    name: Some("dhcp-client".to_owned()),
                    exposes: Some(vec!["fuchsia.net.dhcp.ClientProvider".to_owned()]),
                    uses: Some(fnetemul::ChildUses::Capabilities(vec![
                        fnetemul::Capability::ChildDep(fnetemul::ChildDep {
                            name: Some(netstack_testing_common::realms::constants::netstack::COMPONENT_NAME.to_owned()),
                            capability: Some(fnetemul::ExposedCapability::Protocol(
                                "fuchsia.posix.socket.packet.Provider".to_owned(),
                            )),
                            ..fnetemul::ChildDep::EMPTY
                        }),
                    ])),
                    program_args: None,
                    eager: None,
                    ..fnetemul::ChildDef::EMPTY
                }),
            )
            .expect("create realm should succeed");

        let iface = realm
            .join_network_with(
                &network,
                "iface",
                fnetemul_network::EndpointConfig {
                    mtu: netemul::DEFAULT_MTU,
                    mac: Some(Box::new(fnet_ext::MacAddress { octets: MAC.bytes() }.into())),
                },
                netemul::InterfaceConfig { name: Some("iface".into()), metric: None },
            )
            .await
            .expect("join network with realm should succeed");

        SingleInterfaceTestRealm { realm, iface }
    }

    #[fasync::run_singlethreaded(test)]
    async fn client_provider_two_overlapping_clients_on_same_interface() {
        let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
        let SingleInterfaceTestRealm { realm, iface } =
            create_single_interface_test_realm(&sandbox).await;

        let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
        let iface = &iface;

        let (client_a, server_end_a) =
            endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
        let (client_b, server_end_b) =
            endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");

        proxy
            .new_client(
                iface.id(),
                NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..NewClientParams::EMPTY
                },
                server_end_a,
            )
            .expect("creating new client should succeed");

        proxy
            .new_client(
                iface.id(),
                NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..NewClientParams::EMPTY
                },
                server_end_b,
            )
            .expect("creating new client should succeed");

        let watch_fut_a = client_a.watch_configuration();
        let watch_fut_b = client_b.watch_configuration();
        let (result_a, result_b) = join!(watch_fut_a, watch_fut_b);

        assert_matches!(result_a, Err(fidl::Error::ClientChannelClosed { .. }));
        assert_matches!(result_b, Err(fidl::Error::ClientChannelClosed { .. }));

        let on_exit = client_b
            .take_event_stream()
            .try_next()
            .now_or_never()
            .expect("event should be already available")
            .expect("event stream should not have ended before yielding exit reason")
            .expect("event stream should not have FIDL error");
        assert_matches!(
            on_exit,
            ClientEvent::OnExit { reason: ClientExitReason::ClientAlreadyExistsOnInterface }
        )
    }

    #[fasync::run_singlethreaded(test)]
    async fn client_provider_two_non_overlapping_clients_on_same_interface() {
        let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

        let SingleInterfaceTestRealm { realm, iface } =
            create_single_interface_test_realm(&sandbox).await;

        let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
        let iface = &iface;

        let proxy = &proxy;
        // Executing the following block twice demonstrates that we can run
        // and shutdown DHCP clients on the same interface without running
        // afoul of the multiple-clients-on-same-interface restriction.
        for () in [(), ()] {
            let (client, server_end) =
                endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");

            proxy
                .new_client(
                    iface.id(),
                    NewClientParams {
                        configuration_to_request: None,
                        request_ip_address: Some(true),
                        ..NewClientParams::EMPTY
                    },
                    server_end,
                )
                .expect("creating new client should succeed");

            client.shutdown().expect("shutdown call should not have FIDL error");
            let watch_result = client.watch_configuration().await;

            assert_matches!(watch_result, Err(fidl::Error::ClientChannelClosed { .. }));

            let on_exit = client
                .take_event_stream()
                .try_next()
                .now_or_never()
                .expect("event should be already available")
                .expect("event stream should not have ended before yielding exit reason")
                .expect("event stream should not have FIDL error");
            assert_matches!(
                on_exit,
                ClientEvent::OnExit { reason: ClientExitReason::GracefulShutdown }
            );
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn client_provider_double_watch() {
        let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

        let SingleInterfaceTestRealm { realm, iface } =
            create_single_interface_test_realm(&sandbox).await;

        let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
        let iface = &iface;

        let (client, server_end) =
            endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
        proxy
            .new_client(
                iface.id(),
                NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..NewClientParams::EMPTY
                },
                server_end,
            )
            .expect("new client");

        let watch_fut_a = client.watch_configuration();
        let watch_fut_b = client.watch_configuration();
        let (result_a, result_b) = join!(watch_fut_a, watch_fut_b);

        assert_matches!(result_a, Err(_));
        assert_matches!(result_b, Err(_));

        let on_exit = client
            .take_event_stream()
            .try_next()
            .now_or_never()
            .expect("event should be already available")
            .expect("event stream should not have ended before yielding exit reason")
            .expect("event stream should not have FIDL error");
        assert_matches!(
            on_exit,
            ClientEvent::OnExit { reason: ClientExitReason::WatchConfigurationAlreadyPending }
        )
    }

    #[fasync::run_singlethreaded(test)]
    async fn client_provider_shutdown() {
        let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

        let SingleInterfaceTestRealm { realm, iface } =
            create_single_interface_test_realm(&sandbox).await;

        let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
        let iface = &iface;

        let (client, server_end) =
            endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
        proxy
            .new_client(
                iface.id(),
                NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..NewClientParams::EMPTY
                },
                server_end,
            )
            .expect("creating new client should succeed");

        let watch_fut = client.watch_configuration();
        let shutdown_fut = async {
            futures_lite::future::yield_now().await;
            client.shutdown().expect("shutdown should not have FIDL error");
        };
        let (watch_result, ()) = join!(watch_fut, shutdown_fut);

        assert_matches!(watch_result, Err(_));

        let on_exit = client
            .take_event_stream()
            .try_next()
            .now_or_never()
            .expect("event should be already available")
            .expect("event stream should not have ended before yielding exit reason")
            .expect("event stream should not have FIDL error");
        assert_matches!(on_exit, ClientEvent::OnExit { reason: ClientExitReason::GracefulShutdown })
    }
}
