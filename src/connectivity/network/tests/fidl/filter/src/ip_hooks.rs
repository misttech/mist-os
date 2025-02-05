// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the IP filtering hooks.

use std::any::Any;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU32, Ordering};

use assert_matches::assert_matches;
use fidl_fuchsia_net_filter_ext::{
    Action, Change, Controller, ControllerId, Domain, InstalledIpRoutine, InstalledNatRoutine,
    InterfaceMatcher, IpHook, Matchers, Namespace, NamespaceId, NatHook, Resource, ResourceId,
    Routine, RoutineId, RoutineType, Rule, RuleId, TransportProtocolMatcher,
};
use fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _};
use futures::future::LocalBoxFuture;
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use futures::{FutureExt as _, SinkExt as _, StreamExt as _, TryFutureExt as _};
use heck::SnakeCase as _;
use log::info;
use net_declare::fidl_subnet;
use net_types::ip::{GenericOverIp, Ip, IpVersion, IpVersionMarker, Ipv4, Ipv6};
use netemul::{RealmTcpListener as _, RealmUdpSocket as _};
use netstack_testing_common::interfaces::TestInterfaceExt as _;
use netstack_testing_common::realms::{Netstack3, TestSandboxExt as _};
use netstack_testing_common::ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT;
use netstack_testing_macros::netstack_test;
use test_case::test_case;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_ext as fnet_ext,
    fidl_fuchsia_net_filter as fnet_filter, fidl_fuchsia_net_routes_ext as fnet_routes_ext,
    fidl_fuchsia_posix_socket as fposix_socket,
};

use crate::matchers::{
    AllTraffic, DstAddressRange, DstAddressSubnet, Icmp, InterfaceDeviceClass, InterfaceId,
    InterfaceName, Inversion, Matcher, SrcAddressRange, SrcAddressSubnet, Tcp, TcpDstPort,
    TcpSrcPort, Udp, UdpDstPort, UdpSrcPort,
};

macro_rules! __generate_test_cases_for_all_matchers_inner {
    ($test:item) => {
        #[netstack_test]
        #[variant(I, Ip)]
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
        $test
    };
}

macro_rules! generate_test_cases_for_all_matchers {
    ($test:ident) => {
        paste::paste! {
            __generate_test_cases_for_all_matchers_inner! {
                async fn [<$test _>]<I: TestIpExt + RouterTestIpExt, M: Matcher>(
                    name: &str,
                    matcher: M,
                ) {
                    $test::<I, M>(name, matcher).await;
                }
            }
        }
    };
    ($test:ident, $hook:expr, $suffix:ident) => {
        paste::paste! {
            __generate_test_cases_for_all_matchers_inner! {
                async fn [<$test _ $suffix>]<I: TestIpExt + RouterTestIpExt, M: Matcher>(
                    name: &str,
                    matcher: M,
                ) {
                    $test::<I, M>(name, $hook, matcher).await;
                }
            }
        }
    };
}

/// Use a shorter timeout than [`ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT`] (which is
/// 5 seconds) because these tests rely heavily on negative checks to verify
/// that packets are dropped, and with a 5 second timeout, the test runtimes
/// become very long.
///
/// There is a risk of false negatives here, where a test that would have failed
/// if given enough time passes instead, but the risk is relatively low, so we
/// make the tradeoff for reasonable test durations.
const NEGATIVE_CHECK_TIMEOUT: fuchsia_async::MonotonicDuration =
    fuchsia_async::MonotonicDuration::from_seconds(1);

#[derive(Clone, Copy, Debug)]
pub(crate) enum ExpectedConnectivity {
    TwoWay,
    ClientToServerOnly,
    None,
}

#[derive(Clone, Copy)]
pub(crate) struct OriginalDestination {
    pub dst: std::net::SocketAddr,
    pub known_to_client: bool,
    pub known_to_server: bool,
}

pub(crate) struct BoundSockets<S: SocketType + ?Sized> {
    pub client: S::BoundClient,
    pub server: S::BoundServer,
}

pub(crate) struct ConnectedSockets<C, S> {
    pub client: C,
    pub server: S,
}

impl<C, S> ConnectedSockets<C, S> {
    pub(crate) fn as_mut(&mut self) -> ConnectedSockets<&mut C, &mut S> {
        ConnectedSockets { client: &mut self.client, server: &mut self.server }
    }
}

/// Contains some sort of handle to a socket purely for the purposes of keeping the socket open.
pub(crate) struct OpaqueSocketHandle {
    _handle: Box<dyn Any>,
}

impl OpaqueSocketHandle {
    fn new<T: Any>(handle: T) -> Self {
        Self { _handle: Box::new(handle) }
    }
}

pub(crate) struct OpaqueSocketHandles {
    _client: OpaqueSocketHandle,
    _server: OpaqueSocketHandle,
}

pub(crate) trait SocketType {
    type BoundClient;
    type BoundServer: 'static;
    type ConnectedClient: 'static;
    type ConnectedServer;

    fn matcher<I: Ip>() -> Matchers;

    async fn bind_sockets(realms: Realms<'_>, addrs: Addrs) -> (BoundSockets<Self>, SockAddrs) {
        Self::bind_sockets_to_ports(
            realms,
            addrs,
            Ports { src: 0, dst: 0 }, /* let netstack pick the ports by default */
        )
        .await
    }

    async fn bind_sockets_to_ports(
        realms: Realms<'_>,
        addrs: Addrs,
        ports: Ports,
    ) -> (BoundSockets<Self>, SockAddrs);

    async fn run_test<I: ping::FuchsiaIpExt>(
        realms: Realms<'_>,
        sockets: BoundSockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
        expected_original_dst: Option<OriginalDestination>,
    ) -> OpaqueSocketHandles {
        let BoundSockets { client, mut server } = sockets;
        let mut connected = Self::connect(
            client,
            &mut server,
            sock_addrs,
            expected_connectivity,
            expected_original_dst,
        )
        .await;
        if let Some(connected) = connected.as_mut() {
            Self::send_and_recv::<I>(realms, connected.as_mut(), sock_addrs, expected_connectivity)
                .await;
        }
        OpaqueSocketHandles {
            _client: OpaqueSocketHandle::new(
                connected.map(|ConnectedSockets { client, .. }| client),
            ),
            _server: OpaqueSocketHandle::new(server),
        }
    }

    async fn connect(
        client: Self::BoundClient,
        server: &mut Self::BoundServer,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
        expected_original_dst: Option<OriginalDestination>,
    ) -> Option<ConnectedSockets<Self::ConnectedClient, Self::ConnectedServer>>;

    async fn send_and_recv<I: ping::FuchsiaIpExt>(
        realms: Realms<'_>,
        connected: ConnectedSockets<&mut Self::ConnectedClient, &mut Self::ConnectedServer>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    );

    const SUPPORTS_NAT_PORT_RANGE: bool;
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
    type BoundClient = <UdpSocket as SocketType>::BoundClient;
    type BoundServer = <UdpSocket as SocketType>::BoundServer;
    type ConnectedClient = <UdpSocket as SocketType>::ConnectedClient;
    type ConnectedServer = <UdpSocket as SocketType>::ConnectedServer;

    fn matcher<I: Ip>() -> Matchers {
        <UdpSocket as SocketType>::matcher::<I>()
    }

    async fn bind_sockets_to_ports(
        realms: Realms<'_>,
        addrs: Addrs,
        ports: Ports,
    ) -> (BoundSockets<Self>, SockAddrs) {
        let (BoundSockets { client, server }, addrs) =
            UdpSocket::bind_sockets_to_ports(realms, addrs, ports).await;
        (BoundSockets { client, server }, addrs)
    }

    async fn connect(
        client: Self::BoundClient,
        server: &mut Self::BoundServer,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
        expected_original_dst: Option<OriginalDestination>,
    ) -> Option<ConnectedSockets<Self::ConnectedClient, Self::ConnectedServer>> {
        UdpSocket::connect(client, server, sock_addrs, expected_connectivity, expected_original_dst)
            .await
            .map(|ConnectedSockets { client, server }| ConnectedSockets { client, server })
    }

    async fn send_and_recv<I: ping::FuchsiaIpExt>(
        realms: Realms<'_>,
        connected: ConnectedSockets<&mut Self::ConnectedClient, &mut Self::ConnectedServer>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        UdpSocket::send_and_recv::<I>(
            realms,
            ConnectedSockets { client: connected.client, server: connected.server },
            sock_addrs,
            expected_connectivity,
        )
        .await;
    }

    const SUPPORTS_NAT_PORT_RANGE: bool = true;
}

#[derive(Debug)]
pub(crate) struct TcpSocket;

impl SocketType for TcpSocket {
    // NB: even though we eventually convert this to a `TcpStream` when we connect
    // it to the server, we use a `socket2::Socket` to store it at rest because
    // neither `std` nor `fuchsia_async` provide a way to bind a local socket
    // without connecting it to a remote.
    type BoundClient = socket2::Socket;
    type BoundServer = fasync::net::AcceptStream;
    type ConnectedClient = fasync::net::TcpStream;
    type ConnectedServer = fasync::net::TcpStream;

    fn matcher<I: Ip>() -> Matchers {
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: None,
                dst_port: None,
            }),
            ..Default::default()
        }
    }

    async fn bind_sockets_to_ports(
        realms: Realms<'_>,
        addrs: Addrs,
        ports: Ports,
    ) -> (BoundSockets<Self>, SockAddrs) {
        let Realms { client, server } = realms;
        let Addrs { client: client_addr, server: server_addr } = addrs;
        let Ports { src: client_port, dst: server_port } = ports;

        let fnet_ext::IpAddress(client_addr) = client_addr.into();
        let client_addr = std::net::SocketAddr::new(client_addr, client_port);

        let fnet_ext::IpAddress(server_addr) = server_addr.into();
        let server_addr = std::net::SocketAddr::new(server_addr, server_port);

        let server = fasync::net::TcpListener::listen_in_realm(&server, server_addr)
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

        (BoundSockets { client, server: server.accept_stream() }, addrs)
    }

    async fn connect(
        client: Self::BoundClient,
        server: &mut Self::BoundServer,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
        expected_original_dst: Option<OriginalDestination>,
    ) -> Option<ConnectedSockets<Self::ConnectedClient, Self::ConnectedServer>> {
        let SockAddrs { client: client_addr, server: server_addr } = sock_addrs;

        info!(
            "running {Self:?} test client={client_addr} server={server_addr} \
            expected={expected_connectivity:?}",
        );

        let server_fut = async move {
            match expected_connectivity {
                ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                    match server
                        .next()
                        .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || None)
                        .await
                        .transpose()
                        .expect("accept connection")
                    {
                        Some((_stream, _addr)) => {
                            panic!("unexpectedly connected successfully")
                        }
                        None => None,
                    }
                }
                ExpectedConnectivity::TwoWay => {
                    let (stream, from) = server
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

                    assert_eq!(from.ip(), client_addr.ip());
                    if client_addr.port() != 0 {
                        assert_eq!(from.port(), client_addr.port());
                    }

                    if let Some(OriginalDestination {
                        dst: expected,
                        known_to_server,
                        known_to_client: _,
                    }) = expected_original_dst
                    {
                        if !known_to_server {
                            return Some(stream);
                        }
                        if expected.is_ipv4() {
                            let original_dst = socket2::Socket::from(
                                stream.std().try_clone().expect("clone socket"),
                            )
                            .original_dst()
                            .expect("get original destination of connection");
                            assert_eq!(original_dst, expected.into());
                        } else {
                            // TODO(https://fxbug.dev/345465222): implement SOL_IPV6 ->
                            // IP6T_SO_ORIGINAL_DST on Fuchsia.
                        };
                    }

                    Some(stream)
                }
            }
        };

        let client_fut = async move {
            match expected_connectivity {
                ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                    match fasync::net::TcpStream::connect_from_raw(client, server_addr)
                        .expect("create connector from socket")
                        .map_ok(Some)
                        .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
                        .await
                        .expect("connect to server")
                    {
                        Some(_stream) => panic!("unexpectedly connected successfully"),
                        None => None,
                    }
                }
                ExpectedConnectivity::TwoWay => {
                    let stream = fasync::net::TcpStream::connect_from_raw(client, server_addr)
                        .expect("connect to server")
                        .map(|r| r.expect("connect to server"))
                        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
                            panic!(
                                "timed out waiting for a connection after {:?}",
                                ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
                            );
                        })
                        .await;

                    if let Some(OriginalDestination {
                        dst: expected,
                        known_to_client,
                        known_to_server: _,
                    }) = expected_original_dst
                    {
                        if !known_to_client {
                            return Some(stream);
                        }
                        if expected.is_ipv4() {
                            let original_dst = socket2::Socket::from(
                                stream.std().try_clone().expect("clone socket"),
                            )
                            .original_dst()
                            .expect("get original destination of connection");
                            assert_eq!(original_dst, expected.into());
                        } else {
                            // TODO(https://fxbug.dev/345465222): implement SOL_IPV6 ->
                            // IP6T_SO_ORIGINAL_DST on Fuchsia.
                        };
                    }
                    Some(stream)
                }
            }
        };

        let (client, server) = futures::future::join(client_fut, server_fut).await;
        match expected_connectivity {
            ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                assert_matches!((client, server), (None, None));
                None
            }
            ExpectedConnectivity::TwoWay => {
                let client = client.expect("client should have connected");
                let server = server.expect("server should have accepted connection");
                Some(ConnectedSockets { client, server })
            }
        }
    }

    async fn send_and_recv<I: ping::FuchsiaIpExt>(
        _realms: Realms<'_>,
        sockets: ConnectedSockets<&mut Self::ConnectedClient, &mut Self::ConnectedServer>,
        // NB: socket addresses are not needed because the TCP sockets are already
        // connected.
        _sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        match expected_connectivity {
            ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                panic!("sockets are already connected")
            }
            ExpectedConnectivity::TwoWay => {}
        }

        let ConnectedSockets { client, server } = sockets;

        let server_fut = async move {
            let mut buf = [0u8; 1024];
            let bytes = server.read(&mut buf).await.expect("read from client");
            assert_eq!(bytes, CLIENT_PAYLOAD.as_bytes().len());
            assert_eq!(&buf[..bytes], CLIENT_PAYLOAD.as_bytes());

            let bytes = server.write(SERVER_PAYLOAD.as_bytes()).await.expect("write to client");
            assert_eq!(bytes, SERVER_PAYLOAD.as_bytes().len());
        };

        let client_fut = async move {
            let bytes = client.write(CLIENT_PAYLOAD.as_bytes()).await.expect("write to server");
            assert_eq!(bytes, CLIENT_PAYLOAD.as_bytes().len());

            let mut buf = [0u8; 1024];
            let bytes = client.read(&mut buf).await.expect("read from server");
            assert_eq!(bytes, SERVER_PAYLOAD.as_bytes().len());
            assert_eq!(&buf[..bytes], SERVER_PAYLOAD.as_bytes());
        };

        futures::future::join(server_fut, client_fut).await;
    }

    const SUPPORTS_NAT_PORT_RANGE: bool = true;
}

#[derive(Debug)]
pub(crate) struct UdpSocket;

impl SocketType for UdpSocket {
    type BoundClient = fasync::net::UdpSocket;
    type BoundServer = fasync::net::UdpSocket;
    type ConnectedClient = fasync::net::UdpSocket;
    type ConnectedServer = fasync::net::UdpSocket;

    fn matcher<I: Ip>() -> Matchers {
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Udp {
                src_port: None,
                dst_port: None,
            }),
            ..Default::default()
        }
    }

    async fn bind_sockets_to_ports(
        realms: Realms<'_>,
        addrs: Addrs,
        ports: Ports,
    ) -> (BoundSockets<Self>, SockAddrs) {
        let Realms { client, server } = realms;
        let Addrs { client: client_addr, server: server_addr } = addrs;
        let Ports { src: client_port, dst: server_port } = ports;

        let fnet_ext::IpAddress(client_addr) = client_addr.into();
        let client_addr = std::net::SocketAddr::new(client_addr, client_port);

        let fnet_ext::IpAddress(server_addr) = server_addr.into();
        let server_addr = std::net::SocketAddr::new(server_addr, server_port);

        let client_sock =
            fasync::net::UdpSocket::bind_in_realm(&client, client_addr).await.expect("bind socket");
        let server_sock =
            fasync::net::UdpSocket::bind_in_realm(&server, server_addr).await.expect("bind socket");

        let addrs = SockAddrs {
            client: client_sock.local_addr().expect("get client addr"),
            server: server_sock.local_addr().expect("get server addr"),
        };

        (BoundSockets { client: client_sock, server: server_sock }, addrs)
    }

    async fn connect(
        client: Self::BoundClient,
        server: &mut Self::BoundServer,
        _sock_addrs: SockAddrs,
        _expected_connectivity: ExpectedConnectivity,
        // NB: SO_ORIGINAL_DST is not supported for UDP sockets.
        _expected_original_dst: Option<OriginalDestination>,
    ) -> Option<ConnectedSockets<Self::ConnectedClient, Self::ConnectedServer>> {
        // Clone the underlying server socket so the lifetime of the resulting
        // [`ConnectedSockets`] is not tied to the [`Self::BoundServer`] socket.
        let server = fasync::net::UdpSocket::from_socket(
            server.as_ref().try_clone().expect("clone socket").into(),
        )
        .expect("into std socket");
        Some(ConnectedSockets { client, server })
    }

    async fn send_and_recv<I: ping::FuchsiaIpExt>(
        _realms: Realms<'_>,
        sockets: ConnectedSockets<&mut Self::ConnectedClient, &mut Self::ConnectedServer>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let ConnectedSockets { client, server } = sockets;
        let SockAddrs { client: client_addr, server: server_addr } = sock_addrs;

        info!(
            "running {Self:?} test client={client_addr} server={server_addr} \
            expected={expected_connectivity:?}",
        );

        let server_fut = async move {
            let mut buf = [0u8; 1024];
            match expected_connectivity {
                ExpectedConnectivity::None => {
                    match server
                        .recv_from(&mut buf[..])
                        .map_ok(Some)
                        .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
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
                    assert_eq!(from.ip(), client_addr.ip());
                    if client_addr.port() != 0 {
                        assert_eq!(from.port(), client_addr.port());
                    }
                    let bytes = server
                        .send_to(SERVER_PAYLOAD.as_bytes(), from)
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
                        .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
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

    const SUPPORTS_NAT_PORT_RANGE: bool = true;
}

pub(crate) struct IcmpSocket;

pub(crate) struct IcmpSocketAndSeq {
    socket: fasync::net::DatagramSocket,
    // Keep track of used sequence numbers to avoid sending messages with the
    // same sequence.
    seq: u16,
}

impl IcmpSocketAndSeq {
    async fn new_bound(realm: &netemul::TestRealm<'_>, addr: std::net::SocketAddr) -> Self {
        let socket = match addr {
            std::net::SocketAddr::V4(_) => realm.icmp_socket::<Ipv4>().await,
            std::net::SocketAddr::V6(_) => realm.icmp_socket::<Ipv6>().await,
        }
        .expect("create icmp socket");
        socket.as_ref().bind(&addr.into()).expect("bind icmp socket");
        Self { socket, seq: 0 }
    }
}

impl SocketType for IcmpSocket {
    type BoundClient = IcmpSocketAndSeq;
    type BoundServer = ();
    type ConnectedClient = IcmpSocketAndSeq;
    type ConnectedServer = ();

    fn matcher<I: Ip>() -> Matchers {
        Matchers {
            transport_protocol: Some(match I::VERSION {
                IpVersion::V4 => TransportProtocolMatcher::Icmp,
                IpVersion::V6 => TransportProtocolMatcher::Icmpv6,
            }),
            ..Default::default()
        }
    }

    async fn bind_sockets_to_ports(
        realms: Realms<'_>,
        addrs: Addrs,
        ports: Ports,
    ) -> (BoundSockets<Self>, SockAddrs) {
        let Addrs { client: client_addr, server: server_addr } = addrs;
        let Ports { src: client_port, dst: server_port } = ports;

        let fnet_ext::IpAddress(client_addr) = client_addr.into();
        let client_addr = std::net::SocketAddr::new(client_addr, client_port);

        let fnet_ext::IpAddress(server_addr) = server_addr.into();
        let server_addr = std::net::SocketAddr::new(server_addr, server_port);

        let client = IcmpSocketAndSeq::new_bound(&realms.client, client_addr).await;

        let addrs = SockAddrs {
            client: client
                .socket
                .local_addr()
                .expect("get client addr")
                .as_socket()
                .expect("socket addr"),
            server: server_addr,
        };

        (BoundSockets { client, server: () }, addrs)
    }

    async fn connect(
        client: Self::BoundClient,
        _server: &mut Self::BoundServer,
        _sock_addrs: SockAddrs,
        _expected_connectivity: ExpectedConnectivity,
        _expected_original_dst: Option<OriginalDestination>,
    ) -> Option<ConnectedSockets<Self::ConnectedClient, Self::ConnectedServer>> {
        Some(ConnectedSockets { client, server: () })
    }

    async fn send_and_recv<I: ping::FuchsiaIpExt>(
        _realms: Realms<'_>,
        connected: ConnectedSockets<&mut Self::ConnectedClient, &mut Self::ConnectedServer>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let ConnectedSockets { client, server: _ } = connected;

        async fn ping_once<I: ping::IpExt>(socket: &mut IcmpSocketAndSeq, addr: I::SockAddr) {
            let seq = socket.seq;
            socket.seq += 1;

            const MESSAGE: &'static str = "hello, world";
            let (mut sink, stream) = ping::new_unicast_sink_and_stream::<
                I,
                _,
                { MESSAGE.len() + ping::ICMP_HEADER_LEN },
            >(&socket.socket, &addr, MESSAGE.as_bytes());

            sink.send(seq).await.expect("send ping");
            stream
                .filter_map(|r| {
                    let got = r.expect("ping error");
                    // Ignore any old replies not matching our SEQ number.
                    futures::future::ready((got == seq).then_some(()))
                })
                .next()
                .await
                .expect("ping stream ended unexpectedly");
        }

        async fn expect_ping_timeout<I: ping::IpExt>(
            socket: &mut IcmpSocketAndSeq,
            addr: I::SockAddr,
        ) {
            ping_once::<I>(socket, addr)
                .map(|()| panic!("pinged successfully unexpectedly"))
                .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || ())
                .await;
        }

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct SockAddrSpecific<I: ping::IpExt>(I::SockAddr);

        let mut server_addr = sock_addrs.server;
        server_addr.set_port(0);
        let SockAddrSpecific(server_addr) = I::map_ip_out(
            server_addr,
            |a| SockAddrSpecific(assert_matches!(a, std::net::SocketAddr::V4(addr) => addr)),
            |a| SockAddrSpecific(assert_matches!(a, std::net::SocketAddr::V6(addr) => addr)),
        );

        match expected_connectivity {
            ExpectedConnectivity::None | ExpectedConnectivity::ClientToServerOnly => {
                expect_ping_timeout::<I>(client, server_addr).await;
            }
            ExpectedConnectivity::TwoWay => {
                ping_once::<I>(client, server_addr).await;
            }
        }
    }

    const SUPPORTS_NAT_PORT_RANGE: bool = false;
}

#[derive(Clone, Copy)]
pub(crate) struct Realms<'a> {
    pub client: &'a netemul::TestRealm<'a>,
    pub server: &'a netemul::TestRealm<'a>,
}

pub(crate) struct Addrs {
    pub client: fnet::IpAddress,
    pub server: fnet::IpAddress,
}

#[derive(Clone, Copy)]
pub(crate) struct SockAddrs {
    pub client: std::net::SocketAddr,
    pub server: std::net::SocketAddr,
}

impl SockAddrs {
    pub(crate) fn client_ports(&self) -> Ports {
        let Self { client, server } = self;
        Ports { src: client.port(), dst: server.port() }
    }

    pub(crate) fn server_ports(&self) -> Ports {
        let Self { client, server } = self;
        Ports { src: server.port(), dst: client.port() }
    }
}

/// Interfaces traffic is expected to traverse at a given IP filtering hook.
#[derive(Clone, Copy)]
pub(crate) struct Interfaces<'a> {
    pub ingress: Option<&'a netemul::TestInterface<'a>>,
    pub egress: Option<&'a netemul::TestInterface<'a>>,
}

/// Subnets expected for traffic arriving on a given interface. `src` is the
/// subnet that is expected to include the source address, `dst` is the subnet
/// that is expected to include the destination address, and `other` is expected
/// to be a third non-overlapping subnet, used for the purpose of exercising
/// inverse match functionality.
#[derive(Clone, Copy)]
pub(crate) struct Subnets {
    pub src: fnet::Subnet,
    pub dst: fnet::Subnet,
    pub other: fnet::Subnet,
}

/// Ports expected for traffic arriving on a given interface. `src` is the
/// expected source port for incoming traffic, and `dst` is the expected
/// destination port for incoming traffic.
pub(crate) struct Ports {
    pub src: u16,
    pub dst: u16,
}

pub(crate) trait TestIpExt: ping::FuchsiaIpExt {
    /// The client netstack's IP address and subnet prefix. The client and server
    /// are on the same subnet.
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The server netstack's IP address and subnet prefix. The client and server
    /// are on the same subnet.
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// Another IP address in the same subnet as the client and server.
    const OTHER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// An unrelated subnet on which neither netstack has an assigned IP address;
    /// defined for the purpose of exercising inverse subnet and address range
    /// match.
    const OTHER_SUBNET: fnet::Subnet;
}

impl TestIpExt for Ipv4 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.2/24");
    const OTHER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.3/24");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("192.0.3.0/24");
}

impl TestIpExt for Ipv6 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("2001:db8::1/64");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("2001:db8::2/64");
    const OTHER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("2001:db8::3/64");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("2001:db81::/64");
}

pub(crate) struct TestNet<'a> {
    pub client: TestRealm<'a>,
    pub server: TestRealm<'a>,
}

impl<'a> TestNet<'a> {
    pub(crate) async fn new<I: TestIpExt>(
        sandbox: &'a netemul::TestSandbox,
        network: &'a netemul::TestNetwork<'a>,
        name: &str,
        ip_hook: Option<IpHook>,
        nat_hook: Option<NatHook>,
    ) -> Self {
        let client_name = format!("{name}_client");
        let client = TestRealm::new::<I>(
            &sandbox,
            network,
            ip_hook,
            nat_hook,
            client_name,
            I::CLIENT_ADDR_WITH_PREFIX,
            I::SERVER_ADDR_WITH_PREFIX,
        )
        .await;
        let server_name = format!("{name}_server");
        let server = TestRealm::new::<I>(
            &sandbox,
            network,
            ip_hook,
            nat_hook,
            server_name,
            I::SERVER_ADDR_WITH_PREFIX,
            I::CLIENT_ADDR_WITH_PREFIX,
        )
        .await;

        Self { client, server }
    }

    pub(crate) fn realms(&'a self) -> Realms<'a> {
        let Self { client, server } = self;
        Realms { client: &client.realm, server: &server.realm }
    }

    pub(crate) fn addrs(&self) -> Addrs {
        Addrs { client: self.client.local_subnet.addr, server: self.server.local_subnet.addr }
    }

    /// Returns `OpaqueSocketHandles` so that a previous test case's sockets can be kept alive so
    /// that subsequent bind calls don't collide with previous cases' ports.
    pub(crate) async fn run_test<I, S>(
        &mut self,
        expected_connectivity: ExpectedConnectivity,
    ) -> OpaqueSocketHandles
    where
        I: TestIpExt,
        S: SocketType,
    {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), self.addrs()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity, None).await
    }

    /// NB: in order for callers to provide a `setup` that captures its environment,
    /// we need to constrain the HRTB lifetime `'b` with `'params: 'b`, i.e.
    /// "`'params`' outlives `'b`". Since "where" clauses are unsupported for HRTB,
    /// the only way to do this is with an implied bound. The type `&'b &'params ()`
    /// is only well-formed if `'params: 'b`, so adding an argument of that type
    /// implies the bound.
    ///
    /// See https://stackoverflow.com/a/72673740 for a more thorough explanation.
    ///
    /// Returns `OpaqueSocketHandles` so that a previous test case's sockets can be kept alive so
    /// that subsequent bind calls don't collide with previous cases' ports.
    pub(crate) async fn run_test_with<'params, I, S, F>(
        &'params mut self,
        expected_connectivity: ExpectedConnectivity,
        setup: F,
    ) -> OpaqueSocketHandles
    where
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
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity, None).await
    }
}

pub(crate) struct TestRealm<'a> {
    pub realm: netemul::TestRealm<'a>,
    pub interface: netemul::TestInterface<'a>,
    pub controller: Controller,
    namespace: NamespaceId,
    ip_routine: Option<RoutineId>,
    pub nat_routine: Option<RoutineId>,
    pub local_subnet: fnet::Subnet,
    remote_subnet: fnet::Subnet,
}

impl<'a> TestRealm<'a> {
    pub async fn new<I: TestIpExt>(
        sandbox: &'a netemul::TestSandbox,
        network: &'a netemul::TestNetwork<'a>,
        ip_hook: Option<IpHook>,
        nat_hook: Option<NatHook>,
        name: String,
        local_subnet: fnet::Subnet,
        remote_subnet: fnet::Subnet,
    ) -> Self {
        let realm =
            sandbox.create_netstack_realm::<Netstack3, _>(name.clone()).expect("create realm");

        let interface = realm.join_network(&network, name.clone()).await.expect("join network");
        interface.add_address_and_subnet_route(local_subnet).await.expect("configure address");
        interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

        let control =
            realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
        let mut controller = Controller::new(&control, &ControllerId(name.clone()))
            .await
            .expect("create controller");
        let namespace = NamespaceId(name.clone());
        let ip_routine = ip_hook.map(|hook| {
            (hook, RoutineId { namespace: namespace.clone(), name: format!("{hook:?}") })
        });
        let nat_routine = nat_hook.map(|hook| {
            (hook, RoutineId { namespace: namespace.clone(), name: format!("{hook:?}") })
        });
        controller
            .push_changes(
                [Change::Create(Resource::Namespace(Namespace {
                    id: namespace.clone(),
                    domain: Domain::AllIp,
                }))]
                .into_iter()
                .chain(ip_routine.clone().map(|(hook, routine)| {
                    Change::Create(Resource::Routine(Routine {
                        id: routine.clone(),
                        routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                            hook,
                            priority: 0,
                        })),
                    }))
                }))
                .chain(nat_routine.clone().map(|(hook, routine)| {
                    Change::Create(Resource::Routine(Routine {
                        id: routine.clone(),
                        routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                            hook,
                            priority: 0,
                        })),
                    }))
                }))
                .collect(),
            )
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");

        Self {
            realm,
            interface,
            controller,
            namespace,
            ip_routine: ip_routine.map(|(_hook, routine)| routine),
            nat_routine: nat_routine.map(|(_hook, routine)| routine),
            local_subnet,
            remote_subnet,
        }
    }

    pub(crate) async fn install_rule<I: TestIpExt, M: Matcher>(
        controller: &mut Controller,
        rule_id: RuleId,
        matcher: &M,
        interfaces: Interfaces<'_>,
        subnets: Subnets,
        ports: Ports,
        action: Action,
    ) {
        let matcher = matcher.matcher::<I>(interfaces, subnets, ports).await;
        controller
            .push_changes(vec![Change::Create(Resource::Rule(Rule {
                id: rule_id,
                matchers: matcher,
                action,
            }))])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");
    }

    async fn install_rule_for_incoming_traffic<I: TestIpExt, M: Matcher>(
        &mut self,
        index: u32,
        matcher: &M,
        ports: Ports,
        action: Action,
    ) {
        let Self { controller, ip_routine, interface, local_subnet, remote_subnet, .. } = self;
        Self::install_rule::<I, M>(
            controller,
            RuleId { routine: ip_routine.clone().expect("IP routine should be installed"), index },
            matcher,
            Interfaces { ingress: Some(&interface), egress: None },
            // We are installing a filter on the INGRESS or LOCAL_INGRESS hook, which
            // means we are dealing with incoming traffic. This means the source address
            // of this traffic will be the remote's subnet, and the destination address
            // will be the local subnet.
            Subnets { src: *remote_subnet, dst: *local_subnet, other: I::OTHER_SUBNET },
            ports,
            action,
        )
        .await;
    }

    async fn install_rule_for_outgoing_traffic<I: TestIpExt, M: Matcher>(
        &mut self,
        index: u32,
        matcher: &M,
        ports: Ports,
        action: Action,
    ) {
        let Self { controller, ip_routine, interface, local_subnet, remote_subnet, .. } = self;
        Self::install_rule::<I, M>(
            controller,
            RuleId { routine: ip_routine.clone().expect("IP routine should be installed"), index },
            matcher,
            Interfaces { ingress: None, egress: Some(&interface) },
            // We are installing a filter on the EGRESS or LOCAL_EGRESS hook, which means we
            // are dealing with outgoing traffic. This means the source address of this
            // traffic will be the local subnet, and the destination address will be the
            // remote's subnet.
            Subnets { src: *local_subnet, dst: *remote_subnet, other: I::OTHER_SUBNET },
            ports,
            action,
        )
        .await;
    }

    pub(crate) async fn install_nat_rule<I: TestIpExt>(
        &mut self,
        index: u32,
        matchers: Matchers,
        action: Action,
    ) {
        let Self { controller, nat_routine, .. } = self;
        controller
            .push_changes(vec![Change::Create(Resource::Rule(Rule {
                id: RuleId {
                    routine: nat_routine.clone().expect("NAT routine should be installed"),
                    index,
                },
                matchers,
                action,
            }))])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");
    }

    pub(crate) async fn clear_filter(&mut self) {
        self.controller
            .push_changes(vec![Change::Remove(ResourceId::Namespace(self.namespace.clone()))])
            .await
            .expect("push changes");
        self.controller.commit().await.expect("commit changes");
    }
}

pub(crate) const LOW_RULE_PRIORITY: u32 = 2;
pub(crate) const MEDIUM_RULE_PRIORITY: u32 = 1;
pub(crate) const HIGH_RULE_PRIORITY: u32 = 0;

#[derive(Debug)]
enum IncomingHook {
    Ingress,
    LocalIngress,
}

async fn drop_incoming<I: TestIpExt, M: Matcher>(name: &str, hook: IncomingHook, matcher: M) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let network = sandbox.create_network("net").await.expect("create network");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    let mut net = TestNet::new::<I>(
        &sandbox,
        &network,
        &name,
        Some(match hook {
            IncomingHook::Ingress => IpHook::Ingress,
            IncomingHook::LocalIngress => IpHook::LocalIngress,
        }),
        None, /* nat_hook */
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    let _handles = net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule that explicitly accepts traffic of a certain type on the
    // incoming hook for both the client and server. This should not change the
    // two-way connectivity because accepting traffic is the default.
    let _handles = net
        .run_test_with::<I, M::SocketType, _>(
            ExpectedConnectivity::TwoWay,
            |TestNet { client, server }, addrs, ()| {
                Box::pin(async move {
                    client
                        .install_rule_for_incoming_traffic::<I, _>(
                            LOW_RULE_PRIORITY,
                            &matcher,
                            addrs.server_ports(),
                            Action::Accept,
                        )
                        .await;
                    server
                        .install_rule_for_incoming_traffic::<I, _>(
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

    // Prepend a rule that *drops* traffic of the same type to the incoming hook on
    // the client. This should still allow traffic to go from the client to the
    // server, but not the reverse.
    let _handles = net
        .run_test_with::<I, M::SocketType, _>(
            ExpectedConnectivity::ClientToServerOnly,
            |TestNet { client, server: _ }, addrs, ()| {
                Box::pin(async move {
                    client
                        .install_rule_for_incoming_traffic::<I, _>(
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

    // Prepend the drop rule to the incoming hook on *both* the client and server.
    // Now neither should be able to reach each other.
    let _handles = net
        .run_test_with::<I, M::SocketType, _>(
            ExpectedConnectivity::None,
            |TestNet { client, server }, addrs, ()| {
                Box::pin(async move {
                    client
                        .install_rule_for_incoming_traffic::<I, _>(
                            HIGH_RULE_PRIORITY,
                            &matcher,
                            addrs.server_ports(),
                            Action::Drop,
                        )
                        .await;
                    server
                        .install_rule_for_incoming_traffic::<I, _>(
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
    let _handles = net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;
}

generate_test_cases_for_all_matchers!(drop_incoming, IncomingHook::Ingress, ingress);
generate_test_cases_for_all_matchers!(drop_incoming, IncomingHook::LocalIngress, local_ingress);

enum OutgoingHook {
    LocalEgress,
    Egress,
}

async fn drop_outgoing<I: TestIpExt, M: Matcher>(name: &str, hook: OutgoingHook, matcher: M) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let network = sandbox.create_network("net").await.expect("create network");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    let mut net = TestNet::new::<I>(
        &sandbox,
        &network,
        &name,
        Some(match hook {
            OutgoingHook::LocalEgress => IpHook::LocalEgress,
            OutgoingHook::Egress => IpHook::Egress,
        }),
        None, /* nat_hook */
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    let _handles = net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule that explicitly accepts traffic of a certain type on the local
    // egress hook for both the client and server. This should not change the
    // two-way connectivity because accepting traffic is the default.
    let _handles = net
        .run_test_with::<I, M::SocketType, _>(
            ExpectedConnectivity::TwoWay,
            |TestNet { client, server }, addrs, ()| {
                Box::pin(async move {
                    client
                        .install_rule_for_outgoing_traffic::<I, _>(
                            LOW_RULE_PRIORITY,
                            &matcher,
                            addrs.client_ports(),
                            Action::Accept,
                        )
                        .await;
                    server
                        .install_rule_for_outgoing_traffic::<I, _>(
                            LOW_RULE_PRIORITY,
                            &matcher,
                            addrs.server_ports(),
                            Action::Accept,
                        )
                        .await;
                })
            },
        )
        .await;

    // Prepend a rule that *drops* traffic of the same type to the local egress hook
    // on the server. This should still allow traffic to go from the client to the
    // server, but not the reverse.
    let _handles = net
        .run_test_with::<I, M::SocketType, _>(
            ExpectedConnectivity::ClientToServerOnly,
            |TestNet { client: _, server }, addrs, ()| {
                Box::pin(async move {
                    server
                        .install_rule_for_outgoing_traffic::<I, _>(
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

    // Prepend the drop rule to the local egress hook on *both* the client and
    // server. Now neither should be able to reach each other.
    let _handles = net
        .run_test_with::<I, M::SocketType, _>(
            ExpectedConnectivity::None,
            |TestNet { client, server }, addrs, ()| {
                Box::pin(async move {
                    client
                        .install_rule_for_outgoing_traffic::<I, _>(
                            HIGH_RULE_PRIORITY,
                            &matcher,
                            addrs.client_ports(),
                            Action::Drop,
                        )
                        .await;
                    server
                        .install_rule_for_outgoing_traffic::<I, _>(
                            HIGH_RULE_PRIORITY,
                            &matcher,
                            addrs.server_ports(),
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
    let _handles = net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;
}

generate_test_cases_for_all_matchers!(drop_outgoing, OutgoingHook::LocalEgress, local_egress);
generate_test_cases_for_all_matchers!(drop_outgoing, OutgoingHook::Egress, egress);

// TODO(https://github.com/rust-lang/rustfmt/issues/5321): remove when rustfmt
// handles these supertrait bounds correctly.
#[rustfmt::skip]
pub(crate) trait RouterTestIpExt:
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
    /// The subnet shared by the client and router.
    const ROUTER_CLIENT_SUBNET: fnet::Subnet;
    /// The server netstack's IP address and subnet prefix. The server is on the
    /// same subnet as the router's server-facing interface.
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// The router's IP address and subnet prefix assigned on the interface that
    /// neighbors the server.
    const ROUTER_SERVER_ADDR_WITH_PREFIX: fnet::Subnet;
    /// An unrelated subnet on which neither netstack nor the router has an assigned
    /// IP address; defined for the purpose of exercising inverse subnet and address
    /// range match.
    const OTHER_SUBNET: fnet::Subnet;
}

impl RouterTestIpExt for Ipv4 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const ROUTER_CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("192.0.2.2/24");
    const ROUTER_CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.0/24");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("10.0.0.1/24");
    const ROUTER_SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("10.0.0.2/24");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("8.8.8.0/24");
}

impl RouterTestIpExt for Ipv6 {
    const CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("a::1/64");
    const ROUTER_CLIENT_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("a::2/64");
    const ROUTER_CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("a::/64");
    const SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("b::1/64");
    const ROUTER_SERVER_ADDR_WITH_PREFIX: fnet::Subnet = fidl_subnet!("b::2/64");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("c::/64");
}

pub(crate) struct TestRouterNet<'a, I: RouterTestIpExt> {
    // Router resources. We keep handles around to the test realm and networks
    // so that they are not torn down for the lifetime of the test.
    pub router: netemul::TestRealm<'a>,
    pub _router_client_net: netemul::TestNetwork<'a>,
    router_client_interface: netemul::TestInterface<'a>,
    pub _router_server_net: netemul::TestNetwork<'a>,
    pub router_server_interface: netemul::TestInterface<'a>,

    // Client resources. We keep the handle to the interface around so that it is
    // not torn down for the lifetime of the test.
    client: netemul::TestRealm<'a>,
    _client_interface: netemul::TestInterface<'a>,

    // Server resources. We keep the handle to the interface around so that it is
    // not torn down for the lifetime of the test.
    pub server: netemul::TestRealm<'a>,
    _server_interface: netemul::TestInterface<'a>,

    // Filtering resources (for the router).
    controller: Controller,
    namespace: NamespaceId,
    ip_routine: Option<RoutineId>,
    nat_routine: Option<RoutineId>,

    _ip_version: IpVersionMarker<I>,
}

impl<'a, I: RouterTestIpExt> TestRouterNet<'a, I> {
    // These two just need to be unique since they may be installed on the same routine.
    const CLIENT_FILTER_RULE_INDEX: u32 = 0;
    const SERVER_FILTER_RULE_INDEX: u32 = 1;

    const NAT_RULE_INDEX: u32 = 0;

    pub async fn new(
        sandbox: &'a netemul::TestSandbox,
        name: &str,
        ip_hook: Option<IpHook>,
        nat_hook: Option<NatHook>,
    ) -> Self {
        let router = sandbox
            .create_netstack_realm::<Netstack3, _>(format!("{name}_router"))
            .expect("create realm");

        let client_net = sandbox.create_network("router_client").await.expect("create network");
        let router_client_interface =
            router.join_network(&client_net, "router_client").await.expect("join network");
        router_client_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");
        router_client_interface
            .add_address_and_subnet_route(I::ROUTER_CLIENT_ADDR_WITH_PREFIX)
            .await
            .expect("configure address");
        router_client_interface.set_ipv4_forwarding_enabled(true).await.expect("enable forwarding");
        router_client_interface.set_ipv6_forwarding_enabled(true).await.expect("enable forwarding");

        let server_net = sandbox.create_network("router_server").await.expect("create network");
        let router_server_interface =
            router.join_network(&server_net, "router_server").await.expect("join network");
        router_server_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");
        router_server_interface
            .add_address_and_subnet_route(I::ROUTER_SERVER_ADDR_WITH_PREFIX)
            .await
            .expect("configure address");
        router_server_interface.set_ipv4_forwarding_enabled(true).await.expect("enable forwarding");
        router_server_interface.set_ipv6_forwarding_enabled(true).await.expect("enable forwarding");

        let add_host = |name: String, net, subnet, router_addr| async move {
            let realm =
                sandbox.create_netstack_realm::<Netstack3, _>(name.clone()).expect("create realm");

            let interface = realm.join_network(net, name).await.expect("join network");
            interface.add_address_and_subnet_route(subnet).await.expect("configure address");
            interface.apply_nud_flake_workaround().await.expect("nud flake workaround");
            interface.add_default_route(router_addr).await.expect("add router as default gateway");

            (realm, interface)
        };

        let (client, client_interface) = add_host(
            format!("{name}_client"),
            &client_net,
            I::CLIENT_ADDR_WITH_PREFIX,
            I::ROUTER_CLIENT_ADDR_WITH_PREFIX.addr,
        )
        .await;
        let (server, server_interface) = add_host(
            format!("{name}_server"),
            &server_net,
            I::SERVER_ADDR_WITH_PREFIX,
            I::ROUTER_SERVER_ADDR_WITH_PREFIX.addr,
        )
        .await;

        let control = router
            .connect_to_protocol::<fnet_filter::ControlMarker>()
            .expect("connect to protocol");
        let mut controller = Controller::new(&control, &ControllerId(name.to_owned()))
            .await
            .expect("create controller");
        let namespace = NamespaceId(name.to_owned());
        let ip_routine = ip_hook.map(|hook| {
            (hook, RoutineId { namespace: namespace.clone(), name: format!("{hook:?}") })
        });
        let nat_routine = nat_hook.map(|hook| {
            (hook, RoutineId { namespace: namespace.clone(), name: format!("{hook:?}") })
        });
        controller
            .push_changes(
                [Change::Create(Resource::Namespace(Namespace {
                    id: namespace.clone(),
                    domain: Domain::AllIp,
                }))]
                .into_iter()
                .chain(ip_routine.clone().map(|(hook, routine)| {
                    Change::Create(Resource::Routine(Routine {
                        id: routine.clone(),
                        routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                            hook,
                            priority: 0,
                        })),
                    }))
                }))
                .chain(nat_routine.clone().map(|(hook, routine)| {
                    Change::Create(Resource::Routine(Routine {
                        id: routine.clone(),
                        routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                            hook,
                            priority: 0,
                        })),
                    }))
                }))
                .collect(),
            )
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");

        Self {
            router,
            router_client_interface,
            _router_client_net: client_net,
            router_server_interface,
            _router_server_net: server_net,
            client,
            _client_interface: client_interface,
            server,
            _server_interface: server_interface,
            controller,
            namespace,
            ip_routine: ip_routine.map(|(_hook, routine)| routine),
            nat_routine: nat_routine.map(|(_hook, routine)| routine),
            _ip_version: IpVersionMarker::new(),
        }
    }

    async fn drop_traffic<M: Matcher>(
        controller: &mut Controller,
        rule_id: RuleId,
        matcher: &M,
        interfaces: Interfaces<'_>,
        subnets: Subnets,
        ports: Ports,
    ) {
        let mut matcher = matcher.matcher::<I>(interfaces, subnets, ports).await;
        // Only filter traffic that is arriving on this particular interface.
        let interface_matcher = |interface: &netemul::TestInterface<'_>| {
            Some(InterfaceMatcher::Id(
                NonZeroU64::new(interface.id()).expect("interface ID should be nonzero"),
            ))
        };
        let Interfaces { ingress, egress } = interfaces;
        if let Some(interface) = ingress {
            matcher.in_interface = interface_matcher(interface);
        }
        if let Some(interface) = egress {
            matcher.out_interface = interface_matcher(interface);
        }
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

    async fn install_filter_incoming_server_to_client<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self { controller, ip_routine, router_server_interface, .. } = self;
        Self::drop_traffic::<M>(
            controller,
            RuleId {
                routine: ip_routine.clone().expect("IP routine should be installed"),
                index: Self::CLIENT_FILTER_RULE_INDEX,
            },
            matcher,
            Interfaces { ingress: Some(router_server_interface), egress: None },
            Subnets {
                src: I::SERVER_ADDR_WITH_PREFIX,
                dst: I::CLIENT_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.server_ports(),
        )
        .await;
    }

    async fn install_filter_incoming_client_to_server<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self { controller, ip_routine, router_client_interface, .. } = self;
        Self::drop_traffic::<M>(
            controller,
            RuleId {
                routine: ip_routine.clone().expect("IP routine should be installed"),
                index: Self::SERVER_FILTER_RULE_INDEX,
            },
            matcher,
            Interfaces { ingress: Some(router_client_interface), egress: None },
            Subnets {
                src: I::CLIENT_ADDR_WITH_PREFIX,
                dst: I::SERVER_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.client_ports(),
        )
        .await;
    }

    async fn install_filter_outgoing_server_to_client<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self { controller, ip_routine, router_client_interface, .. } = self;
        Self::drop_traffic::<M>(
            controller,
            RuleId {
                routine: ip_routine.clone().expect("IP routine should be installed"),
                index: Self::CLIENT_FILTER_RULE_INDEX,
            },
            matcher,
            Interfaces { ingress: None, egress: Some(router_client_interface) },
            Subnets {
                src: I::SERVER_ADDR_WITH_PREFIX,
                dst: I::CLIENT_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.server_ports(),
        )
        .await;
    }

    async fn install_filter_outgoing_client_to_server<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self { controller, ip_routine, router_server_interface, .. } = self;
        Self::drop_traffic::<M>(
            controller,
            RuleId {
                routine: ip_routine.clone().expect("IP routine should be installed"),
                index: Self::SERVER_FILTER_RULE_INDEX,
            },
            matcher,
            Interfaces { ingress: None, egress: Some(router_server_interface) },
            Subnets {
                src: I::CLIENT_ADDR_WITH_PREFIX,
                dst: I::SERVER_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.client_ports(),
        )
        .await;
    }

    async fn install_filter_forwarded_server_to_client<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self {
            controller, ip_routine, router_client_interface, router_server_interface, ..
        } = self;
        Self::drop_traffic::<M>(
            controller,
            RuleId {
                routine: ip_routine.clone().expect("IP routine should be installed"),
                index: Self::CLIENT_FILTER_RULE_INDEX,
            },
            matcher,
            Interfaces {
                ingress: Some(router_server_interface),
                egress: Some(router_client_interface),
            },
            Subnets {
                src: I::SERVER_ADDR_WITH_PREFIX,
                dst: I::CLIENT_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.server_ports(),
        )
        .await;
    }

    async fn install_filter_forwarded_client_to_server<M: Matcher>(
        &mut self,
        matcher: &M,
        ports: SockAddrs,
    ) {
        let Self {
            controller, ip_routine, router_client_interface, router_server_interface, ..
        } = self;
        Self::drop_traffic::<M>(
            controller,
            RuleId {
                routine: ip_routine.clone().expect("IP routine should be installed"),
                index: Self::SERVER_FILTER_RULE_INDEX,
            },
            matcher,
            Interfaces {
                ingress: Some(router_client_interface),
                egress: Some(router_server_interface),
            },
            Subnets {
                src: I::CLIENT_ADDR_WITH_PREFIX,
                dst: I::SERVER_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            ports.client_ports(),
        )
        .await;
    }

    pub async fn install_nat_rule(&mut self, matchers: Matchers, action: Action) {
        let Self { controller, nat_routine, .. } = self;
        controller
            .push_changes(vec![Change::Create(Resource::Rule(Rule {
                id: RuleId {
                    routine: nat_routine.clone().expect("NAT routine should be installed"),
                    index: Self::NAT_RULE_INDEX,
                },
                matchers,
                action,
            }))])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");
    }

    async fn clear_filter(&mut self) {
        self.controller
            .push_changes(vec![Change::Remove(ResourceId::Namespace(self.namespace.clone()))])
            .await
            .expect("push changes");
        self.controller.commit().await.expect("commit changes");
    }

    pub fn realms(&'a self) -> Realms<'a> {
        let Self { client, server, .. } = self;
        Realms { client, server }
    }

    pub fn addrs() -> Addrs {
        Addrs { client: I::CLIENT_ADDR_WITH_PREFIX.addr, server: I::SERVER_ADDR_WITH_PREFIX.addr }
    }

    pub async fn run_test<S: SocketType>(
        &mut self,
        expected_connectivity: ExpectedConnectivity,
    ) -> OpaqueSocketHandles {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), Self::addrs()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity, None).await
    }

    /// NB: in order for callers to provide a `setup` that captures its environment,
    /// we need to constrain the HRTB lifetime `'b` with `'params: 'b`, i.e.
    /// "`'params`' outlives `'b`". Since "where" clauses are unsupported for HRTB,
    /// the only way to do this is with an implied bound. The type `&'b &'params ()`
    /// is only well-formed if `'params: 'b`, so adding an argument of that type
    /// implies the bound.
    ///
    /// See https://stackoverflow.com/a/72673740 for a more thorough explanation.
    pub async fn run_test_with<'params, S, F>(
        &'params mut self,
        expected_connectivity: ExpectedConnectivity,
        setup: F,
    ) -> OpaqueSocketHandles
    where
        S: SocketType,
        F: for<'b> FnOnce(
            &'b mut TestRouterNet<'a, I>,
            SockAddrs,
            &'b &'params (),
        ) -> LocalBoxFuture<'b, ()>,
    {
        let (sockets, sock_addrs) = S::bind_sockets(self.realms(), Self::addrs()).await;
        setup(self, sock_addrs, &&()).await;
        S::run_test::<I>(self.realms(), sockets, sock_addrs, expected_connectivity, None).await
    }
}

async fn forwarded_traffic_skips_local_ingress<I: RouterTestIpExt, M: Matcher>(
    name: &str,
    hook: IncomingHook,
    matcher: M,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    // Set up a network with two hosts (client and server) and a router. The client
    // and server are both link-layer neighbors with the router but on isolated L2
    // networks.
    let mut net = TestRouterNet::<I>::new(
        &sandbox,
        &name,
        Some(match hook {
            IncomingHook::Ingress => IpHook::Ingress,
            IncomingHook::LocalIngress => IpHook::LocalIngress,
        }),
        None, /* nat_hook */
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule on either the ingress or local ingress hook on the router that
    // drops traffic from the server to the client. If the rule was installed on the
    // local ingress hook, this should have no effect on connectivity because all of
    // this traffic is being forwarded. If the rule was installed on the ingress
    // hook, this should still allow traffic to go from the client to the server,
    // but not the reverse.
    let _handles = net
        .run_test_with::<M::SocketType, _>(
            match hook {
                IncomingHook::Ingress => ExpectedConnectivity::ClientToServerOnly,
                IncomingHook::LocalIngress => ExpectedConnectivity::TwoWay,
            },
            |net, addrs, ()| {
                Box::pin(async move {
                    net.install_filter_incoming_server_to_client(&matcher, addrs).await;
                })
            },
        )
        .await;

    // Install a similar rule on the same hook, but which drops traffic from the
    // client to the server. For local ingress, this should again have no effect.
    // For ingress, this should result in neither host being able to reach each
    // other.
    let _handles = net
        .run_test_with::<M::SocketType, _>(
            match hook {
                IncomingHook::Ingress => ExpectedConnectivity::None,
                IncomingHook::LocalIngress => ExpectedConnectivity::TwoWay,
            },
            |net, addrs, ()| {
                Box::pin(async move {
                    net.install_filter_incoming_client_to_server(&matcher, addrs).await;
                })
            },
        )
        .await;

    // Remove all filtering rules; two-way connectivity should now be possible
    // again.
    net.clear_filter().await;
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;
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

async fn forwarded_traffic_skips_local_egress<I: RouterTestIpExt, M: Matcher>(
    name: &str,
    hook: OutgoingHook,
    matcher: M,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    // Set up a network with two hosts (client and server) and a router. The client
    // and server are both link-layer neighbors with the router but on isolated L2
    // networks.
    let mut net = TestRouterNet::<I>::new(
        &sandbox,
        &name,
        Some(match hook {
            OutgoingHook::LocalEgress => IpHook::LocalEgress,
            OutgoingHook::Egress => IpHook::Egress,
        }),
        None, /* nat_hook */
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule on either the egress or local egress hook on the router that
    // drops traffic from the server to the client. If the rule was installed on the
    // local egress hook, this should have no effect on connectivity because all of
    // this traffic is being forwarded. If the rule was installed on the egress
    // hook, this should still allow traffic to go from the client to the server,
    // but not the reverse.
    let _handles = net
        .run_test_with::<M::SocketType, _>(
            match hook {
                OutgoingHook::LocalEgress => ExpectedConnectivity::TwoWay,
                OutgoingHook::Egress => ExpectedConnectivity::ClientToServerOnly,
            },
            |net, addrs, ()| {
                Box::pin(async move {
                    net.install_filter_outgoing_server_to_client(&matcher, addrs).await;
                })
            },
        )
        .await;

    // Install a similar rule on the same hook, but which drops traffic from the
    // client to the server. For local ingress, this should again have no effect.
    // For ingress, this should result in neither host being able to reach each
    // other.
    let _handles = net
        .run_test_with::<M::SocketType, _>(
            match hook {
                OutgoingHook::LocalEgress => ExpectedConnectivity::TwoWay,
                OutgoingHook::Egress => ExpectedConnectivity::None,
            },
            |net, addrs, ()| {
                Box::pin(async move {
                    net.install_filter_outgoing_client_to_server(&matcher, addrs).await;
                })
            },
        )
        .await;

    // Remove all filtering rules; two-way connectivity should now be possible
    // again.
    net.clear_filter().await;
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;
}

generate_test_cases_for_all_matchers!(
    forwarded_traffic_skips_local_egress,
    OutgoingHook::LocalEgress,
    local_egress
);

generate_test_cases_for_all_matchers!(
    forwarded_traffic_skips_local_egress,
    OutgoingHook::Egress,
    egress
);

async fn drop_forwarded<I: RouterTestIpExt, M: Matcher>(name: &str, matcher: M) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    // Set up a network with two hosts (client and server) and a router. The client
    // and server are both link-layer neighbors with the router but on isolated L2
    // networks.
    let mut net = TestRouterNet::<I>::new(
        &sandbox,
        &name,
        Some(IpHook::Forwarding),
        None, /* nat_hook */
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Install a rule on the forwarding hook on the router that drops traffic
    // from the server to the client. This should still allow traffic to go from
    // the client to the server, but not the reverse.
    let _handles = net
        .run_test_with::<M::SocketType, _>(
            ExpectedConnectivity::ClientToServerOnly,
            |net, addrs, ()| {
                Box::pin(async move {
                    net.install_filter_forwarded_server_to_client(&matcher, addrs).await;
                })
            },
        )
        .await;

    // Install a similar rule on the same hook, but which drops traffic from the
    // client to the server. This should result in neither host being able to
    // reach each other.
    let _handles = net
        .run_test_with::<M::SocketType, _>(ExpectedConnectivity::None, |net, addrs, ()| {
            Box::pin(async move {
                net.install_filter_forwarded_client_to_server(&matcher, addrs).await;
            })
        })
        .await;

    // Remove all filtering rules; two-way connectivity should now be possible
    // again.
    net.clear_filter().await;
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;
}

generate_test_cases_for_all_matchers!(drop_forwarded);

async fn local_traffic_skips_forwarding<I: RouterTestIpExt, M: Matcher>(name: &str, matcher: M) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    // Set up a network with two hosts (client and server) and a router. The client
    // and server are both link-layer neighbors with the router but on isolated L2
    // networks.
    let mut net = TestRouterNet::<I>::new(
        &sandbox,
        &name,
        Some(IpHook::Forwarding),
        None, /* nat_hook */
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured. Having client-server
    // connectivity implies that client-router and server-router connectivity is
    // also established.
    let _handles = net.run_test::<M::SocketType>(ExpectedConnectivity::TwoWay).await;

    async fn drop_traffic_between_realms<I: RouterTestIpExt, M: Matcher>(
        controller: &mut Controller,
        routine: RoutineId,
        matcher: &M,
        subnets: Subnets,
        sock_addrs: SockAddrs,
    ) {
        static INDEX: AtomicU32 = AtomicU32::new(0);

        TestRouterNet::<I>::drop_traffic::<M>(
            controller,
            RuleId { routine: routine.clone(), index: INDEX.fetch_add(1, Ordering::SeqCst) },
            matcher,
            Interfaces { ingress: None, egress: None },
            subnets,
            sock_addrs.client_ports(),
        )
        .await;

        TestRouterNet::<I>::drop_traffic::<M>(
            controller,
            RuleId { routine, index: INDEX.fetch_add(1, Ordering::SeqCst) },
            matcher,
            Interfaces { ingress: None, egress: None },
            Subnets { src: subnets.dst, dst: subnets.src, other: subnets.other },
            sock_addrs.server_ports(),
        )
        .await;
    }

    async fn drop_forwarded_traffic_and_assert_connectivity<I: RouterTestIpExt, M: Matcher>(
        controller: &mut Controller,
        routine: RoutineId,
        matcher: &M,
        realms: Realms<'_>,
        subnets: Subnets,
    ) -> OpaqueSocketHandles {
        let (sockets, sock_addrs) = M::SocketType::bind_sockets(
            realms,
            Addrs { client: subnets.src.addr, server: subnets.dst.addr },
        )
        .await;

        drop_traffic_between_realms::<I, M>(
            controller,
            routine.clone(),
            &matcher,
            subnets,
            sock_addrs,
        )
        .await;

        M::SocketType::run_test::<I>(
            realms,
            sockets,
            sock_addrs,
            ExpectedConnectivity::TwoWay,
            None,
        )
        .await
    }

    let TestRouterNet {
        ref mut controller,
        ref ip_routine,
        ref client,
        ref server,
        ref router,
        ..
    } = net;

    // Dropping traffic between the client and router in the forwarding hook
    // should not affect client-router connectivity, because this traffic is
    // never forwarded; it is always locally-generated and locally-delivered.
    let _handles = drop_forwarded_traffic_and_assert_connectivity::<I, M>(
        controller,
        ip_routine.clone().expect("IP routine should be installed"),
        &matcher,
        Realms { client, server: router },
        Subnets {
            src: I::CLIENT_ADDR_WITH_PREFIX,
            dst: I::ROUTER_CLIENT_ADDR_WITH_PREFIX,
            other: I::OTHER_SUBNET,
        },
    )
    .await;

    // Dropping traffic between the server and router in the forwarding hook
    // should not affect server-router connectivity, because this traffic is
    // never forwarded; it is always locally-generated and locally-delivered.
    let _handles = drop_forwarded_traffic_and_assert_connectivity::<I, M>(
        controller,
        ip_routine.clone().expect("IP routine should be installed"),
        &matcher,
        Realms { client: server, server: router },
        Subnets {
            src: I::SERVER_ADDR_WITH_PREFIX,
            dst: I::ROUTER_SERVER_ADDR_WITH_PREFIX,
            other: I::OTHER_SUBNET,
        },
    )
    .await;
}

generate_test_cases_for_all_matchers!(local_traffic_skips_forwarding);
