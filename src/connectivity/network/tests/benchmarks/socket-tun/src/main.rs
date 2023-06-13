// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Read as _, Write as _};

use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_stack as fnet_stack;
use fidl_fuchsia_net_tun as fnet_tun;
use fidl_fuchsia_posix_socket as fposix_socket;
use futures::FutureExt as _;

use libc as _;
use net_declare::fidl_subnet;
use netstack_testing_common::{
    devices, interfaces,
    realms::{KnownServiceProvider, Netstack, ProdNetstack2, ProdNetstack3, TestSandboxExt as _},
};

trait IpExt {
    const CLIENT_SUBNET: fnet::Subnet;
    const SERVER_SUBNET: fnet::Subnet;
    const NAME: &'static str;
    const DOMAIN: fposix_socket::Domain;
}

impl IpExt for net_types::ip::Ipv4 {
    const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.254/24");
    const NAME: &'static str = "IPv4";
    const DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv4;
}

impl IpExt for net_types::ip::Ipv6 {
    const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("2001:0db8:abcd:efff::1/64");
    const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("2001:0db8:abcd:efff:ffff:ffff:ffff:ffff/64");
    const NAME: &'static str = "IPv6";
    const DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv6;
}

// This type holds unused FIDL proxies so that the underlying object stays
// alive.
struct InterfaceFidlProxies {
    _device_control: fnet_interfaces_admin::DeviceControlProxy,
    _control: fidl_fuchsia_net_interfaces_ext::admin::Control,
}

// This type holds unused FIDL proxies so that the underlying object stays
// alive.
struct FidlProxies {
    _tun_dev_pair: fnet_tun::DevicePairProxy,
    _client_if: InterfaceFidlProxies,
    _server_if: InterfaceFidlProxies,
}

async fn setup<'a, N: Netstack>(
    sandbox: &'a netemul::TestSandbox,
) -> (netemul::TestRealm<'a>, netemul::TestRealm<'a>, FidlProxies) {
    let client_realm = sandbox
        .create_netstack_realm_with::<N, _, _>("client", &[KnownServiceProvider::SecureStash])
        .expect("create client netstack");
    let server_realm = sandbox
        .create_netstack_realm_with::<N, _, _>("server", &[KnownServiceProvider::SecureStash])
        .expect("create server netstack");

    let (tun_dev_pair, left_port, right_port) =
        netstack_testing_common::devices::create_eth_tun_pair().await;

    async fn install_interface(
        realm: &netemul::TestRealm<'_>,
        port: fhardware_network::PortProxy,
        subnets: impl IntoIterator<Item = fnet::Subnet>,
    ) -> InterfaceFidlProxies {
        let device = {
            let (device, server_end) =
                fidl::endpoints::create_endpoints::<fhardware_network::DeviceMarker>();
            let () = port.get_device(server_end).expect("get device");
            device
        };
        let device_control = devices::install_device(realm, device);

        let control = {
            let port_id =
                port.get_info().await.expect("get port info").id.expect("port ID must be present");
            let (control, server_end) =
                fnet_interfaces_ext::admin::Control::create_endpoints().expect("create endpoints");
            let () = device_control
                .create_interface(&port_id, server_end, &fnet_interfaces_admin::Options::default())
                .expect("create interface");
            control
        };
        assert!(control
            .enable()
            .await
            .expect("enable interface FIDL call")
            .expect("enable interface"));

        for subnet in subnets {
            let addr_state_provider = interfaces::add_address_wait_assigned(
                &control,
                subnet,
                fnet_interfaces_admin::AddressParameters::default(),
            )
            .await
            .expect("add address and wait for it to be assigned");
            addr_state_provider.detach().expect("address state provider detach FIDL call");

            {
                let subnet = fnet_ext::apply_subnet_mask(subnet);
                realm
                    .connect_to_protocol::<fnet_stack::StackMarker>()
                    .expect("connect to Stack")
                    .add_forwarding_entry(&fnet_stack::ForwardingEntry {
                        subnet,
                        device_id: control.get_id().await.expect("get interface ID"),
                        next_hop: None,
                        metric: 0,
                    })
                    .await
                    .expect("add subnet route FIDL call")
                    .expect("add subent route");
            }
        }

        InterfaceFidlProxies { _device_control: device_control, _control: control }
    }
    let (_client_if, _server_if) = futures::future::join(
        install_interface(
            &client_realm,
            left_port,
            [net_types::ip::Ipv4::CLIENT_SUBNET, net_types::ip::Ipv6::CLIENT_SUBNET],
        ),
        install_interface(
            &server_realm,
            right_port,
            [net_types::ip::Ipv4::SERVER_SUBNET, net_types::ip::Ipv6::SERVER_SUBNET],
        ),
    )
    .await;

    (
        client_realm,
        server_realm,
        FidlProxies { _tun_dev_pair: tun_dev_pair, _client_if, _server_if },
    )
}

fn format_byte_count(byte_count: usize) -> String {
    if byte_count >= 1024 {
        format!("{}KiB", byte_count / 1024)
    } else {
        format!("{}B", byte_count)
    }
}

async fn bench_tcp<'a, I: IpExt>(
    test_suite: &'static str,
    iter_count: usize,
    client_realm: &netemul::TestRealm<'a>,
    server_realm: &netemul::TestRealm<'a>,
    transfer: usize,
) -> fuchsiaperf::FuchsiaPerfBenchmarkResult {
    let (mut client_sock, mut server_sock) = {
        let (listen_sock, client_sock) = futures::future::join(
            server_realm
                .stream_socket(I::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
                .map(|r| r.expect("create listening socket")),
            client_realm
                .stream_socket(I::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
                .map(|r| r.expect("create client socket")),
        )
        .await;

        // Since we want to avoid including the overhead of the async
        // executor in the benchmarked read/write steps, intentionally keep
        // the sockets non-async-aware and use `socket2` calls directly to
        // connect the sockets.
        let bind_sockaddr = {
            let fnet_ext::IpAddress(listen_addr) = I::SERVER_SUBNET.addr.into();
            socket2::SockAddr::from(std::net::SocketAddr::from((listen_addr, 0)))
        };
        listen_sock.bind(&bind_sockaddr).expect("bind");
        listen_sock.listen(0).expect("listen");
        let listen_sockaddr = listen_sock.local_addr().expect("local addr");

        if client_sock.send_buffer_size().expect("get send buffer size") < transfer {
            // Set send buffer to transfer size to ensure we can write
            // `transfer` bytes before reading it on the other end.
            client_sock
                .set_send_buffer_size(transfer)
                .expect("set send buffer size to transfer size");
            assert!(client_sock.send_buffer_size().expect("get send buffer size") >= transfer);
        }
        // Disable the Nagle algorithm, it introduces artificial
        // latency that defeats this benchmark.
        client_sock.set_nodelay(true).expect("set TCP NODELAY to true");
        client_sock.connect(&listen_sockaddr).expect("connect");

        let (server_sock, _): (_, socket2::SockAddr) = listen_sock.accept().expect("accept");
        (client_sock, server_sock)
    };

    let values = (0..iter_count)
        .map(|_| {
            // The [zero page scanner] may reclaim large memory filled with zeroes, and
            // even though it is turned off in environments where the benchmark results
            // are collected, we fill buffers with a non-zero byte just to be extra
            // safe.
            //
            // [zero page scanner]: https://fuchsia.dev/fuchsia-src/gen/boot-options?hl=en#kernelpage-scannerzero-page-scans-per-seconduint64_t
            let send_buf = vec![0xAA; transfer];
            let mut recv_buf = vec![0xBB; transfer];
            let now = std::time::Instant::now();
            client_sock.write_all(send_buf.as_ref()).expect("write failed");
            server_sock.read_exact(recv_buf.as_mut()).expect("read failed");
            now.elapsed().as_nanos() as f64
        })
        .collect();

    fuchsiaperf::FuchsiaPerfBenchmarkResult {
        test_suite: test_suite.into(),
        label: format!("WriteRead/TCP/{}/{}", I::NAME, format_byte_count(transfer)),
        unit: "ns".into(),
        values,
    }
}

#[derive(argh::FromArgs)]
/// E2E socket benchmark arguments.
struct Args {
    /// run in perf mode and write metrics in fuchsiaperf JSON format to
    /// the provided path
    #[argh(option)]
    output_fuchsiaperf: Option<std::path::PathBuf>,

    /// run with Netstack3
    #[argh(switch)]
    netstack3: bool,
}

#[fuchsia::main]
async fn main() {
    let Args { output_fuchsiaperf, netstack3 } = argh::from_env();
    let iter_count = if output_fuchsiaperf.is_some() { 1000 } else { 3 };
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");

    let (test_suite, (client_realm, server_realm, _fidl_proxies)) = if netstack3 {
        ("fuchsia.network.socket.tun.netstack3", setup::<ProdNetstack3>(&sandbox).await)
    } else {
        ("fuchsia.network.socket.tun", setup::<ProdNetstack2>(&sandbox).await)
    };

    let metrics = {
        let mut metrics = Vec::new();
        for transfer in [1 << 10, 10 << 10, 100 << 10, 500 << 10, 1000 << 10] {
            metrics.push(
                bench_tcp::<net_types::ip::Ipv4>(
                    test_suite,
                    iter_count,
                    &client_realm,
                    &server_realm,
                    transfer,
                )
                .await,
            );
            metrics.push(
                bench_tcp::<net_types::ip::Ipv6>(
                    test_suite,
                    iter_count,
                    &client_realm,
                    &server_realm,
                    transfer,
                )
                .await,
            );
        }
        metrics
    };

    if let Some(output_fuchsiaperf) = output_fuchsiaperf {
        let metrics_json =
            serde_json::to_string_pretty(&metrics).expect("serialize metrics as JSON");
        std::fs::write(output_fuchsiaperf, metrics_json).expect("write metrics as custom artifact");
    }
}
