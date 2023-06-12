// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{format_err, Context as _, Error},
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_routes as fnet_routes,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext,
    fidl_fuchsia_net_stack::StackMarker,
    fuchsia_async as fasync,
    fuchsia_component::client,
    net_declare::fidl_subnet,
    net_types::ip::Ipv6,
    prettytable::{cell, format, row, Table},
    std::collections::HashMap,
    std::io::{Read as _, Write as _},
    std::net::{SocketAddr, TcpListener, TcpStream},
    structopt::StructOpt,
    tracing::info,
};

const BUS_NAME: &str = "test-bus";
const WEAVE_NODE_NAME: &str = "weave-node";
const FUCHSIA_NODE_NAME: &str = "fuchsia-node";
const WLAN_NODE_NAME: &str = "wlan-node";
const WPAN_NODE_NAME: &str = "wpan-node";
const WLAN_NODE_1_NAME: &str = "wlan-node-1";
const WPAN_SERVER_NODE_NAME: &str = "wpan-server-node";
const HELLO_MSG_REQ: &str = "Hello World from TCP Client!";
const HELLO_MSG_RSP: &str = "Hello World from TCP Server!";
const WEAVE_SERVER_NODE_DONE: i32 = 1;
const WPAN_SERVER_NODE_DONE: i32 = 2;
const ENTRY_METRICS: u32 = 256;

fn get_interface_id(
    want_name: &str,
    intf: &HashMap<u64, fnet_interfaces_ext::Properties>,
) -> Result<u64, Error> {
    intf.values()
        .find_map(
            |fidl_fuchsia_net_interfaces_ext::Properties {
                 id,
                 name,
                 device_class: _,
                 online: _,
                 addresses: _,
                 has_default_ipv4_route: _,
                 has_default_ipv6_route: _,
             }| if name == want_name { Some(id.get()) } else { None },
        )
        .ok_or(anyhow::format_err!("failed to find {}", want_name))
}

async fn add_route_table_entry(
    stack_proxy: &fidl_fuchsia_net_stack::StackProxy,
    subnet: fidl_fuchsia_net::Subnet,
    nicid: u64,
) -> Result<(), Error> {
    let entry = fidl_fuchsia_net_stack::ForwardingEntry {
        subnet,
        device_id: nicid,
        next_hop: None,
        metric: ENTRY_METRICS,
    };
    stack_proxy
        .add_forwarding_entry(&entry)
        .await
        .with_context(|| format!("failed to send add fowrarding entry {:?}", entry))?
        .map_err(|e: fidl_fuchsia_net_stack::Error| {
            format_err!("failed to add fowrarding entry {:?}: {:?}", entry, e)
        })
}

async fn run_fuchsia_node() -> Result<(), Error> {
    let interface_state =
        fuchsia_component::client::connect_to_protocol::<fnet_interfaces::StateMarker>()
            .context("failed to connect to interfaces/State")?;
    let stack =
        client::connect_to_protocol::<StackMarker>().context("failed to connect to netstack")?;

    let stream = fnet_interfaces_ext::event_stream_from_state(
        &interface_state,
        fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
    )
    .context("failed to get interface stream")?;
    let intf = fnet_interfaces_ext::existing(stream, HashMap::new())
        .await
        .context("failed to get existing interfaces")?;
    let wlan_if_id = get_interface_id("wlan-f-ep", &intf)?;
    let wpan_if_id = get_interface_id("wpan-f-ep", &intf)?;
    let weave_if_id = get_interface_id("weave-f-ep", &intf)?;

    info!(wlan_intf = ?wlan_if_id);
    info!(wpan_intf = ?wpan_if_id);
    info!(weave_intf = ?weave_if_id);

    // routing rules for weave tun
    let () = add_route_table_entry(
        &stack,
        fidl_subnet!("fdce:da10:7616:6:6616:6600:4734:b051/128"),
        weave_if_id,
    )
    .await
    .context("adding routing table entry for weave tun")?;
    let () = add_route_table_entry(&stack, fidl_subnet!("fdce:da10:7616::/48"), weave_if_id)
        .await
        .context("adding routing table entry for weave tun")?;

    // routing rules for wpan
    let () = add_route_table_entry(&stack, fidl_subnet!("fdce:da10:7616:6::/64"), wpan_if_id)
        .await
        .context("adding routing table entry for wpan")?;
    let () = add_route_table_entry(&stack, fidl_subnet!("fdd3:b786:54dc::/64"), wpan_if_id)
        .await
        .context("adding routing table entry for wpan")?;

    // routing rules for wlan
    let () = add_route_table_entry(&stack, fidl_subnet!("fdce:da10:7616:1::/64"), wlan_if_id)
        .await
        .context("adding routing table entry for wlan")?;

    info!("successfully added entries to route table");

    let ipv6_routing_table = {
        let state_v6 = client::connect_to_protocol::<fnet_routes::StateV6Marker>()
            .context("connect to protocol")?;
        let stream = fnet_routes_ext::event_stream_from_state::<Ipv6>(&state_v6)
            .context("failed to connect to watcher")?;
        futures::pin_mut!(stream);
        fnet_routes_ext::collect_routes_until_idle::<_, Vec<_>>(stream)
            .await
            .context("failed to get routing table")?
    };

    let mut t = Table::new();
    t.set_format(format::FormatBuilder::new().padding(2, 2).build());

    t.set_titles(row!["Destination", "Gateway", "NICID", "Metric"]);
    for route in ipv6_routing_table {
        let fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route { destination, action, properties: _ },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        } = route;
        let (outbound_interface, next_hop) = match action {
            fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                outbound_interface,
                next_hop,
            }) => (outbound_interface, next_hop),
            fnet_routes_ext::RouteAction::Unknown => panic!("route with unknown action"),
        };
        let next_hop = next_hop.map(|next_hop| next_hop.to_string());
        let next_hop = next_hop.as_ref().map_or("-", |s| s.as_str());
        t.add_row(row![destination, next_hop, outbound_interface, metric]);
    }

    info!("{}", t.printstd());

    let bus = netemul_sync::Bus::subscribe(BUS_NAME, FUCHSIA_NODE_NAME)?;
    info!("waiting for server to finish...");
    let () = bus
        .wait_for_events(vec![
            netemul_sync::Event::from_code(WEAVE_SERVER_NODE_DONE),
            netemul_sync::Event::from_code(WPAN_SERVER_NODE_DONE),
        ])
        .await?;
    info!("fuchsia node exited");
    Ok(())
}

async fn handle_request(mut stream: TcpStream, remote: SocketAddr) -> Result<(), Error> {
    info!("accepted connection from {}", remote);

    let mut buffer = [0; 512];
    let rd = stream.read(&mut buffer).context("read failed")?;

    let req = String::from_utf8(buffer[0..rd].to_vec()).context("not a valid utf8")?;
    if req != HELLO_MSG_REQ {
        return Err(format_err!("Got unexpected request from client: {}", req));
    }
    info!("Got request {}", req);
    let bytes_written = stream.write(HELLO_MSG_RSP.as_bytes()).context("write failed")?;
    if bytes_written != HELLO_MSG_RSP.len() {
        return Err(format_err!("response not fully written to TCP stream: {}", bytes_written));
    }
    stream.flush().context("flush failed")
}

async fn run_server_node(
    listen_addrs: Vec<String>,
    conn_nums: Vec<u32>,
    node_name: &str,
    node_code: i32,
) -> Result<(), Error> {
    let mut listener_vec = Vec::new();
    for listen_addr in listen_addrs {
        listener_vec.push(TcpListener::bind(listen_addr).context("Can't bind to address")?);
    }
    info!("server {} for connections...", node_name);
    let bus = netemul_sync::Bus::subscribe(BUS_NAME, node_name)?;

    for listener_idx in 0..listener_vec.len() {
        let mut handler_futs = Vec::new();
        for _ in 0..conn_nums[listener_idx] {
            let (stream, remote) = listener_vec[listener_idx].accept().unwrap();
            handler_futs.push(handle_request(stream, remote));
        }
        for handler_fut in handler_futs {
            let () = handler_fut.await?;
        }
    }

    let () = bus.publish(netemul_sync::Event::from_code(node_code))?;

    info!("server {} exited successfully", node_name);

    Ok(())
}

async fn get_test_fut_client(connect_addr: String) -> Result<(), Error> {
    let mut stream = TcpStream::connect(connect_addr.clone()).context("Tcp connection failed")?;
    let request = HELLO_MSG_REQ.as_bytes();
    let bytes_written = stream.write(request)?;
    if bytes_written != request.len() {
        return Err(format_err!(
            "request not fully written to TCP stream: {}/{}",
            bytes_written,
            request.len(),
        ));
    }
    stream.flush()?;

    let mut buffer = [0; 512];
    let rd = stream.read(&mut buffer)?;
    let rsp = String::from_utf8(buffer[0..rd].to_vec()).context("not a valid utf8")?;
    info!("got response {} from {}", rsp, connect_addr);
    if rsp != HELLO_MSG_RSP {
        return Err(format_err!("Got unexpected echo from server: {}", rsp));
    }
    Ok(())
}

async fn run_client_node(
    connect_addrs: Vec<String>,
    node_name: &str,
    server_node_names: Vec<&'static str>,
) -> Result<(), Error> {
    let bus = netemul_sync::Bus::subscribe(BUS_NAME, node_name)?;
    info!("client {} is up and for fuchsia node to start", node_name);
    let () = bus.wait_for_client(FUCHSIA_NODE_NAME).await?;
    for server_node_name in server_node_names {
        info!("waiting for server node {} to start...", server_node_name);
        let () = bus.wait_for_client(server_node_name).await?;
    }

    let futs = connect_addrs.into_iter().map(|connect_addr| async move {
        info!("connecting to {}...", connect_addr);
        let result = get_test_fut_client(connect_addr.clone()).await;
        match result {
            Ok(()) => info!("connected to {}", connect_addr),
            Err(ref e) => info!("failed to connect to {}: {}", connect_addr, e),
        };
        result
    });

    let _: Vec<()> = futures::future::try_join_all(futs).await?;

    info!("client {} exited", node_name);
    Ok(())
}

#[derive(StructOpt, Debug)]
enum Opt {
    #[structopt(name = "weave-node")]
    WeaveNode { listen_addr_0: String, listen_addr_1: String },
    #[structopt(name = "fuchsia-node")]
    FuchsiaNode,
    #[structopt(name = "wpan-node")]
    WpanNode { connect_addr_0: String, connect_addr_1: String, listen_addr_0: String },
    #[structopt(name = "wlan-node")]
    WlanNode { connect_addr_0: String, connect_addr_1: String, connect_addr_2: String },
}

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let opt = Opt::from_args();

    let node_name_str = match opt {
        Opt::WeaveNode { .. } => "weave_node",
        Opt::FuchsiaNode => "fuchsia_node",
        Opt::WlanNode { .. } => "wlan_node",
        Opt::WpanNode { .. } => "wpan_node",
    };
    diagnostics_log::initialize(diagnostics_log::PublishOptions::default().tags(&[node_name_str]))?;

    match opt {
        Opt::WeaveNode { listen_addr_0, listen_addr_1 } => {
            run_server_node(
                vec![listen_addr_0, listen_addr_1],
                vec![2, 2],
                WEAVE_NODE_NAME,
                WEAVE_SERVER_NODE_DONE,
            )
            .await
            .context("Error running weave-node server")?;
            ()
        }
        Opt::FuchsiaNode => {
            run_fuchsia_node().await.context("Error running fuchsia-node")?;
        }
        Opt::WlanNode { connect_addr_0, connect_addr_1, connect_addr_2 } => {
            run_client_node(
                vec![connect_addr_0, connect_addr_1],
                WLAN_NODE_NAME,
                vec![WEAVE_NODE_NAME],
            )
            .await
            .context("Error running wlan-node client")?;
            run_client_node(vec![connect_addr_2], WLAN_NODE_1_NAME, vec![WPAN_SERVER_NODE_NAME])
                .await
                .context("Error running wlan-node client 1")?;
        }
        Opt::WpanNode { connect_addr_0, connect_addr_1, listen_addr_0 } => {
            run_client_node(
                vec![connect_addr_0, connect_addr_1],
                WPAN_NODE_NAME,
                vec![WEAVE_NODE_NAME],
            )
            .await
            .context("Error running wpan-node client")?;
            run_server_node(
                vec![listen_addr_0],
                vec![1],
                WPAN_SERVER_NODE_NAME,
                WPAN_SERVER_NODE_DONE,
            )
            .await
            .context("Error running wpan-node server")?;
        }
    };
    Ok(())
}
