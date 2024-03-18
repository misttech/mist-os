// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the INGRESS and LOCAL_INGRESS IP filtering hooks.

#![cfg(test)]

mod matchers;

use std::num::NonZeroU64;

use assert_matches::assert_matches;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext::{
    Action, Change, Controller, ControllerId, Domain, InstalledIpRoutine, InterfaceMatcher, IpHook,
    Matchers, Namespace, NamespaceId, Resource, ResourceId, Routine, RoutineId, RoutineType, Rule,
    RuleId,
};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fidl_fuchsia_posix_socket as fposix_socket;
use fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _};
use futures::{
    future::LocalBoxFuture,
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    FutureExt as _, StreamExt as _, TryFutureExt as _,
};
use net_declare::{fidl_subnet, net_ip_v4, net_ip_v6};
use net_types::{
    ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    SpecifiedAddr,
};
use netemul::{RealmTcpListener as _, RealmUdpSocket as _};
use netstack_testing_common::{
    realms::{Netstack3, TestSandboxExt as _},
    ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use test_case::test_case;

use matchers::{
    AllTraffic, DstAddressRange, DstAddressSubnet, Icmp, InterfaceDeviceClass, InterfaceId,
    InterfaceName, Inversion, Matcher, SrcAddressRange, SrcAddressSubnet, Tcp, TcpDstPort,
    TcpSrcPort, Udp, UdpDstPort, UdpSrcPort,
};

macro_rules! generate_test_cases_for_all_matchers {
    ($test:ident, $hook:path, $suffix:ident) => {
        paste::paste! {
            #[netstack_test]
            #[test_case(AllTraffic; "all traffic")]
            #[test_case(InterfaceId; "incoming interface id")]
            #[test_case(InterfaceName; "incoming interface name")]
            #[test_case(InterfaceDeviceClass; "incoming interface device class")]
            #[test_case(SrcAddressSubnet(Inversion::Default); "src address within subnet")]
            #[test_case(SrcAddressSubnet(Inversion::InverseMatch); "src address outside subnet")]
            #[test_case(SrcAddressRange(Inversion::Default); "src address within range")]
            #[test_case(SrcAddressRange(Inversion::InverseMatch); "src address outside range")]
            #[test_case(DstAddressSubnet(Inversion::Default); "dst address within subnet")]
            #[test_case(DstAddressSubnet(Inversion::InverseMatch); "dst address outside subnet")]
            #[test_case(DstAddressRange(Inversion::Default); "dst address within range")]
            #[test_case(DstAddressRange(Inversion::InverseMatch); "dst address outside range")]
            #[test_case(Tcp; "tcp traffic")]
            #[test_case(TcpSrcPort(Inversion::Default); "tcp src port within range")]
            #[test_case(TcpSrcPort(Inversion::InverseMatch); "tcp src port outside range")]
            #[test_case(TcpDstPort(Inversion::Default); "tcp dst port within range")]
            #[test_case(TcpDstPort(Inversion::InverseMatch); "tcp dst port outside range")]
            #[test_case(Udp; "udp traffic")]
            #[test_case(UdpSrcPort(Inversion::Default); "udp src port within range")]
            #[test_case(UdpSrcPort(Inversion::InverseMatch); "udp src port outside range")]
            #[test_case(UdpDstPort(Inversion::Default); "udp dst port within range")]
            #[test_case(UdpDstPort(Inversion::InverseMatch); "udp dst port outside range")]
            #[test_case(Icmp; "ping")]
            async fn [<$test _ $suffix>]<I: net_types::ip::Ip + TestIpExt + RouterTestIpExt, M: Matcher>(
                name: &str,
                matcher: M,
            ) {
                $test::<I, M>(name, $hook, matcher).await;
            }
        }
    };
}

#[derive(Debug)]
enum IncomingHook {
    Ingress,
    LocalIngress,
}

#[derive(Clone, Copy)]
enum ExpectedConnectivity {
    TwoWay,
    ClientToServerOnly,
    None,
}

trait SocketType {
    type Client;
    type Server;

    async fn bind_sockets(realms: Realms<'_>, addrs: Addrs) -> (Sockets<Self>, SockAddrs);

    async fn run_test<I: ping::FuchsiaIpExt>(
        realms: Realms<'_>,
        sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    );
}

const CLIENT_PAYLOAD: &'static str = "hello from client";
const SERVER_PAYLOAD: &'static str = "hello from server";

/// This is a `SocketType` for use by test cases that are agnostic to the
/// transport protocol that is used. For example, a test exercising filtering on
/// interface device class should apply regardless of whether the filtered
/// traffic happens to be TCP or UDP traffic.
//
/// `IrrelevantToTest` delegates its implementation to `UdpSocket`, which is
/// mostly arbitrary, but has the benefit of being less sensitive to filtering
/// because it is connectionless: when traffic is allowed in one direction but
/// not the other, for example, it is still possible to observe that traffic at
/// the socket layer in UDP, whereas connectivity must be bidirectional for the
/// TCP handshake to complete, which implies that one-way connectivity is
/// indistinguishable from no connectivity at the TCP socket layer.
pub(crate) struct IrrelevantToTest;

impl SocketType for IrrelevantToTest {
    type Client = <UdpSocket as SocketType>::Client;
    type Server = <UdpSocket as SocketType>::Server;

    async fn bind_sockets(realms: Realms<'_>, addrs: Addrs) -> (Sockets<Self>, SockAddrs) {
        let (Sockets { client, server }, addrs) = UdpSocket::bind_sockets(realms, addrs).await;
        (Sockets { client, server }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        realms: Realms<'_>,
        sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let Sockets { client: client_sock, server: server_sock } = sockets;
        UdpSocket::run_test::<I>(
            realms,
            Sockets { client: client_sock, server: server_sock },
            sock_addrs,
            expected_connectivity,
        )
        .await
    }
}

pub(crate) struct TcpSocket;

impl SocketType for TcpSocket {
    // NB: even though we eventually convert this to a `TcpStream` when we connect
    // it to the server, we use a `socket2::Socket` to store it at rest because
    // neither `std` nor `fuchsia_async` provide a way to bind a local socket
    // without connecting it to a remote.
    type Client = socket2::Socket;
    type Server = fasync::net::AcceptStream;

    async fn bind_sockets(realms: Realms<'_>, addrs: Addrs) -> (Sockets<Self>, SockAddrs) {
        let Realms { client, server } = realms;
        let Addrs { client: client_addr, server: server_addr } = addrs;

        let fnet_ext::IpAddress(client_addr) = client_addr.into();
        let client_addr =
            std::net::SocketAddr::new(client_addr, /* let netstack pick the port */ 0);

        let fnet_ext::IpAddress(server_addr) = server_addr.into();
        let server_addr =
            std::net::SocketAddr::new(server_addr, /* let netstack pick the port */ 0);

        let server =
            fasync::net::TcpListener::listen_in_realm_with(&server, server_addr, |socket| {
                Ok(socket.set_reuse_address(true).expect("set reuse address"))
            })
            .await
            .expect("listen on server");

        let client = client
            .stream_socket(
                match client_addr {
                    std::net::SocketAddr::V4(_) => fposix_socket::Domain::Ipv4,
                    std::net::SocketAddr::V6(_) => fposix_socket::Domain::Ipv6,
                },
                fposix_socket::StreamSocketProtocol::Tcp,
            )
            .await
            .expect("create client socket");
        client.bind(&client_addr.into()).expect("bind client socket");

        let addrs = SockAddrs {
            client: client
                .local_addr()
                .expect("get local addr")
                .as_socket()
                .expect("should be inet socket"),
            server: server.local_addr().expect("get local addr"),
        };

        (Sockets { client, server: server.accept_stream() }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        _realms: Realms<'_>,
        sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let Sockets { client, mut server } = sockets;
        let SockAddrs { client: client_addr, server: server_addr } = sock_addrs;

        let server_fut = async move {
            match expected_connectivity {
                ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                    match server
                        .next()
                        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || None)
                        .await
                        .transpose()
                        .expect("accept connection")
                    {
                        Some((_stream, _addr)) => {
                            panic!("unexpectedly connected successfully")
                        }
                        None => {}
                    }
                }
                ExpectedConnectivity::TwoWay => {
                    let (mut stream, from) = server
                        .next()
                        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
                            panic!(
                                "timed out waiting for a connection after {:?}",
                                ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
                            );
                        })
                        .await
                        .expect("client should connect to server")
                        .expect("accept connection");

                    let mut buf = [0u8; 1024];
                    let bytes = stream.read(&mut buf).await.expect("read from client");
                    assert_eq!(from.ip(), client_addr.ip());
                    assert_eq!(bytes, CLIENT_PAYLOAD.as_bytes().len());
                    assert_eq!(&buf[..bytes], CLIENT_PAYLOAD.as_bytes());

                    let bytes =
                        stream.write(SERVER_PAYLOAD.as_bytes()).await.expect("write to client");
                    assert_eq!(bytes, SERVER_PAYLOAD.as_bytes().len());
                }
            }
        };

        let client_fut = async move {
            // We clone the client socket because `fasync::net::TcpStream::connect_from_raw`
            // takes an owned socket, and we only have a borrow to the socket in this
            // function because the caller may be reusing the socket across invocations.
            let client = client.try_clone().expect("clone socket");
            match expected_connectivity {
                ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                    match fasync::net::TcpStream::connect_from_raw(client, server_addr)
                        .expect("create connector from socket")
                        .map_ok(Some)
                        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
                        .await
                        .expect("connect to server")
                    {
                        Some(_stream) => panic!("unexpectedly connected successfully"),
                        None => {}
                    }
                }
                ExpectedConnectivity::TwoWay => {
                    let mut stream = fasync::net::TcpStream::connect_from_raw(client, server_addr)
                        .expect("connect to server")
                        .map(|r| r.expect("connect to server"))
                        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
                            panic!(
                                "timed out waiting for a connection after {:?}",
                                ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
                            );
                        })
                        .await;

                    let bytes =
                        stream.write(CLIENT_PAYLOAD.as_bytes()).await.expect("write to server");
                    assert_eq!(bytes, CLIENT_PAYLOAD.as_bytes().len());

                    let mut buf = [0u8; 1024];
                    let bytes = stream.read(&mut buf).await.expect("read from server");
                    assert_eq!(bytes, SERVER_PAYLOAD.as_bytes().len());
                    assert_eq!(&buf[..bytes], SERVER_PAYLOAD.as_bytes());
                }
            }
        };

        futures::future::join(server_fut, client_fut).await;
    }
}

pub(crate) struct UdpSocket;

impl SocketType for UdpSocket {
    type Client = fasync::net::UdpSocket;
    type Server = fasync::net::UdpSocket;

    async fn bind_sockets(realms: Realms<'_>, addrs: Addrs) -> (Sockets<Self>, SockAddrs) {
        let Realms { client, server } = realms;
        let Addrs { client: client_addr, server: server_addr } = addrs;

        let fnet_ext::IpAddress(client_addr) = client_addr.into();
        let client_addr =
            std::net::SocketAddr::new(client_addr, /* let netstack pick the port */ 0);

        let fnet_ext::IpAddress(server_addr) = server_addr.into();
        let server_addr =
            std::net::SocketAddr::new(server_addr, /* let netstack pick the port */ 0);

        let client_sock =
            fasync::net::UdpSocket::bind_in_realm(&client, client_addr).await.expect("bind socket");
        let server_sock =
            fasync::net::UdpSocket::bind_in_realm(&server, server_addr).await.expect("bind socket");

        let addrs = SockAddrs {
            client: client_sock.local_addr().expect("get client addr"),
            server: server_sock.local_addr().expect("get server addr"),
        };

        (Sockets { client: client_sock, server: server_sock }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        _realms: Realms<'_>,
        sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let Sockets { client, server } = sockets;
        let SockAddrs { client: client_addr, server: server_addr } = sock_addrs;

        let server_fut = async move {
            let mut buf = [0u8; 1024];
            match expected_connectivity {
                ExpectedConnectivity::None => {
                    match server
                        .recv_from(&mut buf[..])
                        .map_ok(Some)
                        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
                        .await
                        .expect("call recvfrom")
                    {
                        Some((bytes, from)) => {
                            panic!(
                                "server unexpectedly received packet {:?} from {:?}",
                                &buf[..bytes],
                                from
                            )
                        }
                        None => {}
                    }
                }
                ExpectedConnectivity::ClientToServerOnly | ExpectedConnectivity::TwoWay => {
                    let (bytes, from) =
                        server.recv_from(&mut buf[..]).await.expect("receive from client");
                    assert_eq!(bytes, CLIENT_PAYLOAD.as_bytes().len());
                    assert_eq!(&buf[..bytes], CLIENT_PAYLOAD.as_bytes());
                    assert_eq!(from, client_addr);
                    let bytes = server
                        .send_to(SERVER_PAYLOAD.as_bytes(), client_addr)
                        .await
                        .expect("reply to client");
                    assert_eq!(bytes, SERVER_PAYLOAD.as_bytes().len());
                }
            }
        };

        let client_fut = async move {
            let bytes = client
                .send_to(CLIENT_PAYLOAD.as_bytes(), server_addr)
                .await
                .expect("send to server");
            assert_eq!(bytes, CLIENT_PAYLOAD.as_bytes().len());

            let mut buf = [0u8; 1024];
            match expected_connectivity {
                ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                    match client
                        .recv_from(&mut buf[..])
                        .map_ok(Some)
                        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
                        .await
                        .expect("recvfrom failed")
                    {
                        Some((bytes, from)) => {
                            panic!(
                                "client unexpectedly received packet {:?} from {:?}",
                                &buf[..bytes],
                                from
                            )
                        }
                        None => {}
                    }
                }
                ExpectedConnectivity::TwoWay => {
                    let (bytes, from) = client
                        .recv_from(&mut buf[..])
                        .map(|result| result.expect("recvfrom failed"))
                        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
                            panic!(
                                "timed out waiting for packet from server after {:?}",
                                ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
                            );
                        })
                        .await;
                    assert_eq!(bytes, SERVER_PAYLOAD.as_bytes().len());
                    assert_eq!(&buf[..bytes], SERVER_PAYLOAD.as_bytes());
                    assert_eq!(from, server_addr);
                }
            }
        };
        futures::future::join(server_fut, client_fut).await;
    }
}

pub(crate) struct IcmpSocket;

impl SocketType for IcmpSocket {
    // We do not bind the sockets up front as with TCP and UDP. This is because
    // for those tests, the test may need to use information from the socket,
    // such as the locally bound port, to create an appropriate filter. For
    // ICMP, there is no port, and filtering is not done based on ICMP ID, so
    // there is no requirement to bind the sockets up front.
    type Client = ();
    type Server = ();

    async fn bind_sockets(_realms: Realms<'_>, addrs: Addrs) -> (Sockets<Self>, SockAddrs) {
        let Addrs { client: client_addr, server: server_addr } = addrs;

        let fnet_ext::IpAddress(client_addr) = client_addr.into();
        let client_addr = std::net::SocketAddr::new(client_addr, 0);

        let fnet_ext::IpAddress(server_addr) = server_addr.into();
        let server_addr = std::net::SocketAddr::new(server_addr, 0);

        let addrs = SockAddrs { client: client_addr, server: server_addr };

        (Sockets { client: (), server: () }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        realms: Realms<'_>,
        _sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let Realms { client, server } = realms;

        const SEQ: u16 = 1;

        async fn expect_ping_timeout<I: ping::FuchsiaIpExt>(
            realm: &netemul::TestRealm<'_>,
            addr: I::SockAddr,
        ) {
            match realm
                .ping_once::<I>(addr, SEQ)
                .map_ok(Some)
                .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
                .await
                .expect("send ping")
            {
                Some(()) => panic!("ping to {addr} unexpectedly succeeded"),
                None => {}
            }
        }

        let SockAddrsIpSpecific::<I> { client: client_addr, server: server_addr } =
            sock_addrs.into();
        match expected_connectivity {
            ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                futures::future::join(
                    expect_ping_timeout::<I>(client, server_addr),
                    expect_ping_timeout::<I>(server, client_addr),
                )
                .await;
            }
            ExpectedConnectivity::TwoWay => {
                futures::future::join(
                    client.ping_once::<I>(server_addr, SEQ).map(|r| r.expect("ping")),
                    server.ping_once::<I>(client_addr, SEQ).map(|r| r.expect("ping")),
                )
                .await;
            }
        }
    }
}

struct Realms<'a> {
    client: &'a netemul::TestRealm<'a>,
    server: &'a netemul::TestRealm<'a>,
}

struct Addrs {
    client: fnet::IpAddress,
    server: fnet::IpAddress,
}

struct Sockets<S: SocketType + ?Sized> {
    client: S::Client,
    server: S::Server,
}

#[derive(Clone, Copy)]
struct SockAddrs {
    client: std::net::SocketAddr,
    server: std::net::SocketAddr,
}

impl SockAddrs {
    fn client_ports(&self) -> Ports {
        let Self { client, server } = self;
        Ports { src: client.port(), dst: server.port() }
    }

    fn server_ports(&self) -> Ports {
        let Self { client, server } = self;
        Ports { src: server.port(), dst: client.port() }
    }
}

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
struct SockAddrsIpSpecific<I: ping::IpExt> {
    client: I::SockAddr,
    server: I::SockAddr,
}

impl<I: ping::IpExt> From<SockAddrs> for SockAddrsIpSpecific<I> {
    fn from(sock_addrs: SockAddrs) -> Self {
        I::map_ip(
            IpInvariant(sock_addrs),
            |IpInvariant(SockAddrs { client, server })| {
                let client = assert_matches!(client, std::net::SocketAddr::V4(addr) => addr);
                let server = assert_matches!(server, std::net::SocketAddr::V4(addr) => addr);
                SockAddrsIpSpecific { client, server }
            },
            |IpInvariant(SockAddrs { client, server })| {
                let client = assert_matches!(client, std::net::SocketAddr::V6(addr) => addr);
                let server = assert_matches!(server, std::net::SocketAddr::V6(addr) => addr);
                SockAddrsIpSpecific { client, server }
            },
        )
    }
}

/// Subnets expected for traffic arriving on a given interface. `src` is the
/// subnet that is expected to include the source address, `dst` is the subnet
/// that is expected to include the destination address, and `other` is expected
/// to be a third non-overlapping subnet, used for the purpose of exercising
/// inverse match functionality.
struct Subnets {
    src: fnet::Subnet,
    dst: fnet::Subnet,
    other: fnet::Subnet,
}

/// Ports expected for traffic arriving on a given interface. `src` is the
/// expected source port for incoming traffic, and `dst` is the expected
/// destination port for incoming traffic.
struct Ports {
    src: u16,
    dst: u16,
}

trait TestIpExt: ping::FuchsiaIpExt {
    /// The client netstack's IP address and subnet prefix. The client and server
    /// are on the same subnet.
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The server netstack's IP address and subnet prefix. The client and server
    /// are on the same subnet.
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// An unrelated subnet on which neither netstack has an assigned IP address;
    /// defined for the purpose of exercising inverse subnet and address range
    /// match.
    const OTHER_SUBNET: fnet::Subnet;
}

impl TestIpExt for Ipv4 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.2/24");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("192.0.3.0/24");
}

impl TestIpExt for Ipv6 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("2001:db8::1/64");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("2001:db8::2/64");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("2001:db81::/64");
}

struct TestNet<'a> {
    client: TestRealm<'a>,
    server: TestRealm<'a>,
}

impl<'a> TestNet<'a> {
    async fn new<I: TestIpExt>(
        sandbox: &'a netemul::TestSandbox,
        network: &'a netemul::TestNetwork<'a>,
        name: &str,
        hook: IpHook,
    ) -> Self {
        let client_name = format!("{name}_client");
        let client = TestRealm::new::<I>(
            &sandbox,
            network,
            hook,
            client_name,
            I::CLIENT_ADDR_WITH_PREFIX,
            I::SERVER_ADDR_WITH_PREFIX,
        )
        .await;
        let server_name = format!("{name}_server");
        let server = TestRealm::new::<I>(
            &sandbox,
            network,
            hook,
            server_name,
            I::SERVER_ADDR_WITH_PREFIX,
            I::CLIENT_ADDR_WITH_PREFIX,
        )
        .await;

        Self { client, server }
    }

    fn realms(&'a self) -> Realms<'a> {
        let Self { client, server } = self;
        Realms { client: &client.realm, server: &server.realm }
    }

    fn addrs(&self) -> Addrs {
        Addrs { client: self.client.local_subnet.addr, server: self.server.local_subnet.addr }
    }

    async fn run_test<I, S>(&mut self, expected_connectivity: ExpectedConnectivity)
    where
        I: TestIpExt,
        S: SocketType,
    {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), self.addrs()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity).await;
    }

    /// NB: in order for callers to provide a `setup` that captures its environment,
    /// we need to constrain the HRTB lifetime `'b` with `'params: 'b`, i.e.
    /// "`'params`' outlives `'b`". Since "where" clauses are unsupported for HRTB,
    /// the only way to do this is with an implied bound. The type `&'b &'params ()`
    /// is only well-formed if `'params: 'b`, so adding an argument of that type
    /// implies the bound.
    ///
    /// See https://stackoverflow.com/a/72673740 for a more thorough explanation.
    async fn run_test_with<'params, I, S, F>(
        &'params mut self,
        expected_connectivity: ExpectedConnectivity,
        setup: F,
    ) where
        I: TestIpExt,
        S: SocketType,
        F: for<'b> FnOnce(
            &'b mut TestNet<'a>,
            SockAddrs,
            &'b &'params (),
        ) -> LocalBoxFuture<'b, ()>,
    {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), self.addrs()).await;
        setup(self, sock_addrs, &&()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity).await;
    }
}

struct TestRealm<'a> {
    realm: netemul::TestRealm<'a>,
    interface: netemul::TestInterface<'a>,
    controller: Controller,
    namespace: NamespaceId,
    routine: RoutineId,
    local_subnet: fnet::Subnet,
    remote_subnet: fnet::Subnet,
}

impl<'a> TestRealm<'a> {
    async fn new<I: TestIpExt>(
        sandbox: &'a netemul::TestSandbox,
        network: &'a netemul::TestNetwork<'a>,
        hook: IpHook,
        name: String,
        local_subnet: fnet::Subnet,
        remote_subnet: fnet::Subnet,
    ) -> Self {
        let realm =
            sandbox.create_netstack_realm::<Netstack3, _>(name.clone()).expect("create realm");

        let interface = realm.join_network(&network, name.clone()).await.expect("join network");
        interface.add_address_and_subnet_route(local_subnet).await.expect("configure address");

        let control =
            realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
        let mut controller = Controller::new(&control, &ControllerId(name.clone()))
            .await
            .expect("create controller");
        let namespace = NamespaceId(name.clone());
        let routine = RoutineId { namespace: namespace.clone(), name: format!("{hook:?}") };
        controller
            .push_changes(vec![
                Change::Create(Resource::Namespace(Namespace {
                    id: namespace.clone(),
                    domain: Domain::AllIp,
                })),
                Change::Create(Resource::Routine(Routine {
                    id: routine.clone(),
                    routine_type: RoutineType::Ip(Some(InstalledIpRoutine { hook, priority: 0 })),
                })),
            ])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");

        Self { realm, interface, controller, namespace, routine, local_subnet, remote_subnet }
    }

    async fn install_rule<I: TestIpExt, M: Matcher>(
        &mut self,
        index: u32,
        matcher: &M,
        ports: Ports,
        action: Action,
    ) {
        let matcher = matcher
            .matcher::<I>(
                &self.interface,
                // We are installing a filter on the INGRESS or LOCAL_INGRESS hook, which means
                // we are dealing with incoming traffic. This means the source address of this
                // traffic will be the remote's subnet, and the destination address will be the
                // local subnet.
                Subnets { src: self.remote_subnet, dst: self.local_subnet, other: I::OTHER_SUBNET },
                ports,
            )
            .await;
        self.controller
            .push_changes(vec![Change::Create(Resource::Rule(Rule {
                id: RuleId { routine: self.routine.clone(), index },
                matchers: matcher,
                action,
            }))])
            .await
            .expect("push changes");
        self.controller.commit().await.expect("commit changes");
    }

    async fn clear_filter(&mut self) {
        self.controller
            .push_changes(vec![Change::Remove(ResourceId::Namespace(self.namespace.clone()))])
            .await
            .expect("push changes");
        self.controller.commit().await.expect("commit changes");
    }
}

const LOW_RULE_PRIORITY: u32 = 2;
const MEDIUM_RULE_PRIORITY: u32 = 1;
const HIGH_RULE_PRIORITY: u32 = 0;

async fn drop_incoming<I: net_types::ip::Ip + TestIpExt, M: Matcher>(
    name: &str,
    hook: IncomingHook,
    matcher: M,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let network = sandbox.create_network("net").await.expect("create network");
    let name = format!("{name}_{hook:?}");

    let mut net = TestNet::new::<I>(
        &sandbox,
        &network,
        &name,
        match hook {
            IncomingHook::Ingress => IpHook::Ingress,
            IncomingHook::LocalIngress => IpHook::LocalIngress,
        },
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule that explicitly accepts traffic of a certain type on the
    // ingress hook for both the client and server. This should not change the
    // two-way connectivity because accepting traffic is the default.
    net.run_test_with::<I, M::SocketType, _>(
        ExpectedConnectivity::TwoWay,
        |TestNet { client, server }, addrs, ()| {
            Box::pin(async move {
                client
                    .install_rule::<I, _>(
                        LOW_RULE_PRIORITY,
                        &matcher,
                        addrs.server_ports(),
                        Action::Accept,
                    )
                    .await;
                server
                    .install_rule::<I, _>(
                        LOW_RULE_PRIORITY,
                        &matcher,
                        addrs.client_ports(),
                        Action::Accept,
                    )
                    .await;
            })
        },
    )
    .await;

    // Prepend a rule that *drops* traffic of the same type to the ingress hook on
    // the client. This should still allow traffic to go from the client to the
    // server, but not the reverse.
    net.run_test_with::<I, M::SocketType, _>(
        ExpectedConnectivity::ClientToServerOnly,
        |TestNet { client, server: _ }, addrs, ()| {
            Box::pin(async move {
                client
                    .install_rule::<I, _>(
                        MEDIUM_RULE_PRIORITY,
                        &matcher,
                        addrs.server_ports(),
                        Action::Drop,
                    )
                    .await;
            })
        },
    )
    .await;

    // Prepend the drop rule to the ingress hook on *both* the client and server.
    // Now neither should be able to reach each other.
    net.run_test_with::<I, M::SocketType, _>(
        ExpectedConnectivity::None,
        |TestNet { client, server }, addrs, ()| {
            Box::pin(async move {
                client
                    .install_rule::<I, _>(
                        HIGH_RULE_PRIORITY,
                        &matcher,
                        addrs.server_ports(),
                        Action::Drop,
                    )
                    .await;
                server
                    .install_rule::<I, _>(
                        HIGH_RULE_PRIORITY,
                        &matcher,
                        addrs.client_ports(),
                        Action::Drop,
                    )
                    .await;
            })
        },
    )
    .await;

    // Remove all filtering rules; two-way connectivity should now be possible
    // again.
    net.client.clear_filter().await;
    net.server.clear_filter().await;
    net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;
}

generate_test_cases_for_all_matchers!(drop_incoming, IncomingHook::Ingress, ingress);
generate_test_cases_for_all_matchers!(drop_incoming, IncomingHook::LocalIngress, local_ingress);

// TODO(https://github.com/rust-lang/rustfmt/issues/5321): remove when rustfmt
// handles these supertrait bounds correctly.
#[rustfmt::skip]
trait RouterTestIpExt:
    ping::FuchsiaIpExt
    + fnet_routes_ext::FidlRouteIpExt
    + fnet_routes_ext::admin::FidlRouteAdminIpExt
{
    /// The client netstack's IP address and subnet prefix. The client is on the
    /// same subnet as the router's client-facing interface.
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The router's IP address and subnet prefix assigned on the interface that
    /// neighbors the client.
    const ROUTER_CLIENT_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The router's IP address assigned on the interface that neighbors the client.
    const ROUTER_CLIENT_ADDR: Self::Addr;
    /// The server netstack's IP address and subnet prefix. The server is on the
    /// same subnet as the router's server-facing interface.
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The router's IP address and subnet prefix assigned on the interface that
    /// neighbors the server.
    const ROUTER_SERVER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The router's IP address assigned on the interface that neighbors the server.
    const ROUTER_SERVER_ADDR: Self::Addr;
    /// An unrelated subnet on which neither netstack nor the router has an assigned
    /// IP address; defined for the purpose of exercising inverse subnet and address
    /// range match.
    const OTHER_SUBNET: fnet::Subnet;
}

impl RouterTestIpExt for Ipv4 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const ROUTER_CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.2/24");
    const ROUTER_CLIENT_ADDR: Ipv4Addr = net_ip_v4!("192.0.2.2");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("10.0.0.1/24");
    const ROUTER_SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("10.0.0.2/24");
    const ROUTER_SERVER_ADDR: Ipv4Addr = net_ip_v4!("10.0.0.2");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("8.8.8.0/24");
}

impl RouterTestIpExt for Ipv6 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("a::1/64");
    const ROUTER_CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("a::2/64");
    const ROUTER_CLIENT_ADDR: Ipv6Addr = net_ip_v6!("a::2");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("b::1/64");
    const ROUTER_SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("b::2/64");
    const ROUTER_SERVER_ADDR: Ipv6Addr = net_ip_v6!("b::2");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("c::/64");
}

struct TestRouterNet<'a, I: RouterTestIpExt> {
    // Router resources. We keep handles around to the test realm and networks
    // so that they are not torn down for the lifetime of the test.
    _router: netemul::TestRealm<'a>,
    _router_client_net: netemul::TestNetwork<'a>,
    router_client_interface: netemul::TestInterface<'a>,
    _router_server_net: netemul::TestNetwork<'a>,
    router_server_interface: netemul::TestInterface<'a>,

    // Client resources. We keep handles around to the interface and route set
    // so that they are not torn down for the lifetime of the test.
    client: netemul::TestRealm<'a>,
    _client_interface: netemul::TestInterface<'a>,
    _client_route_set: <I::RouteSetMarker as ProtocolMarker>::Proxy,

    // Server resources. We keep handles around to the interface and route set
    // so that they are not torn down for the lifetime of the test.
    server: netemul::TestRealm<'a>,
    _server_interface: netemul::TestInterface<'a>,
    _server_route_set: <I::RouteSetMarker as ProtocolMarker>::Proxy,

    // Filtering resources (for the router).
    controller: Controller,
    namespace: NamespaceId,
    routine: RoutineId,
}

impl<'a, I: RouterTestIpExt> TestRouterNet<'a, I> {
    // These just need to be unique.
    const CLIENT_FILTER_RULE_INDEX: u32 = 0;
    const SERVER_FILTER_RULE_INDEX: u32 = 1;

    async fn new(sandbox: &'a netemul::TestSandbox, name: &str, hook: IpHook) -> Self {
        let router = sandbox
            .create_netstack_realm::<Netstack3, _>(format!("{name}_router"))
            .expect("create realm");

        let client_net = sandbox.create_network("router_client").await.expect("create network");
        let router_client_interface =
            router.join_network(&client_net, "router_client").await.expect("join network");
        router_client_interface
            .add_address_and_subnet_route(I::ROUTER_CLIENT_ADDR_WITH_PREFIX)
            .await
            .expect("configure address");

        let server_net = sandbox.create_network("router_server").await.expect("create network");
        let router_server_interface =
            router.join_network(&server_net, "router_server").await.expect("join network");
        router_server_interface
            .add_address_and_subnet_route(I::ROUTER_SERVER_ADDR_WITH_PREFIX)
            .await
            .expect("configure address");

        async fn enable_forwarding(interface: &netemul::TestInterface<'_>) {
            let _prev = interface
                .control()
                .set_configuration(&fnet_interfaces_admin::Configuration {
                    ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                        forwarding: Some(true),
                        ..Default::default()
                    }),
                    ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                        forwarding: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .await
                .expect("set_configuration FIDL error")
                .expect("error setting configuration");
        }

        enable_forwarding(&router_client_interface).await;
        enable_forwarding(&router_server_interface).await;

        let add_host = |name: String, net, subnet, router_addr| async move {
            let realm =
                sandbox.create_netstack_realm::<Netstack3, _>(name.clone()).expect("create realm");
            let interface = realm.join_network(net, name).await.expect("join network");
            interface.add_address_and_subnet_route(subnet).await.expect("configure address");

            // Add the router as a default gateway.
            let set_provider = realm
                .connect_to_protocol::<I::SetProviderMarker>()
                .expect("connect to route set provider");
            let route_set =
                fnet_routes_ext::admin::new_route_set::<I>(&set_provider).expect("new route set");

            // Authenticate for this interface so we can add a route.
            let grant = interface.get_authorization().await.expect("getting grant should succeed");
            let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
            fnet_routes_ext::admin::authenticate_for_interface::<I>(&route_set, proof)
                .await
                .expect("call authenticate")
                .expect("authentication should succeed");

            let added = fnet_routes_ext::admin::add_route::<I>(
                &route_set,
                &fnet_routes_ext::Route {
                    destination: net_types::ip::Subnet::new(I::UNSPECIFIED_ADDRESS, 0)
                        .expect("unspecified subnet should be valid"),
                    action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget::<
                        I,
                    > {
                        outbound_interface: interface.id(),
                        next_hop: Some(
                            SpecifiedAddr::new(router_addr)
                                .expect("router address should be specified"),
                        ),
                    }),
                    properties: fnet_routes_ext::RouteProperties {
                        specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                            metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(
                                fnet_routes::Empty {},
                            ),
                        },
                    },
                }
                .try_into()
                .expect("convert to FIDL"),
            )
            .await
            .expect("call add route")
            .expect("add default route");
            assert!(added);

            (realm, interface, route_set)
        };

        let (client, client_interface, _client_route_set) = add_host(
            format!("{name}_client"),
            &client_net,
            I::CLIENT_ADDR_WITH_PREFIX,
            I::ROUTER_CLIENT_ADDR,
        )
        .await;
        let (server, server_interface, _server_route_set) = add_host(
            format!("{name}_server"),
            &server_net,
            I::SERVER_ADDR_WITH_PREFIX,
            I::ROUTER_SERVER_ADDR,
        )
        .await;

        let control = router
            .connect_to_protocol::<fnet_filter::ControlMarker>()
            .expect("connect to protocol");
        let mut controller = Controller::new(&control, &ControllerId(name.to_owned()))
            .await
            .expect("create controller");
        let namespace = NamespaceId(name.to_owned());
        let routine = RoutineId { namespace: namespace.clone(), name: format!("{hook:?}") };
        controller
            .push_changes(vec![
                Change::Create(Resource::Namespace(Namespace {
                    id: namespace.clone(),
                    domain: Domain::AllIp,
                })),
                Change::Create(Resource::Routine(Routine {
                    id: routine.clone(),
                    routine_type: RoutineType::Ip(Some(InstalledIpRoutine { hook, priority: 0 })),
                })),
            ])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");

        Self {
            _router: router,
            router_client_interface,
            _router_client_net: client_net,
            router_server_interface,
            _router_server_net: server_net,
            client,
            _client_interface: client_interface,
            _client_route_set,
            server,
            _server_interface: server_interface,
            _server_route_set,
            controller,
            namespace,
            routine,
        }
    }

    async fn drop_traffic_on_interface<M: Matcher>(
        controller: &mut Controller,
        rule_id: RuleId,
        matcher: &M,
        interface: &netemul::TestInterface<'_>,
        subnets: Subnets,
        ports: Ports,
    ) {
        let id = NonZeroU64::new(interface.id()).expect("interface ID should be nonzero");
        let matcher = Matchers {
            // Only filter traffic that is arriving on this particular interface.
            in_interface: Some(InterfaceMatcher::Id(id)),
            ..matcher.matcher::<I>(interface, subnets, ports).await
        };
        controller
            .push_changes(vec![Change::Create(Resource::Rule(Rule {
                id: rule_id,
                matchers: matcher,
                action: Action::Drop,
            }))])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");
    }

    async fn install_filter_for_traffic_from_server<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self { controller, router_server_interface, .. } = self;
        Self::drop_traffic_on_interface::<M>(
            controller,
            RuleId { routine: self.routine.clone(), index: Self::CLIENT_FILTER_RULE_INDEX },
            matcher,
            router_server_interface,
            Subnets {
                src: I::SERVER_ADDR_WITH_PREFIX,
                dst: I::CLIENT_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.server_ports(),
        )
        .await;
    }

    async fn install_filter_for_traffic_from_client<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self { controller, router_client_interface, .. } = self;
        Self::drop_traffic_on_interface::<M>(
            controller,
            RuleId { routine: self.routine.clone(), index: Self::SERVER_FILTER_RULE_INDEX },
            matcher,
            router_client_interface,
            Subnets {
                src: I::CLIENT_ADDR_WITH_PREFIX,
                dst: I::SERVER_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.client_ports(),
        )
        .await;
    }

    async fn clear_filter(&mut self) {
        self.controller
            .push_changes(vec![Change::Remove(ResourceId::Namespace(self.namespace.clone()))])
            .await
            .expect("push changes");
        self.controller.commit().await.expect("commit changes");
    }

    fn realms(&'a self) -> Realms<'a> {
        let Self { client, server, .. } = self;
        Realms { client, server }
    }

    fn addrs() -> Addrs {
        Addrs { client: I::CLIENT_ADDR_WITH_PREFIX.addr, server: I::SERVER_ADDR_WITH_PREFIX.addr }
    }

    async fn run_test<S: SocketType>(&mut self, expected_connectivity: ExpectedConnectivity) {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), Self::addrs()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity).await;
    }

    /// NB: in order for callers to provide a `setup` that captures its environment,
    /// we need to constrain the HRTB lifetime `'b` with `'params: 'b`, i.e.
    /// "`'params`' outlives `'b`". Since "where" clauses are unsupported for HRTB,
    /// the only way to do this is with an implied bound. The type `&'b &'params ()`
    /// is only well-formed if `'params: 'b`, so adding an argument of that type
    /// implies the bound.
    ///
    /// See https://stackoverflow.com/a/72673740 for a more thorough explanation.
    async fn run_test_with<'params, S, F>(
        &'params mut self,
        expected_connectivity: ExpectedConnectivity,
        setup: F,
    ) where
        S: SocketType,
        F: for<'b> FnOnce(
            &'b mut TestRouterNet<'a, I>,
            SockAddrs,
            &'b &'params (),
        ) -> LocalBoxFuture<'b, ()>,
    {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), Self::addrs()).await;
        setup(self, sock_addrs, &&()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity).await;
    }
}

async fn forwarded_traffic_skips_local_ingress<
    I: net_types::ip::Ip + RouterTestIpExt,
    M: Matcher,
>(
    name: &str,
    hook: IncomingHook,
    matcher: M,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let name = format!("{name}_{hook:?}");

    // Set up a network with two hosts (client and server) and a router. The client
    // and server are both link-layer neighbors with the router but on isolated L2
    // networks.
    let mut net = TestRouterNet::<I>::new(
        &sandbox,
        &name,
        match hook {
            IncomingHook::Ingress => IpHook::Ingress,
            IncomingHook::LocalIngress => IpHook::LocalIngress,
        },
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule on either the ingress or local ingress hook on the router that
    // drops traffic from the server to the client. If the rule was installed on the
    // local ingress hook, this should have no effect on connectivity because all of
    // this traffic is being forwarded. If the rule was installed on the ingress
    // hook, this should still allow traffic to go from the client to the server,
    // but not the reverse.
    net.run_test_with::<M::SocketType, _>(
        match hook {
            IncomingHook::Ingress => ExpectedConnectivity::ClientToServerOnly,
            IncomingHook::LocalIngress => ExpectedConnectivity::TwoWay,
        },
        |net, addrs, ()| {
            Box::pin(async move {
                net.install_filter_for_traffic_from_server(&matcher, addrs).await;
            })
        },
    )
    .await;

    // Install a similar rule on the same hook, but which drops traffic from the
    // client to the server. For local ingress, this should again have no effect.
    // For ingress, this should result in neither host being able to reach each
    // other.
    net.run_test_with::<M::SocketType, _>(
        match hook {
            IncomingHook::Ingress => ExpectedConnectivity::None,
            IncomingHook::LocalIngress => ExpectedConnectivity::TwoWay,
        },
        |net, addrs, ()| {
            Box::pin(async move {
                net.install_filter_for_traffic_from_client(&matcher, addrs).await;
            })
        },
    )
    .await;

    // Remove all filtering rules; two-way connectivity should now be possible
    // again.
    net.clear_filter().await;
    net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;
}

generate_test_cases_for_all_matchers!(
    forwarded_traffic_skips_local_ingress,
    IncomingHook::Ingress,
    ingress
);
generate_test_cases_for_all_matchers!(
    forwarded_traffic_skips_local_ingress,
    IncomingHook::LocalIngress,
    local_ingress
);
