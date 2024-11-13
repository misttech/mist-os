// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;

use async_utils::hanging_get::client::HangingGetStream;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_lowpan_experimental::Nat64Mapping;
use fuchsia_async::Task;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_inspect::{LazyNode, Node, StringProperty};
use fuchsia_inspect_contrib::inspect_log;
use fuchsia_inspect_contrib::nodes::{BoundedListNode, MonotonicTimeProperty, NodeTimeExt};
use fuchsia_sync::Mutex;
use std::collections::{HashMap, HashSet};

type IfaceId = String;

// Limit was chosen arbitrary.
const EVENTS_LIMIT: usize = 20;

pub struct LowpanServiceTree {
    // Root of the tree
    inspector: Inspector,

    // "events" subtree
    events: Mutex<BoundedListNode>,

    // "iface-<n>" subtrees, where n is the iface ID.
    ifaces_trees: Mutex<HashMap<IfaceId, Arc<IfaceTreeHolder>>>,

    // Iface devices that have been removed but whose debug infos are still kept in Inspect tree.
    dead_ifaces: Mutex<HashMap<IfaceId, Arc<IfaceTreeHolder>>>,
}

impl LowpanServiceTree {
    pub fn new(inspector: Inspector) -> Self {
        let events = inspector.root().create_child("events");
        Self {
            inspector,
            events: Mutex::new(BoundedListNode::new(events, EVENTS_LIMIT)),
            ifaces_trees: Mutex::new(HashMap::new()),
            dead_ifaces: Mutex::new(HashMap::new()),
        }
    }

    pub fn create_iface_child(&self, iface_id: IfaceId) -> Arc<IfaceTreeHolder> {
        // Check if this iface is already in |dead_ifaces|
        if let Some(prev_holder) = self.dead_ifaces.lock().remove(&iface_id) {
            self.ifaces_trees.lock().insert(iface_id, prev_holder.clone());
            prev_holder
        } else {
            let child = self.inspector.root().create_child(&format!("iface-{}", iface_id));
            let holder = Arc::new(IfaceTreeHolder::new(child));
            self.ifaces_trees.lock().insert(iface_id, holder.clone());
            holder
        }
    }

    pub fn notify_iface_removed(&self, iface_id: IfaceId) {
        let mut iface_tree_list = self.ifaces_trees.lock();
        let mut dead_iface_list = self.dead_ifaces.lock();
        if let Some(child_holder) = iface_tree_list.remove(&iface_id) {
            inspect_log!(child_holder.events.lock(), msg: "Removed");

            //Remove lazy child monitoring for these interfaces
            {
                let mut lazy_counters = child_holder.counters.lock();
                *lazy_counters = LazyNode::default();
                let mut lazy_neighbors = child_holder.neighbors.lock();
                *lazy_neighbors = LazyNode::default();
                let mut lazy_telemetry = child_holder.telemetry.lock();
                *lazy_telemetry = LazyNode::default();
            }

            dead_iface_list.insert(iface_id, child_holder.clone());
        }
    }
}

pub struct IfaceTreeHolder {
    node: Node,
    // "iface-*/events" subtree
    events: Mutex<BoundedListNode>,
    // "iface-*/status" subtree
    status: Mutex<IfaceStatusNode>,
    // "iface-*/counters" subtree
    counters: Mutex<LazyNode>,
    // "iface-*/neighbors" subtree
    neighbors: Mutex<LazyNode>,
    // "iface-*/telemetry" subtree
    telemetry: Mutex<LazyNode>,
}

impl IfaceTreeHolder {
    pub fn new(node: Node) -> Self {
        let events = node.create_child("events");
        let status = node.create_child("status");
        Self {
            node,
            events: Mutex::new(BoundedListNode::new(events, EVENTS_LIMIT)),
            status: Mutex::new(IfaceStatusNode::new(status)),
            counters: Mutex::new(LazyNode::default()),
            neighbors: Mutex::new(LazyNode::default()),
            telemetry: Mutex::new(LazyNode::default()),
        }
    }

    pub fn update_status(&self, new_state: DeviceState) {
        let new_connectivity_state = match new_state.connectivity_state {
            Some(state) => format!("{:?}", state),
            None => String::from(""),
        };

        let online_states: HashSet<String> =
            vec![String::from("Attaching"), String::from("Attached"), String::from("Isolated")]
                .into_iter()
                .collect();
        let mut status = self.status.lock();
        let mut status_change_messages = Vec::new();
        if online_states.contains(&new_connectivity_state)
            && !online_states.contains(&status.connectivity_state_value)
        {
            status._online_since = Some(status.node.create_time("online_since").property);
        } else if !online_states.contains(&new_connectivity_state) {
            status._online_since = None;
        }
        if new_connectivity_state != status.connectivity_state_value {
            status_change_messages.push(format!(
                "connectivity_state:{}->{}",
                status.connectivity_state_value, new_connectivity_state
            ));
            status.connectivity_state_value = new_connectivity_state.clone();
            status._connectivity_state =
                status.node.create_string("connectivity_state", new_connectivity_state);
        }

        let new_role = match new_state.role {
            Some(role) => format!("{:?}", role),
            None => String::from(""),
        };
        if new_role != status.role_value {
            status_change_messages.push(format!("role:{}->{}\n", status.role_value, new_role));
            status.role_value = new_role.clone();
            status._role = status.node.create_string("role", new_role);
        }
        let joined_status_messages = status_change_messages.join(";");
        if !joined_status_messages.is_empty() {
            inspect_log!(self.events.lock(), msg: joined_status_messages);
        }
    }

    pub fn update_identity(&self, new_identity: Identity) {
        let mut status = self.status.lock();
        let new_net_type = new_identity.net_type.unwrap_or_else(|| "".to_string());
        let mut status_change_messages = Vec::new();
        if new_net_type != status.net_type_value {
            status_change_messages
                .push(format!("net_type:{}->{}", status.net_type_value, new_net_type));
            status.net_type_value = new_net_type.clone();
            status._net_type = status.node.create_string("net_type", new_net_type);
        }

        let new_channel = match new_identity.channel {
            None => String::new(),
            Some(channel) => channel.to_string(),
        };
        if new_channel != status.channel_value {
            status_change_messages
                .push(format!("channel:{}->{}", status.channel_value, new_channel));

            status.channel_value = new_channel.clone();
            status._channel = status.node.create_string("channel", new_channel);
        }
        let joined_status_messages = status_change_messages.join(";");
        if !joined_status_messages.is_empty() {
            inspect_log!(self.events.lock(), msg: joined_status_messages);
        }
    }
}

pub struct IfaceStatusNode {
    // Iface state values.
    connectivity_state_value: String,
    role_value: String,
    channel_value: String,
    net_type_value: String,

    node: Node,
    // Properties of "iface-*/status" node.
    _connectivity_state: StringProperty,
    _online_since: Option<MonotonicTimeProperty>,
    _role: StringProperty,
    _channel: StringProperty,
    _net_type: StringProperty,
}
impl IfaceStatusNode {
    pub fn new(node: Node) -> Self {
        Self {
            connectivity_state_value: String::from(""),
            role_value: String::from(""),
            channel_value: String::from(""),
            net_type_value: String::from(""),
            node,
            _connectivity_state: StringProperty::default(),
            _online_since: None,
            _role: StringProperty::default(),
            _channel: StringProperty::default(),
            _net_type: StringProperty::default(),
        }
    }
}

pub async fn watch_device_changes<
    LP: 'static
        + DeviceWatcherProxyInterface<
            WatchDevicesResponseFut = fidl::client::QueryResponseFut<(Vec<String>, Vec<String>)>,
        >,
>(
    inspect_tree: Arc<LowpanServiceTree>,
    lookup: Arc<LP>,
) {
    let mut device_table: HashMap<String, Arc<Task<()>>> = HashMap::new();
    let lookup_clone = lookup.clone();
    let mut lookup_stream = HangingGetStream::new(lookup_clone, |lookup| lookup.watch_devices());
    loop {
        match lookup_stream.next().await {
            None => {
                debug!("LoWPAN device lookup stream finished");
                break;
            }
            Some(Ok(devices)) => {
                for available_device in devices.0.iter() {
                    inspect_log!(
                        inspect_tree.events.lock(),
                        msg: format!("{}:available", available_device)
                    );
                    let future = monitor_device(
                        available_device.clone().to_string(),
                        inspect_tree.create_iface_child(available_device.clone()),
                    )
                    .map(|x| match x {
                        Ok(()) => {}
                        Err(err) => {
                            warn!("Failed to monitor LoWPAN device: {:?}", err);
                        }
                    });

                    device_table
                        .insert(available_device.to_string(), Arc::new(Task::spawn(future)));
                }
                for unavailable_device in devices.1.iter() {
                    inspect_log!(
                        inspect_tree.events.lock(),
                        msg: format!("{}:unavailable", unavailable_device)
                    );
                    device_table.remove(unavailable_device);
                    inspect_tree.notify_iface_removed(unavailable_device.to_string());
                }
            }
            Some(Err(e)) => {
                warn!("LoWPAN device lookup stream returned err: {} ", e);
                break;
            }
        }
    }
}

pub async fn start_inspect_process(inspect_tree: Arc<LowpanServiceTree>) -> Result<(), Error> {
    let lookup = connect_to_protocol::<DeviceWatcherMarker>()?;
    watch_device_changes(inspect_tree, Arc::new(lookup)).await;
    Ok::<(), Error>(())
}

fn record_nat64_mapping_in_inspect_node(nat64_mapping_node: &Node, mapping: Nat64Mapping) {
    if let Some(x) = mapping.mapping_id {
        nat64_mapping_node.record_uint("mapping_id", x.into());
    }
    if let Some(x) = mapping.ip4_addr {
        nat64_mapping_node.record_bytes("ip4_addr", x);
    }
    if let Some(x) = mapping.ip6_addr {
        nat64_mapping_node.record_bytes("ip6_addr", x);
    }
    if let Some(x) = mapping.remaining_time_ms {
        nat64_mapping_node.record_uint("remaining_time_ms", x.into());
    }
    if let Some(x) = mapping.counters {
        nat64_mapping_node.record_child("counters", |nat64_mapping_counters_node| {
            let mapping_counters_list =
                [("tcp", x.tcp), ("udp", x.udp), ("icmp", x.icmp), ("total", x.total)];
            for (counter_name, counter) in mapping_counters_list {
                nat64_mapping_counters_node.record_child(counter_name, |per_counter_node| {
                    let packet_counter = counter.unwrap();
                    if let Some(y) = packet_counter.ipv4_to_ipv6_packets {
                        per_counter_node.record_uint("ipv4_to_ipv6_packets", y.into());
                    }
                    if let Some(y) = packet_counter.ipv4_to_ipv6_bytes {
                        per_counter_node.record_uint("ipv4_to_ipv6_bytes", y.into());
                    }
                    if let Some(y) = packet_counter.ipv6_to_ipv4_packets {
                        per_counter_node.record_uint("ipv6_to_ipv4_packets", y.into());
                    }
                    if let Some(y) = packet_counter.ipv6_to_ipv4_bytes {
                        per_counter_node.record_uint("ipv6_to_ipv4_bytes", y.into());
                    }
                });
            }
        });
    }
}

async fn monitor_device(name: String, iface_tree: Arc<IfaceTreeHolder>) -> Result<(), Error> {
    let (device_client, device_server) = create_endpoints::<DeviceMarker>();
    let (device_extra_client, device_extra_server) = create_endpoints::<DeviceExtraMarker>();
    let (device_test_client, device_test_server) = create_endpoints::<DeviceTestMarker>();
    let (counters_client, counters_server) = create_endpoints::<CountersMarker>();
    let (telemetry_client, telemetry_server) = create_endpoints::<TelemetryProviderMarker>();

    connect_to_protocol::<DeviceConnectorMarker>()?.connect(&name, device_server)?;
    connect_to_protocol::<DeviceExtraConnectorMarker>()?.connect(&name, device_extra_server)?;
    connect_to_protocol::<DeviceTestConnectorMarker>()?.connect(&name, device_test_server)?;
    connect_to_protocol::<CountersConnectorMarker>()?.connect(&name, counters_server)?;
    connect_to_protocol::<TelemetryProviderConnectorMarker>()?.connect(&name, telemetry_server)?;

    let device = device_client.into_proxy().context("into_proxy() failed")?;
    let device_extra = device_extra_client.into_proxy().context("into_proxy() failed")?;
    let device_test = device_test_client.into_proxy().context("into_proxy() failed")?;
    let counters = counters_client.into_proxy().context("into_proxy() failed")?;
    let telemetry = telemetry_client.into_proxy().context("into_proxy() failed")?;

    {
        // "iface-*/counters" node
        let mut lazy_counters = iface_tree.counters.lock();
        *lazy_counters = iface_tree.node.create_lazy_child("counters", move || {
            let counters_clone = counters.clone();
            async move {
                let inspector = Inspector::default();
                match counters_clone.get().await {
                    Ok(all_counters) => {
                        let mac_counters =
                            [("tx", all_counters.mac_tx), ("rx", all_counters.mac_rx)];

                        for (mac_counter_for_str, mac_counter_option) in mac_counters {
                            if let Some(mac_counter) = mac_counter_option {
                                inspector.root().record_int(
                                    format!("{}_frames", mac_counter_for_str),
                                    mac_counter.total.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_unicast", mac_counter_for_str),
                                    mac_counter.unicast.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_broadcast", mac_counter_for_str),
                                    mac_counter.broadcast.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_ack_requested", mac_counter_for_str),
                                    mac_counter.ack_requested.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_acked", mac_counter_for_str),
                                    mac_counter.acked.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_no_ack_requested", mac_counter_for_str),
                                    mac_counter.no_ack_requested.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_data", mac_counter_for_str),
                                    mac_counter.data.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_data_poll", mac_counter_for_str),
                                    mac_counter.data_poll.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_beacon", mac_counter_for_str),
                                    mac_counter.beacon.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_beacon_request", mac_counter_for_str),
                                    mac_counter.beacon_request.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_other", mac_counter_for_str),
                                    mac_counter.other.unwrap_or(0).into(),
                                );
                                inspector.root().record_int(
                                    format!("{}_address_filtered", mac_counter_for_str),
                                    mac_counter.address_filtered.unwrap_or(0).into(),
                                );

                                if mac_counter_for_str == "tx" {
                                    inspector.root().record_int(
                                        format!("{}_retries", mac_counter_for_str),
                                        mac_counter.retries.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_direct_max_retry_expiry", mac_counter_for_str),
                                        mac_counter.direct_max_retry_expiry.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!(
                                            "{}_indirect_max_retry_expiry",
                                            mac_counter_for_str
                                        ),
                                        mac_counter.indirect_max_retry_expiry.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_cca", mac_counter_for_str),
                                        mac_counter.err_cca.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_abort", mac_counter_for_str),
                                        mac_counter.err_abort.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_busy_channel", mac_counter_for_str),
                                        mac_counter.err_busy_channel.unwrap_or(0).into(),
                                    );
                                } else {
                                    inspector.root().record_int(
                                        format!("{}_dest_addr_filtered", mac_counter_for_str),
                                        mac_counter.dest_addr_filtered.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_duplicated", mac_counter_for_str),
                                        mac_counter.duplicated.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_no_frame", mac_counter_for_str),
                                        mac_counter.err_no_frame.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_unknown_neighbor", mac_counter_for_str),
                                        mac_counter.err_unknown_neighbor.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_invalid_src_addr", mac_counter_for_str),
                                        mac_counter.err_invalid_src_addr.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_sec", mac_counter_for_str),
                                        mac_counter.err_sec.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        format!("{}_err_fcs", mac_counter_for_str),
                                        mac_counter.err_fcs.unwrap_or(0).into(),
                                    );
                                }

                                inspector.root().record_int(
                                    format!("{}_err_other", mac_counter_for_str),
                                    mac_counter.err_other.unwrap_or(0).into(),
                                );
                            }
                        }

                        // Log coex counters
                        let coex_counters =
                            [("tx", all_counters.coex_tx), ("rx", all_counters.coex_rx)];
                        inspector.root().record_child("coex_counters", |coex_counters_child| {
                            for (coex_counter_for_str, coex_counter_option) in coex_counters {
                                if let Some(coex_counter) = coex_counter_option {
                                    if let Some(val) = coex_counter.requests {
                                        coex_counters_child.record_uint(
                                            format!("{}_requests", coex_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.grant_immediate {
                                        coex_counters_child.record_uint(
                                            format!("{}_grant_immediate", coex_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.grant_wait {
                                        coex_counters_child.record_uint(
                                            format!("{}_grant_wait", coex_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.grant_wait_activated {
                                        coex_counters_child.record_uint(
                                            format!(
                                                "{}_grant_wait_activated",
                                                coex_counter_for_str
                                            ),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.grant_wait_timeout {
                                        coex_counters_child.record_uint(
                                            format!("{}_grant_wait_timeout", coex_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.grant_deactivated_during_request
                                    {
                                        coex_counters_child.record_uint(
                                            format!(
                                                "{}_grant_deactivated_during_request",
                                                coex_counter_for_str
                                            ),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.delayed_grant {
                                        coex_counters_child.record_uint(
                                            format!("{}_delayed_grant", coex_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.avg_delay_request_to_grant_usec
                                    {
                                        coex_counters_child.record_uint(
                                            format!(
                                                "{}_avg_delay_request_to_grant_usec",
                                                coex_counter_for_str
                                            ),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = coex_counter.grant_none {
                                        coex_counters_child.record_uint(
                                            format!("{}_grant_none", coex_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                }
                            }
                        });
                        if let Some(val) = all_counters.coex_saturated {
                            inspector.root().record_bool("coex_saturated", val.into());
                        }
                        // Log ip counters
                        let ip_counters = [("tx", all_counters.ip_tx), ("rx", all_counters.ip_rx)];
                        inspector.root().record_child("ip_counters", |ip_counters_child| {
                            for (ip_counter_for_str, ip_counter_option) in ip_counters {
                                if let Some(ip_counter) = ip_counter_option {
                                    if let Some(val) = ip_counter.success {
                                        ip_counters_child.record_uint(
                                            format!("{}_success", ip_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                    if let Some(val) = ip_counter.failure {
                                        ip_counters_child.record_uint(
                                            format!("{}_failure", ip_counter_for_str),
                                            val.into(),
                                        );
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => {
                        warn!("Error in logging counters. Error: {}", e);
                    }
                };
                Ok(inspector)
            }
            .boxed()
        });

        // "iface-*/neighbors" node
        let mut lazy_neighbors = iface_tree.neighbors.lock();
        *lazy_neighbors = iface_tree.node.create_lazy_child("neighbors", move || {
            let device_test_clone = device_test.clone();
            async move {
                let inspector = Inspector::default();
                match device_test_clone.get_neighbor_table().await {
                    Ok(neighbor_table) => {
                        let mut index = -1;
                        for neighbor_info in neighbor_table {
                            let neighbor_info_c = neighbor_info.clone();
                            index += 1;
                            inspector.root().record_lazy_child(format!("{}", index), move || {
                                let neighbor_info_clone = neighbor_info_c.clone();
                                async move {
                                    let inspector = Inspector::default();
                                    inspector.root().record_int(
                                        "short_address",
                                        neighbor_info_clone.short_address.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "age",
                                        neighbor_info_clone
                                            .age
                                            .unwrap_or(0)
                                            .try_into()
                                            .unwrap_or(0),
                                    );
                                    inspector.root().record_bool(
                                        "is_child",
                                        neighbor_info_clone.is_child.unwrap_or(false).into(),
                                    );
                                    inspector.root().record_uint(
                                        "link_frame_count",
                                        neighbor_info_clone.link_frame_count.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "mgmt_frame_count",
                                        neighbor_info_clone.mgmt_frame_count.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        "rssi",
                                        neighbor_info_clone.last_rssi_in.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_int(
                                        "avg_rssi_in",
                                        neighbor_info_clone.avg_rssi_in.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "lqi_in",
                                        neighbor_info_clone.lqi_in.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "thread_mode",
                                        neighbor_info_clone.thread_mode.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "frame_error_rate",
                                        neighbor_info_clone.frame_error_rate.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "ipv6_error_rate",
                                        neighbor_info_clone.ipv6_error_rate.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_bool(
                                        "child_is_csl_synced",
                                        neighbor_info_clone
                                            .child_is_csl_synced
                                            .unwrap_or(false)
                                            .into(),
                                    );
                                    inspector.root().record_bool(
                                        "child_is_state_restoring",
                                        neighbor_info_clone
                                            .child_is_state_restoring
                                            .unwrap_or(false)
                                            .into(),
                                    );
                                    inspector.root().record_uint(
                                        "net_data_version",
                                        neighbor_info_clone.net_data_version.unwrap_or(0).into(),
                                    );
                                    inspector.root().record_uint(
                                        "queued_messages",
                                        neighbor_info_clone.queued_messages.unwrap_or(0).into(),
                                    );
                                    Ok(inspector)
                                }
                                .boxed()
                            });
                        }
                    }
                    Err(e) => {
                        warn!("Error in logging neighbors. Error: {}", e);
                    }
                };
                Ok(inspector)
            }
            .boxed()
        });

        // "iface-*/telemetry" node
        let mut lazy_telemetry = iface_tree.telemetry.lock();
        *lazy_telemetry = iface_tree.node.create_lazy_child("telemetry", move || {
            let telemetry_clone = telemetry.clone();
            async move {
                let inspector = Inspector::default();
                match telemetry_clone.get_telemetry().await {
                    Ok(telemetry_data) => {
                        if let Some(x) = telemetry_data.thread_router_id {
                            inspector.root().record_uint("thread_router_id", x.into());
                        }
                        if let Some(x) = telemetry_data.thread_rloc {
                            inspector.root().record_uint("thread_rloc", x.into());
                        }
                        if let Some(x) = telemetry_data.partition_id {
                            inspector.root().record_uint("partition_id", x.into());
                        }
                        if let Some(x) = telemetry_data.tx_power {
                            inspector.root().record_int("tx_power", x.into());
                        }
                        if let Some(x) = telemetry_data.thread_link_mode {
                            inspector.root().record_uint("thread_link_mode", x.into());
                        }
                        if let Some(x) = telemetry_data.thread_network_data_version {
                            inspector.root().record_uint("thread_network_data_version", x.into());
                        }
                        if let Some(x) = telemetry_data.thread_stable_network_data_version {
                            inspector
                                .root()
                                .record_uint("thread_stable_network_data_version", x.into());
                        }
                        if let Some(x) = telemetry_data.thread_network_data {
                            inspector.root().record_bytes("thread_network_data", x);
                        }
                        if let Some(x) = telemetry_data.thread_stable_network_data {
                            inspector.root().record_bytes("thread_stable_network_data", x);
                        }
                        if let Some(x) = telemetry_data.stack_version {
                            inspector.root().record_string("stack_version", x);
                        }
                        if let Some(x) = telemetry_data.rcp_version {
                            inspector.root().record_string("rcp_version", x);
                        }
                        if let Some(x) = telemetry_data.rssi {
                            inspector.root().record_int("rssi", x.into());
                        }
                        if let Some(x) = telemetry_data.dnssd_counters {
                            inspector.root().record_child(
                                "dnssd_counters",
                                |dnssd_counters_child| {
                                    if let Some(y) = x.success_response {
                                        dnssd_counters_child
                                            .record_uint("success_response", y.into());
                                    }
                                    if let Some(y) = x.server_failure_response {
                                        dnssd_counters_child
                                            .record_uint("server_failure_response", y.into());
                                    }
                                    if let Some(y) = x.format_error_response {
                                        dnssd_counters_child
                                            .record_uint("format_error_response", y.into());
                                    }
                                    if let Some(y) = x.name_error_response {
                                        dnssd_counters_child
                                            .record_uint("name_error_response", y.into());
                                    }
                                    if let Some(y) = x.not_implemented_response {
                                        dnssd_counters_child
                                            .record_uint("not_implemented_response", y.into());
                                    }
                                    if let Some(y) = x.other_response {
                                        dnssd_counters_child
                                            .record_uint("other_response", y.into());
                                    }
                                    if let Some(y) = x.resolved_by_srp {
                                        dnssd_counters_child
                                            .record_uint("resolved_by_srp", y.into());
                                    }
                                    if let Some(y) = x.upstream_dns_counters {
                                        dnssd_counters_child.record_child(
                                            "upstream_dns_counters",
                                            |upstream_dns_counters_node| {
                                                if let Some(z) = y.queries {
                                                    upstream_dns_counters_node
                                                        .record_uint("queries", z.into());
                                                }
                                                if let Some(z) = y.responses {
                                                    upstream_dns_counters_node
                                                        .record_uint("responses", z.into());
                                                }
                                                if let Some(z) = y.failures {
                                                    upstream_dns_counters_node
                                                        .record_uint("failures", z.into());
                                                }
                                            },
                                        );
                                    }
                                },
                            );
                        }
                        if let Some(x) = telemetry_data.thread_border_routing_counters {
                            inspector.root().record_child(
                                "border_routing_counters",
                                |border_routing_counters_child| {
                                    border_routing_counters_child.record_child(
                                        "inbound_unicast",
                                        |counter_node| {
                                            if let Some(y) = x.inbound_unicast_packets {
                                                counter_node.record_uint("packets", y);
                                            }
                                            if let Some(y) = x.inbound_unicast_bytes {
                                                counter_node.record_uint("bytes", y);
                                            }
                                        },
                                    );
                                    border_routing_counters_child.record_child(
                                        "inbound_multicast",
                                        |counter_node| {
                                            if let Some(y) = x.inbound_multicast_packets {
                                                counter_node.record_uint("packets", y);
                                            }
                                            if let Some(y) = x.inbound_multicast_bytes {
                                                counter_node.record_uint("bytes", y);
                                            }
                                        },
                                    );
                                    border_routing_counters_child.record_child(
                                        "outbound_unicast",
                                        |counter_node| {
                                            if let Some(y) = x.outbound_unicast_packets {
                                                counter_node.record_uint("packets", y);
                                            }
                                            if let Some(y) = x.outbound_unicast_bytes {
                                                counter_node.record_uint("bytes", y);
                                            }
                                        },
                                    );
                                    border_routing_counters_child.record_child(
                                        "outbound_multicast",
                                        |counter_node| {
                                            if let Some(y) = x.outbound_multicast_packets {
                                                counter_node.record_uint("packets", y);
                                            }
                                            if let Some(y) = x.outbound_multicast_bytes {
                                                counter_node.record_uint("bytes", y);
                                            }
                                        },
                                    );
                                    if let Some(y) = x.ra_rx {
                                        border_routing_counters_child
                                            .record_uint("ra_rx", y.into());
                                    }
                                    if let Some(y) = x.ra_tx_success {
                                        border_routing_counters_child
                                            .record_uint("ra_tx_success", y.into());
                                    }
                                    if let Some(y) = x.ra_tx_failure {
                                        border_routing_counters_child
                                            .record_uint("ra_tx_failure", y.into());
                                    }
                                    if let Some(y) = x.rs_rx {
                                        border_routing_counters_child
                                            .record_uint("rs_rx", y.into());
                                    }
                                    if let Some(y) = x.rs_tx_success {
                                        border_routing_counters_child
                                            .record_uint("rs_tx_success", y.into());
                                    }
                                    if let Some(y) = x.rs_tx_failure {
                                        border_routing_counters_child
                                            .record_uint("rs_tx_failure", y.into());
                                    }
                                    if let Some(y) = x.inbound_internet_packets {
                                        border_routing_counters_child
                                            .record_uint("inbound_internet_packets", y.into());
                                    }
                                    if let Some(y) = x.inbound_internet_bytes {
                                        border_routing_counters_child
                                            .record_uint("inbound_internet_bytes", y.into());
                                    }
                                    if let Some(y) = x.outbound_internet_packets {
                                        border_routing_counters_child
                                            .record_uint("outbound_internet_packets", y.into());
                                    }
                                    if let Some(y) = x.outbound_internet_bytes {
                                        border_routing_counters_child
                                            .record_uint("outbound_internet_bytes", y.into());
                                    }
                                },
                            );
                        }
                        if let Some(x) = telemetry_data.srp_server_info {
                            inspector.root().record_child("srp_server", |srp_server_info_child| {
                                if let Some(y) = x.state {
                                    srp_server_info_child
                                        .record_string("state", format!("{:?}", y));
                                }
                                if let Some(y) = x.port {
                                    srp_server_info_child.record_uint("port", y.into());
                                }
                                if let Some(y) = x.address_mode {
                                    srp_server_info_child
                                        .record_string("address_mode", format!("{:?}", y));
                                }
                                if let Some(y) = x.response_counters {
                                    srp_server_info_child.record_child(
                                        "response_counters",
                                        |response_counters_child| {
                                            if let Some(z) = y.success_response {
                                                response_counters_child
                                                    .record_uint("success_response", z.into());
                                            }
                                            if let Some(z) = y.server_failure_response {
                                                response_counters_child.record_uint(
                                                    "server_failure_response",
                                                    z.into(),
                                                );
                                            }
                                            if let Some(z) = y.format_error_response {
                                                response_counters_child
                                                    .record_uint("format_error_response", z.into());
                                            }
                                            if let Some(z) = y.name_exists_response {
                                                response_counters_child
                                                    .record_uint("name_exists_response", z.into());
                                            }
                                            if let Some(z) = y.refused_response {
                                                response_counters_child
                                                    .record_uint("refused_response", z.into());
                                            }
                                            if let Some(z) = y.other_response {
                                                response_counters_child
                                                    .record_uint("other_response", z.into());
                                            }
                                        },
                                    );
                                }
                                if let Some(y) = x.hosts_registration {
                                    srp_server_info_child.record_child(
                                        "hosts_registration",
                                        |hosts_registration_child| {
                                            if let Some(z) = y.fresh_count {
                                                hosts_registration_child
                                                    .record_uint("fresh_count", z.into());
                                            }
                                            if let Some(z) = y.deleted_count {
                                                hosts_registration_child
                                                    .record_uint("deleted_count", z.into());
                                            }
                                            if let Some(z) = y.lease_time_total {
                                                if let Ok(t) = u64::try_from(z) {
                                                    hosts_registration_child
                                                        .record_uint("lease_time_total", t);
                                                }
                                            }
                                            if let Some(z) = y.key_lease_time_total {
                                                if let Ok(t) = u64::try_from(z) {
                                                    hosts_registration_child
                                                        .record_uint("key_lease_time_total", t);
                                                }
                                            }
                                            if let Some(z) = y.remaining_lease_time_total {
                                                if let Ok(t) = u64::try_from(z) {
                                                    hosts_registration_child.record_uint(
                                                        "remaining_lease_time_total",
                                                        t,
                                                    );
                                                }
                                            }
                                            if let Some(z) = y.remaining_key_lease_time_total {
                                                if let Ok(t) = u64::try_from(z) {
                                                    hosts_registration_child.record_uint(
                                                        "remaining_key_lease_time_total",
                                                        t,
                                                    );
                                                }
                                            }
                                        },
                                    );
                                }
                                if let Some(y) = x.services_registration {
                                    srp_server_info_child.record_child(
                                        "services_registration",
                                        |services_registration_child| {
                                            if let Some(z) = y.fresh_count {
                                                services_registration_child
                                                    .record_uint("fresh_count", z.into());
                                            }
                                            if let Some(z) = y.deleted_count {
                                                services_registration_child
                                                    .record_uint("deleted_count", z.into());
                                            }
                                            if let Some(z) = y.lease_time_total {
                                                services_registration_child
                                                    .record_int("lease_time_total", z);
                                            }
                                            if let Some(z) = y.key_lease_time_total {
                                                services_registration_child
                                                    .record_int("key_lease_time_total", z);
                                            }
                                            if let Some(z) = y.remaining_lease_time_total {
                                                services_registration_child
                                                    .record_int("remaining_lease_time_total", z);
                                            }
                                            if let Some(z) = y.remaining_key_lease_time_total {
                                                services_registration_child.record_int(
                                                    "remaining_key_lease_time_total",
                                                    z,
                                                );
                                            }
                                        },
                                    );
                                }
                            });
                        }
                        if let Some(x) = telemetry_data.leader_data {
                            inspector.root().record_child("leader_data", |leader_data_child| {
                                if let Some(y) = x.partition_id {
                                    leader_data_child.record_uint("partition_id", y.into());
                                }
                                if let Some(y) = x.weight {
                                    leader_data_child.record_uint("weight", y.into());
                                    inspector.root().record_uint("thread_leader_weight", y.into());
                                }
                                if let Some(y) = x.network_data_version {
                                    leader_data_child.record_uint("network_data_version", y.into());
                                }
                                if let Some(y) = x.stable_network_data_version {
                                    leader_data_child
                                        .record_uint("stable_network_data_version", y.into());
                                }
                                if let Some(y) = x.router_id {
                                    leader_data_child.record_uint("router_id", y.into());
                                    inspector
                                        .root()
                                        .record_uint("thread_leader_router_id", y.into());
                                }
                            });
                        }
                        if let Some(x) = telemetry_data.uptime {
                            inspector.root().record_int("uptime", x.into());
                        }
                        if let Some(x) = telemetry_data.trel_counters {
                            inspector.root().record_child("trel_counters", |trel_counters_child| {
                                if let Some(y) = x.rx_bytes {
                                    trel_counters_child.record_uint("rx_bytes", y.into());
                                }
                                if let Some(y) = x.rx_packets {
                                    trel_counters_child.record_uint("rx_packets", y.into());
                                }
                                if let Some(y) = x.tx_bytes {
                                    trel_counters_child.record_uint("tx_bytes", y.into());
                                }
                                if let Some(y) = x.tx_failure {
                                    trel_counters_child.record_uint("tx_failure", y.into());
                                }
                                if let Some(y) = x.tx_packets {
                                    trel_counters_child.record_uint("tx_packets", y.into());
                                }
                            });
                        }
                        if let Some(x) = telemetry_data.nat64_info {
                            inspector.root().record_child("nat64_info", |nat64_info_child| {
                                if let Some(y) = x.nat64_state {
                                    nat64_info_child.record_child(
                                        "nat64_state",
                                        |nat64_state_node| {
                                            if let Some(z) = y.prefix_manager_state {
                                                nat64_state_node.record_string(
                                                    "prefix_manager_state",
                                                    format!("{:?}", z),
                                                );
                                            }
                                            if let Some(z) = y.translator_state {
                                                nat64_state_node.record_string(
                                                    "translator_state",
                                                    format!("{:?}", z),
                                                );
                                            }
                                        },
                                    );
                                }
                                if let Some(y) = x.nat64_mappings {
                                    nat64_info_child.record_child(
                                        "nat64_mappings",
                                        |nat64_mappings_child| {
                                            for (index, nat64_mapping) in y.iter().enumerate() {
                                                nat64_mappings_child.record_child(
                                                    format!("nat64_mapping_{}", index),
                                                    |nat64_mapping_node| {
                                                        record_nat64_mapping_in_inspect_node(
                                                            nat64_mapping_node,
                                                            nat64_mapping.clone(),
                                                        );
                                                    },
                                                );
                                            }
                                        },
                                    );
                                }
                                if let Some(y) = x.nat64_error_counters {
                                    nat64_info_child.record_child(
                                        "nat64_error_counters",
                                        |nat64_error_counters_child| {
                                            let error_counters_list = [
                                                ("unknown", y.unknown),
                                                ("illegal_packet", y.illegal_packet),
                                                ("unsupported_protocol", y.unsupported_protocol),
                                                ("no_mapping", y.no_mapping),
                                            ];
                                            for (counter_name, counter) in error_counters_list {
                                                nat64_error_counters_child.record_child(
                                                    counter_name,
                                                    |per_counter_node| {
                                                        let packet_counter = counter.unwrap();
                                                        if let Some(z) =
                                                            packet_counter.ipv4_to_ipv6_packets
                                                        {
                                                            per_counter_node.record_uint(
                                                                "ipv4_to_ipv6_packets",
                                                                z.into(),
                                                            );
                                                        }
                                                        if let Some(z) =
                                                            packet_counter.ipv6_to_ipv4_packets
                                                        {
                                                            per_counter_node.record_uint(
                                                                "ipv6_to_ipv4_packets",
                                                                z.into(),
                                                            );
                                                        }
                                                    },
                                                );
                                            }
                                        },
                                    );
                                }
                                if let Some(y) = x.nat64_protocol_counters {
                                    nat64_info_child.record_child(
                                        "nat64_protocol_counters",
                                        |nat64_protocol_counters_node| {
                                            let mapping_counters_list = [
                                                ("tcp", y.tcp),
                                                ("udp", y.udp),
                                                ("icmp", y.icmp),
                                                ("total", y.total),
                                            ];
                                            for (counter_name, counter) in mapping_counters_list {
                                                nat64_protocol_counters_node.record_child(
                                                    counter_name,
                                                    |per_counter_node| {
                                                        if let Some(packet_counter) = counter {
                                                            if let Some(z) =
                                                                packet_counter.ipv4_to_ipv6_packets
                                                            {
                                                                per_counter_node.record_uint(
                                                                    "ipv4_to_ipv6_packets",
                                                                    z.into(),
                                                                );
                                                            }
                                                            if let Some(z) =
                                                                packet_counter.ipv4_to_ipv6_bytes
                                                            {
                                                                per_counter_node.record_uint(
                                                                    "ipv4_to_ipv6_bytes",
                                                                    z.into(),
                                                                );
                                                            }
                                                            if let Some(z) =
                                                                packet_counter.ipv6_to_ipv4_packets
                                                            {
                                                                per_counter_node.record_uint(
                                                                    "ipv6_to_ipv4_packets",
                                                                    z.into(),
                                                                );
                                                            }
                                                            if let Some(z) =
                                                                packet_counter.ipv6_to_ipv4_bytes
                                                            {
                                                                per_counter_node.record_uint(
                                                                    "ipv6_to_ipv4_bytes",
                                                                    z.into(),
                                                                );
                                                            }
                                                        }
                                                    },
                                                );
                                            }
                                        },
                                    );
                                }
                            });
                        }
                        if let Some(x) = telemetry_data.trel_peers_info {
                            inspector.root().record_child("trel_peers_info", |trel_peers_node| {
                                if let Some(y) = x.num_trel_peers {
                                    trel_peers_node.record_uint("num_trel_peers", y.into());
                                }
                            });
                        }
                        if let Some(x) = telemetry_data.upstream_dns_info {
                            inspector.root().record_child(
                                "upstream_dns_info",
                                |upstream_dns_info_node| {
                                    if let Some(y) = x.upstream_dns_query_state {
                                        upstream_dns_info_node.record_string(
                                            "upstream_dns_query_state",
                                            format!("{:?}", y),
                                        );
                                    }
                                },
                            );
                        }
                        if let Some(x) = telemetry_data.dhcp6pd_info {
                            inspector.root().record_child("dhcp6pd_info", |dhcp6pd_info_node| {
                                if let Some(y) = x.dhcp6pd_state {
                                    dhcp6pd_info_node
                                        .record_string("dhcp6pd_state", format!("{:?}", y));
                                }
                                if let Some(y) = x.pd_processed_ra_info {
                                    dhcp6pd_info_node.record_child(
                                        "pd_processed_ra_info",
                                        |pd_processed_ra_info_node| {
                                            if let Some(z) = y.num_platform_ra_received {
                                                pd_processed_ra_info_node.record_uint(
                                                    "num_platform_ra_received",
                                                    z.into(),
                                                );
                                            }
                                            if let Some(z) = y.num_platform_pio_processed {
                                                pd_processed_ra_info_node.record_uint(
                                                    "num_platform_pio_processed",
                                                    z.into(),
                                                );
                                            }
                                            if let Some(z) = y.last_platform_ra_msec {
                                                pd_processed_ra_info_node
                                                    .record_uint("last_platform_ra_msec", z.into());
                                            }
                                        },
                                    );
                                }
                                if let Some(y) = x.hashed_pd_prefix {
                                    dhcp6pd_info_node.record_bytes("hashed_pd_prefix", y);
                                }
                            });
                        }
                        if let Some(x) = telemetry_data.link_metrics_entries {
                            inspector.root().record_child(
                                "link_metrics_entries",
                                |link_metrics_entries_child| {
                                    for (index, link_metrics_entry) in x.iter().enumerate() {
                                        link_metrics_entries_child.record_child(
                                            format!("link_metrics_entry_{}", index),
                                            |link_metrics_entry_node| {
                                                link_metrics_entry_node.record_uint(
                                                    "link_margin",
                                                    link_metrics_entry
                                                        .link_margin
                                                        .unwrap_or(0)
                                                        .into(),
                                                );
                                                link_metrics_entry_node.record_int(
                                                    "rssi",
                                                    link_metrics_entry.rssi.unwrap_or(0).into(),
                                                );
                                            },
                                        );
                                    }
                                },
                            )
                        }
                    }
                    Err(e) => {
                        warn!("Error in logging telemetry. Error: {}", e);
                    }
                };
                Ok(inspector)
            }
            .boxed()
        });
    }

    let mut device_stream_handler =
        HangingGetStream::new(device, |device| device.watch_device_state())
            .map_ok(|state| {
                iface_tree.update_status(state);
            })
            .try_collect::<()>();
    let mut device_extra_stream_handler =
        HangingGetStream::new(device_extra, |device| device.watch_identity())
            .map_ok(|identity| {
                iface_tree.update_identity(identity);
            })
            .try_collect::<()>();

    (futures::select! {
        ret = device_stream_handler => ret,
        ret = device_extra_stream_handler => ret,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fuchsia_async as fasync;
    use fuchsia_async::{MonotonicInstant, TimeoutExt};
    use fuchsia_component_test::ScopedInstanceFactory;

    #[fasync::run(4, test)]
    async fn test_watch_device_changes() {
        let lookup = connect_to_protocol::<DeviceWatcherMarker>().unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let inspector_clone = inspector.clone();
        let inspect_tree = Arc::new(LowpanServiceTree::new(inspector_clone));
        let look_up = Arc::new(lookup);

        assert_data_tree!(inspector, root: {
            "events":{},
        });

        // Start a lowpan dummy driver
        let dummy_driver = ScopedInstanceFactory::new("drivers")
            .new_instance("#meta/lowpan-dummy-driver.cm")
            .await
            .unwrap();
        dummy_driver.connect_to_binder().unwrap();

        let _res = watch_device_changes(inspect_tree.clone(), look_up.clone())
            .on_timeout(MonotonicInstant::after(zx::MonotonicDuration::from_seconds(5)), || {
                info!("test_watch_device_changes: watch_device_changes timed out");
            })
            .await;

        assert_data_tree!(inspector, root: {
            "events":{
                "0":{
                    "@time": AnyProperty,
                    "msg": "lowpan0:available"
                }
            },
            "iface-lowpan0":{
                "events": {
                    "0": {
                        "@time": AnyProperty,
                        "msg": "connectivity_state:->Ready;role:->Detached\n"
                    }
                },
                "status": {
                    "connectivity_state": "Ready",
                    "role": "Detached"
                },
                "counters": contains {
                    "rx_ack_requested": AnyProperty,
                    "rx_acked": AnyProperty,
                    "rx_address_filtered": AnyProperty,
                    "rx_beacon": AnyProperty,
                    "rx_beacon_request": AnyProperty,
                    "rx_broadcast": AnyProperty,
                    "rx_data": AnyProperty,
                    "rx_data_poll": AnyProperty,
                    "rx_dest_addr_filtered": AnyProperty,
                    "rx_duplicated": AnyProperty,
                    "rx_err_fcs": AnyProperty,
                    "rx_err_invalid_src_addr": AnyProperty,
                    "rx_err_no_frame": AnyProperty,
                    "rx_err_other": AnyProperty,
                    "rx_err_sec": AnyProperty,
                    "rx_err_unknown_neighbor": AnyProperty,
                    "rx_frames": AnyProperty,
                    "rx_no_ack_requested": AnyProperty,
                    "rx_other": AnyProperty,
                    "rx_unicast": AnyProperty,
                    "tx_ack_requested": AnyProperty,
                    "tx_acked": AnyProperty,
                    "tx_address_filtered": AnyProperty,
                    "tx_beacon": AnyProperty,
                    "tx_beacon_request": AnyProperty,
                    "tx_broadcast": AnyProperty,
                    "tx_data": AnyProperty,
                    "tx_data_poll": AnyProperty,
                    "tx_direct_max_retry_expiry": AnyProperty,
                    "tx_err_abort": AnyProperty,
                    "tx_err_busy_channel": AnyProperty,
                    "tx_err_cca": AnyProperty,
                    "tx_err_other": AnyProperty,
                    "tx_frames": AnyProperty,
                    "tx_indirect_max_retry_expiry": AnyProperty,
                    "tx_no_ack_requested": AnyProperty,
                    "tx_other": AnyProperty,
                    "tx_retries": AnyProperty,
                    "tx_unicast": AnyProperty,
                },
                "neighbors": {
                    "0": {
                        "age": AnyProperty,
                        "avg_rssi_in": AnyProperty,
                        "child_is_csl_synced": AnyProperty,
                        "child_is_state_restoring": AnyProperty,
                        "frame_error_rate": AnyProperty,
                        "ipv6_error_rate": AnyProperty,
                        "is_child": AnyProperty,
                        "link_frame_count": AnyProperty,
                        "lqi_in": AnyProperty,
                        "mgmt_frame_count": AnyProperty,
                        "net_data_version": AnyProperty,
                        "queued_messages": AnyProperty,
                        "rssi": AnyProperty,
                        "short_address": AnyProperty,
                        "thread_mode": AnyProperty,
                    }
                },
                "telemetry": contains {},
            }
        });
        assert_data_tree!(inspector, root: contains {
            "iface-lowpan0": contains {
                "counters": contains {
                    "coex_counters": {
                        "tx_requests": AnyProperty,
                        "tx_grant_immediate": AnyProperty,
                        "tx_grant_wait": AnyProperty,
                        "tx_grant_wait_activated": AnyProperty,
                        "tx_grant_wait_timeout": AnyProperty,
                        "tx_grant_deactivated_during_request": AnyProperty,
                        "tx_delayed_grant": AnyProperty,
                        "tx_avg_delay_request_to_grant_usec": AnyProperty,
                        "rx_requests": AnyProperty,
                        "rx_grant_immediate": AnyProperty,
                        "rx_grant_wait": AnyProperty,
                        "rx_grant_wait_activated": AnyProperty,
                        "rx_grant_wait_timeout": AnyProperty,
                        "rx_grant_deactivated_during_request": AnyProperty,
                        "rx_delayed_grant": AnyProperty,
                        "rx_avg_delay_request_to_grant_usec": AnyProperty,
                        "rx_grant_none": AnyProperty,
                    },
                    "coex_saturated": false,
                }
            }
        });
    }
}
