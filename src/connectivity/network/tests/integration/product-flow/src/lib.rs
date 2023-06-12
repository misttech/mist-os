// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fuchsia_async::{self as fasync};
use fuchsia_zircon as zx;
use futures::{AsyncReadExt as _, AsyncWriteExt as _, FutureExt as _, StreamExt as _};

use assert_matches::assert_matches;
use netemul::{InStack, RealmTcpListener as _, RealmTcpStream as _, RealmUdpSocket as _};
use netstack_testing_common::{
    constants::ipv6 as ipv6_consts,
    dhcpv4, interfaces, ndp,
    realms::{KnownServiceProvider, Netstack, TestSandboxExt as _},
};
use netstack_testing_macros::netstack_test;
use packet_formats::{
    self,
    icmp::ndp::options::{NdpOptionBuilder, PrefixInformation},
};
use test_case::test_case;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IpSupported {
    Ipv4Only,
    Ipv6Only,
    DualStack,
}

/// Test that network is usable after an interface has its link go down and
/// then back up. The client netstack acquires an IPv4 address via DHCPv4
/// client, and an IPv6 global address via SLAAC in response to injected
/// Router Advertisements; both of these closely resembles production
/// environments. These tests exercise both UDP and TCP sockets, as well as the
/// interface watcher API, and guard against regressions in netstack's ability
/// to recover connectivity (or in the case of interface watcher API,
/// misleading clients of the API into thinking connectivity has not
/// been restored when it has).
#[netstack_test]
#[test_case(IpSupported::Ipv6Only; "ipv6_only")]
#[test_case(IpSupported::Ipv4Only; "ipv4_only")]
#[test_case(IpSupported::DualStack; "dual_stack")]
async fn interface_disruption<N: Netstack>(name: &str, ip_supported: IpSupported) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");

    let client_realm = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client netstack realm");
    let server_realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("{}_server", name),
            &[
                KnownServiceProvider::DhcpServer { persistent: false },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::SecureStash,
            ],
        )
        .expect("failed to create server netstack realm");

    let net = sandbox.create_network("net").await.expect("failed to create network");

    let client_if = client_realm
        .join_network(&net, "client_ep")
        .await
        .expect("failed to join network in client realm");
    const SERVER_IF_NAME: &str = "server_if";
    let server_if = server_realm
        .join_network_with_if_config(
            &net,
            "server_ep",
            netemul::InterfaceConfig { name: Some(SERVER_IF_NAME.into()), metric: None },
        )
        .await
        .expect("failed to join network in server realm");

    let fake_ep = net.create_fake_endpoint().expect("create fake endpoint");

    let client_interfaces_state = client_realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to protocol");
    let server_interfaces_state = server_realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to protocol");

    let server_link_local = interfaces::wait_for_v6_ll(&server_interfaces_state, server_if.id())
        .await
        .expect("wait for link-local address in server realm");

    let send_ra_and_wait_for_addr = || async {
        let send_ra_fut =
            fasync::Interval::new(zx::Duration::from_seconds(4)).for_each(|()| async {
                let options = [NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
                    ipv6_consts::GLOBAL_PREFIX.prefix(),  /* prefix_length */
                    false,                                /* on_link_flag */
                    true,  /* autonomous_address_configuration_flag */
                    99999, /* valid_lifetime */
                    99999, /* preferred_lifetime */
                    ipv6_consts::GLOBAL_PREFIX.network(), /* prefix */
                ))];
                ndp::send_ra_with_router_lifetime(&fake_ep, 9999, &options, server_link_local)
                    .await
                    .expect("failed to send router advertisement")
            });
        let wait_addr_fut =
            interfaces::wait_for_addresses(&client_interfaces_state, client_if.id(), |addresses| {
                addresses.iter().find_map(
                    |&fnet_interfaces_ext::Address {
                         addr: fnet::Subnet { addr, prefix_len: _ },
                         valid_until: _,
                         assignment_state,
                     }| {
                        assert_eq!(
                            assignment_state,
                            fnet_interfaces::AddressAssignmentState::Assigned
                        );
                        match addr {
                            fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: _ }) => None,
                            fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr }) => {
                                let addr = net_types::ip::Ipv6Addr::from_bytes(addr);
                                ipv6_consts::GLOBAL_PREFIX.contains(&addr).then_some(addr)
                            }
                        }
                    },
                )
            })
            .map(|r| r.expect("wait for SLAAC IPv6 address to appear"));
        futures::pin_mut!(send_ra_fut, wait_addr_fut);
        let addr = futures::select! {
            () = send_ra_fut => unreachable!("sending Router Advertisements should never stop"),
            addr = wait_addr_fut => addr
        };
        net_types::ip::IpAddr::V6(addr)
    };
    const SERVER_V6: net_types::ip::Ipv6Addr = ipv6_consts::GLOBAL_ADDR;
    let client_v6 = match ip_supported {
        IpSupported::Ipv4Only => None,
        IpSupported::Ipv6Only | IpSupported::DualStack => {
            server_if
                .add_address_and_subnet_route(fnet::Subnet {
                    addr: fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: SERVER_V6.ipv6_bytes() }),
                    prefix_len: ipv6_consts::GLOBAL_PREFIX.prefix(),
                })
                .await
                .expect("add IPv6 address and subnet route to server");

            // Allow the client realm to generate global IPv6 addresses.
            Some(send_ra_and_wait_for_addr().await)
        }
    };

    client_if.start_dhcp::<InStack>().await.expect("start DHCPv4 client");

    let server_v4 = {
        let dhcpv4::TestConfig { server_addr: fnet::Ipv4Address { addr }, managed_addrs: _ } =
            dhcpv4::DEFAULT_TEST_CONFIG;
        net_types::ip::Ipv4Addr::from(addr)
    };
    // Set up DHCPv4 server on server.
    match ip_supported {
        IpSupported::Ipv6Only => {}
        IpSupported::Ipv4Only | IpSupported::DualStack => {
            server_if
                .add_address_and_subnet_route(
                    dhcpv4::DEFAULT_TEST_CONFIG.server_addr_with_prefix().into_ext(),
                )
                .await
                .expect("add IPv4 address and subnet route to server");

            let dhcp_server = server_realm
                .connect_to_protocol::<fnet_dhcp::Server_Marker>()
                .expect("failed to connect to DHCP server");
            let param_iter =
                dhcpv4::DEFAULT_TEST_CONFIG.dhcp_parameters().into_iter().chain(std::iter::once(
                    fnet_dhcp::Parameter::BoundDeviceNames(vec![SERVER_IF_NAME.to_string()]),
                ));
            dhcpv4::set_server_settings(
                &dhcp_server,
                param_iter,
                std::iter::once(fnet_dhcp::Option_::Router(vec![
                    dhcpv4::DEFAULT_TEST_CONFIG.server_addr,
                ])),
            )
            .await;

            dhcp_server
                .start_serving()
                .await
                .expect("failed to call dhcp/Server.StartServing")
                .map_err(zx::Status::from_raw)
                .expect("dhcp/Server.StartServing returned error");
        }
    }
    let wait_for_dhcpv4 = || async {
        let mut state = fidl_fuchsia_net_interfaces_ext::InterfaceState::Unknown(client_if.id());
        let fnet::Subnet { addr, prefix_len: _ } = fnet_interfaces_ext::wait_interface_with_id(
            fnet_interfaces_ext::event_stream_from_state(
                &client_interfaces_state,
                fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
            )
            .expect("get interface event stream"),
            &mut state,
            |fnet_interfaces_ext::Properties {
                 addresses,
                 has_default_ipv4_route,
                 id: _,
                 name: _,
                 device_class: _,
                 online: _,
                 has_default_ipv6_route: _,
             }| {
                if !has_default_ipv4_route {
                    return None;
                }
                addresses.iter().find_map(
                    |&fnet_interfaces_ext::Address { addr, valid_until: _, assignment_state }| {
                        assert_eq!(
                            assignment_state,
                            fnet_interfaces::AddressAssignmentState::Assigned
                        );
                        (addr == dhcpv4::DEFAULT_TEST_CONFIG.expected_acquired()).then_some(addr)
                    },
                )
            },
        )
        .await
        .expect("wait for DHCPv4 address and default route");

        let addr = assert_matches!(addr, fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr }) => addr);
        net_types::ip::IpAddr::V4(net_types::ip::Ipv4Addr::new(addr))
    };
    let client_v4 = match ip_supported {
        IpSupported::Ipv6Only => None,
        IpSupported::Ipv4Only | IpSupported::DualStack => Some(wait_for_dhcpv4().await),
    };

    const CLIENT_PORT: u16 = 12345;
    const SERVER_PORT: u16 = 54321;
    let into_sockaddr =
        |addr: net_types::ip::IpAddr<net_types::ip::Ipv4Addr, net_types::ip::Ipv6Addr>, port| {
            match addr {
                net_types::ip::IpAddr::V4(addr_v4) => {
                    std::net::SocketAddr::from((addr_v4.ipv4_bytes(), port))
                }
                net_types::ip::IpAddr::V6(addr_v6) => {
                    std::net::SocketAddr::from((addr_v6.ipv6_bytes(), port))
                }
            }
        };
    let bind_udp = |realm, sockaddr| async move {
        let sock =
            fasync::net::UdpSocket::bind_in_realm(realm, sockaddr).await.unwrap_or_else(|e| {
                panic!("failed to bind UDP socket to {} in server realm: {}", sockaddr, e)
            });
        sock
    };
    let setup = |client_realm,
                 server_realm,
                 client_addr: Option<
        net_types::ip::IpAddr<net_types::ip::Ipv4Addr, net_types::ip::Ipv6Addr>,
    >,
                 server_addr| async move {
        if let Some(client_addr) = client_addr {
            let server_sockaddr = into_sockaddr(server_addr, SERVER_PORT);
            let client_sockaddr = into_sockaddr(client_addr.into(), CLIENT_PORT);

            let server_udp = bind_udp(server_realm, server_sockaddr).await;
            let client_udp = bind_udp(client_realm, client_sockaddr).await;

            let listener = fasync::net::TcpListener::listen_in_realm(server_realm, server_sockaddr)
                .await
                .expect("server realm listen");
            let connect_fut = fasync::net::TcpStream::bind_and_connect_in_realm(
                client_realm,
                client_sockaddr,
                server_sockaddr,
            )
            .map(|r| r.expect("bind and connect"));
            let accept_fut = listener.accept().map(|r| r.expect("accept"));
            let (client_tcp_stream, (_, server_tcp_stream, _)): (
                _,
                (fasync::net::TcpListener, _, std::net::SocketAddr),
            ) = futures::future::join(connect_fut, accept_fut).await;
            Some((client_udp, server_udp, client_tcp_stream, server_tcp_stream))
        } else {
            None
        }
    };
    let (mut v6, mut v4) = futures::future::join(
        setup(&client_realm, &server_realm, client_v6, SERVER_V6.into()),
        setup(&client_realm, &server_realm, client_v4, server_v4.into()),
    )
    .await;

    let mut message_id = 0;
    let mut next_message_id = || {
        message_id += 1;
        message_id
    };
    async fn udp_send_and_recv(
        message_id: u8,
        client_sock: &fasync::net::UdpSocket,
        server_sock: &fasync::net::UdpSocket,
    ) {
        let message = format!("Message {}", message_id);
        let server_sockaddr = server_sock.local_addr().expect("server socket local addr");
        let got =
            client_sock.send_to(message.as_bytes(), server_sockaddr).await.expect("send failed");
        assert_eq!(got, message.len());

        let mut buf: [u8; 32] = [0; 32];
        let (got_len, got_sockaddr) =
            server_sock.recv_from(&mut buf).await.expect("server receive failed");
        assert_eq!(got_len, message.len());
        assert_eq!(&buf[..got_len], message.as_bytes());
        assert_eq!(got_sockaddr, client_sock.local_addr().expect("client socket local addr"));
    }
    async fn tcp_send_and_recv(
        message_id: u8,
        client: &mut fasync::net::TcpStream,
        server: &mut fasync::net::TcpStream,
    ) {
        let message = format!("Message {}", message_id);
        let mut buf = vec![0; message.len()];
        client.write_all(message.as_bytes()).await.expect("write TCP bytes");
        server.read_exact(&mut buf).await.expect("read TCP");
        assert_eq!(&buf, message.as_bytes());
    }
    futures::stream::iter([v4.as_mut(), v6.as_mut()].into_iter())
        .for_each_concurrent(None, |sockets| {
            futures::future::OptionFuture::from(sockets.map(
                |(client_udp, server_udp, client_tcp, server_tcp)| {
                    let (tcp_message_id, udp_message_id) = (next_message_id(), next_message_id());
                    futures::future::join(
                        udp_send_and_recv(udp_message_id, client_udp, server_udp),
                        tcp_send_and_recv(tcp_message_id, client_tcp, server_tcp),
                    )
                },
            ))
            .map(|_: Option<((), ())>| ())
        })
        .await;

    client_if.set_link_up(false).await.expect("failed to set client interface link down");
    // Waiting for interface watcher to return that the interface is offline
    // enables the test to immediately assert sending on sockets to fail.
    interfaces::wait_for_online(&client_interfaces_state, client_if.id(), false)
        .await
        .expect("wait for client interface to go offline");

    async fn send_while_interface_offline(
        (client_udp, server_udp, client_tcp, _): &mut (
            fasync::net::UdpSocket,
            fasync::net::UdpSocket,
            fasync::net::TcpStream,
            fasync::net::TcpStream,
        ),
        udp_message_id: u8,
        tcp_message_id: u8,
    ) -> u8 {
        let server_sockaddr = server_udp.local_addr().expect("server get local sockaddr");
        futures::future::join(
            async {
                let message = format!("Message {}", udp_message_id);
                let got = client_udp.send_to(message.as_bytes(), server_sockaddr).await;

                assert_matches!(got, Err(e) => {
                    // TODO(https://github.com/rust-lang/rust/issues/86442): once
                    // std::io::ErrorKind::HostUnreachable is stable, we should use that instead.
                    assert_eq!(e.raw_os_error(), Some(libc::EHOSTUNREACH));
                });
            },
            async {
                let message = format!("Message {}", tcp_message_id);
                client_tcp.write_all(message.as_bytes()).await.expect("TCP write failed");
            },
        )
        .await;
        tcp_message_id
    }
    let (tcp_v6_message_id, tcp_v4_message_id): (Option<u8>, Option<u8>) = futures::future::join(
        futures::future::OptionFuture::from(v6.as_mut().map(|sockets| {
            send_while_interface_offline(sockets, next_message_id(), next_message_id())
        })),
        futures::future::OptionFuture::from(v4.as_mut().map(|sockets| {
            send_while_interface_offline(sockets, next_message_id(), next_message_id())
        })),
    )
    .await;

    client_if.set_link_up(true).await.expect("failed to set client interface link up");
    interfaces::wait_for_online(&client_interfaces_state, client_if.id(), true)
        .await
        .expect("wait for client interface to go offline");

    // Induce the IPv6 default route to be re-added.
    match ip_supported {
        IpSupported::Ipv4Only => {}
        IpSupported::Ipv6Only | IpSupported::DualStack => {
            let _: net_types::ip::IpAddr = send_ra_and_wait_for_addr().await;
        }
    };
    match ip_supported {
        IpSupported::Ipv6Only => {}
        IpSupported::Ipv4Only | IpSupported::DualStack => {
            let _: net_types::ip::IpAddr = wait_for_dhcpv4().await;
        }
    }

    futures::stream::iter(
        [(v4.as_mut(), tcp_v4_message_id), (v6.as_mut(), tcp_v6_message_id)].into_iter(),
    )
    .for_each_concurrent(None, |(sockets, want_message_id)| {
        futures::future::OptionFuture::from(sockets.map(
            |(client_udp, server_udp, client_tcp, server_tcp)| {
                let udp_message_id = next_message_id();
                let tcp_message_id = next_message_id();
                async move {
                    udp_send_and_recv(udp_message_id, client_udp, server_udp).await;

                    // Read the TCP message sent while interface was offline.
                    let message =
                        format!("Message {}", want_message_id.expect("message ID must be present"));
                    let mut buf = vec![0; message.len()];
                    server_tcp.read_exact(&mut buf).await.expect("read TCP");
                    assert_eq!(&buf, message.as_bytes());

                    tcp_send_and_recv(tcp_message_id, client_tcp, server_tcp).await;
                }
            },
        ))
        .map(|_: Option<()>| ())
    })
    .await;
}
