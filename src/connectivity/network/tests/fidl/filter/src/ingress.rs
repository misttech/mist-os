// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

mod matchers;

use assert_matches::assert_matches;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext::{
    Action, Change, Controller, ControllerId, Domain, InstalledIpRoutine, IpHook, Namespace,
    NamespaceId, Resource, ResourceId, Routine, RoutineId, RoutineType, Rule, RuleId,
};
use fidl_fuchsia_posix_socket as fposix_socket;
use fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _};
use futures::{
    future::LocalBoxFuture,
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    FutureExt as _, StreamExt as _, TryFutureExt as _,
};
use net_declare::fidl_subnet;
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6};
use netemul::{RealmTcpListener as _, RealmUdpSocket as _};
use netstack_testing_common::{
    realms::{Netstack3, TestSandboxExt as _},
    ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use test_case::test_case;

use matchers::{
    AllTraffic, DstAddressRange, DstAddressSubnet, Icmp, InterfaceDeviceClass, InterfaceId,
    InterfaceName, Inversion, Matcher, SrcAddressRange, SrcAddressSubnet, Tcp, TcpDstPort,
    TcpSrcPort, Udp, UdpDstPort, UdpSrcPort,
};

const NEGATIVE_CHECK_TIMEOUT: fuchsia_async::Duration = fuchsia_async::Duration::from_seconds(1);

#[derive(Clone, Copy)]
enum ExpectedConnectivity {
    TwoWay,
    ClientToServerOnly,
    None,
}

trait TestIpExt: ping::FuchsiaIpExt {
    const CLIENT_SUBNET: fnet::Subnet;
    const SERVER_SUBNET: fnet::Subnet;
    const OTHER_SUBNET: fnet::Subnet;
}

impl TestIpExt for Ipv4 {
    const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.1/30");
    const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.2/30");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.4/30");
}

impl TestIpExt for Ipv6 {
    const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("2001:db8::2/64");
    const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("2001:db8::1/64");
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("2001:db81::/64");
}

trait SocketType {
    type Client;
    type Server;

    async fn bind_sockets(net: &TestNet<'_>) -> (Sockets<Self>, SockAddrs);

    async fn run_test<I: ping::FuchsiaIpExt>(
        net: &TestNet<'_>,
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

    async fn bind_sockets(net: &TestNet<'_>) -> (Sockets<Self>, SockAddrs) {
        let (Sockets { client, server }, addrs) = UdpSocket::bind_sockets(net).await;
        (Sockets { client, server }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        net: &TestNet<'_>,
        sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let Sockets { client: client_sock, server: server_sock } = sockets;
        UdpSocket::run_test::<I>(
            net,
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

    async fn bind_sockets(net: &TestNet<'_>) -> (Sockets<Self>, SockAddrs) {
        let TestNet { client, server } = net;

        let fnet_ext::IpAddress(client_addr) = client.local_subnet.addr.into();
        let client_addr =
            std::net::SocketAddr::new(client_addr, /* let netstack pick the port */ 0);

        let fnet_ext::IpAddress(server_addr) = server.local_subnet.addr.into();
        let server_addr =
            std::net::SocketAddr::new(server_addr, /* let netstack pick the port */ 0);

        let server =
            fasync::net::TcpListener::listen_in_realm_with(&server.realm, server_addr, |socket| {
                Ok(socket.set_reuse_address(true).expect("set reuse address"))
            })
            .await
            .expect("listen on server");

        let client = client
            .realm
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
        _net: &TestNet<'_>,
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
                        .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || None)
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
                        .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
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

    async fn bind_sockets(net: &TestNet<'_>) -> (Sockets<Self>, SockAddrs) {
        let TestNet { client, server } = net;

        let fnet_ext::IpAddress(client_addr) = client.local_subnet.addr.into();
        let client_addr =
            std::net::SocketAddr::new(client_addr, /* let netstack pick the port */ 0);

        let fnet_ext::IpAddress(server_addr) = server.local_subnet.addr.into();
        let server_addr =
            std::net::SocketAddr::new(server_addr, /* let netstack pick the port */ 0);

        let client_sock = fasync::net::UdpSocket::bind_in_realm(&client.realm, client_addr)
            .await
            .expect("bind socket");
        let server_sock = fasync::net::UdpSocket::bind_in_realm(&server.realm, server_addr)
            .await
            .expect("bind socket");

        let addrs = SockAddrs {
            client: client_sock.local_addr().expect("get client addr"),
            server: server_sock.local_addr().expect("get server addr"),
        };

        (Sockets { client: client_sock, server: server_sock }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        _net: &TestNet<'_>,
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

    async fn bind_sockets(net: &TestNet<'_>) -> (Sockets<Self>, SockAddrs) {
        let TestNet { client, server } = net;

        let fnet_ext::IpAddress(client_addr) = client.local_subnet.addr.into();
        let client_addr = std::net::SocketAddr::new(client_addr, 0);

        let fnet_ext::IpAddress(server_addr) = server.local_subnet.addr.into();
        let server_addr = std::net::SocketAddr::new(server_addr, 0);

        let addrs = SockAddrs { client: client_addr, server: server_addr };

        (Sockets { client: (), server: () }, addrs)
    }

    async fn run_test<I: ping::FuchsiaIpExt>(
        net: &TestNet<'_>,
        _sockets: Sockets<Self>,
        sock_addrs: SockAddrs,
        expected_connectivity: ExpectedConnectivity,
    ) {
        let TestNet { client, server } = net;

        const SEQ: u16 = 1;

        async fn expect_ping_timeout<I: ping::FuchsiaIpExt>(
            realm: &TestRealm<'_>,
            addr: I::SockAddr,
        ) {
            match realm
                .realm
                .ping_once::<I>(addr, SEQ)
                .map_ok(Some)
                .on_timeout(NEGATIVE_CHECK_TIMEOUT.after_now(), || Ok(None))
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
                    client.realm.ping_once::<I>(server_addr, SEQ).map(|r| r.expect("ping")),
                    server.realm.ping_once::<I>(client_addr, SEQ).map(|r| r.expect("ping")),
                )
                .await;
            }
        }
    }
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
        Ports { local: client.port(), remote: server.port() }
    }

    fn server_ports(&self) -> Ports {
        let Self { client, server } = self;
        Ports { local: server.port(), remote: client.port() }
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

struct Ports {
    local: u16,
    remote: u16,
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
    ) -> Self {
        let client_name = format!("{name}_client");
        let client =
            TestRealm::new::<I>(&sandbox, network, client_name, I::CLIENT_SUBNET, I::SERVER_SUBNET)
                .await;
        let server_name = format!("{name}_server");
        let server =
            TestRealm::new::<I>(&sandbox, network, server_name, I::SERVER_SUBNET, I::CLIENT_SUBNET)
                .await;

        Self { client, server }
    }

    async fn run_test<I, S>(&mut self, expected_connectivity: ExpectedConnectivity)
    where
        I: TestIpExt,
        S: SocketType,
    {
        let (sockets, addrs) = S::bind_sockets(self).await;
        S::run_test::<I>(self, sockets, addrs, expected_connectivity).await;
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
        let (sockets, addrs) = S::bind_sockets(self).await;
        setup(self, addrs, &&()).await;
        S::run_test::<I>(self, sockets, addrs, expected_connectivity).await;
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
        let routine = RoutineId { namespace: namespace.clone(), name: String::from("ingress") };
        controller
            .push_changes(vec![
                Change::Create(Resource::Namespace(Namespace {
                    id: namespace.clone(),
                    domain: Domain::AllIp,
                })),
                Change::Create(Resource::Routine(Routine {
                    id: routine.clone(),
                    routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                        hook: IpHook::Ingress,
                        priority: 0,
                    })),
                })),
            ])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit changes");

        Self { realm, interface, controller, namespace, routine, local_subnet, remote_subnet }
    }

    async fn install_rule_on_ingress<I: TestIpExt, M: Matcher>(
        &mut self,
        index: u32,
        matcher: &M,
        ports: Ports,
        action: Action,
    ) {
        let matcher = matcher.matcher::<I>(self, ports).await;
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
async fn ingress_drop<I: net_types::ip::Ip + TestIpExt, M: Matcher>(name: &str, matcher: M) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let net = sandbox.create_network("net").await.expect("create network");

    let mut net = TestNet::new::<I>(&sandbox, &net, name).await;

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
                    .install_rule_on_ingress::<I, _>(
                        2,
                        &matcher,
                        addrs.client_ports(),
                        Action::Accept,
                    )
                    .await;
                server
                    .install_rule_on_ingress::<I, _>(
                        2,
                        &matcher,
                        addrs.server_ports(),
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
                    .install_rule_on_ingress::<I, _>(
                        1,
                        &matcher,
                        addrs.client_ports(),
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
                    .install_rule_on_ingress::<I, _>(
                        0,
                        &matcher,
                        addrs.client_ports(),
                        Action::Drop,
                    )
                    .await;
                server
                    .install_rule_on_ingress::<I, _>(
                        0,
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
    net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;
}
