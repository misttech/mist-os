// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod inspect;
mod neighbor_cache;
mod ping;
pub mod watchdog;

use {
    crate::ping::Ping,
    fidl_fuchsia_hardware_network, fidl_fuchsia_net_ext as fnet_ext,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_stack as fnet_stack,
    fuchsia_async as fasync,
    fuchsia_inspect::Inspector,
    futures::{FutureExt as _, StreamExt as _},
    inspect::InspectInfo,
    itertools::Itertools,
    named_timer::DeadlineId,
    net_declare::{fidl_subnet, std_ip},
    net_types::ScopeableAddress as _,
    std::collections::hash_map::{Entry, HashMap},
    tracing::{debug, error, info},
};

pub use neighbor_cache::{InterfaceNeighborCache, NeighborCache};

const IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS: std::net::IpAddr = std_ip!("8.8.8.8");
const IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS: std::net::IpAddr = std_ip!("2001:4860:4860::8888");
const UNSPECIFIED_V4: fidl_fuchsia_net::Subnet = fidl_subnet!("0.0.0.0/0");
const UNSPECIFIED_V6: fidl_fuchsia_net::Subnet = fidl_subnet!("::0/0");

// Timeout ID for the fake clock component that restrains the integration tests from reaching the
// FIDL timeout and subsequently failing. Shared by the eventloop and integration library.
pub const FIDL_TIMEOUT_ID: DeadlineId<'static> =
    DeadlineId::new("reachability", "fidl-request-timeout");

/// `Stats` keeps the monitoring service statistic counters.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// `events` is the number of events received.
    pub events: u64,
    /// `state_updates` is the number of times reachability state has changed.
    pub state_updates: HashMap<Id, u64>,
}

// TODO(dpradilla): consider splitting the state in l2 state and l3 state, as there can be multiple
// L3 networks on the same physical medium.
/// `State` represents the reachability state.
#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Clone, Copy)]
pub enum State {
    /// State not yet determined.
    None,
    /// Interface no longer present.
    Removed,
    /// Interface is down.
    Down,
    /// Interface is up, no packets seen yet.
    Up,
    /// Interface is up, packets seen.
    LinkLayerUp,
    /// Interface is up, and configured as an L3 interface.
    NetworkLayerUp,
    /// L3 Interface is up, local neighbors seen.
    Local,
    /// L3 Interface is up, local gateway configured and reachable.
    Gateway,
    /// Expected response not seen from reachability test URL.
    WalledGarden,
    /// Expected response seen from reachability test URL.
    Internet,
}

impl Default for State {
    fn default() -> Self {
        State::None
    }
}

impl std::str::FromStr for State {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "None" => Ok(Self::None),
            "Removed" => Ok(Self::Removed),
            "Down" => Ok(Self::Down),
            "Up" => Ok(Self::Up),
            "LinkLayerUp" => Ok(Self::LinkLayerUp),
            "NetworkLayerUp" => Ok(Self::NetworkLayerUp),
            "Local" => Ok(Self::Local),
            "Gateway" => Ok(Self::Gateway),
            "WalledGarden" => Ok(Self::WalledGarden),
            "Internet" => Ok(Self::Internet),
            _ => Err(()),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
enum Proto {
    IPv4,
    IPv6,
}
impl std::fmt::Display for Proto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Proto::IPv4 => write!(f, "IPv4"),
            Proto::IPv6 => write!(f, "IPv6"),
        }
    }
}

/// `PortType` is the type of port backing the L3 interface.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PortType {
    /// Unknown.
    Unknown,
    /// EthernetII or 802.3.
    Ethernet,
    /// Wireless LAN based on 802.11.
    WiFi,
    /// Switch virtual interface.
    SVI,
    /// Loopback.
    Loopback,
}

impl From<fnet_interfaces::DeviceClass> for PortType {
    fn from(device_class: fnet_interfaces::DeviceClass) -> Self {
        match device_class {
            fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {}) => PortType::Loopback,
            fnet_interfaces::DeviceClass::Device(device_class) => match device_class {
                fidl_fuchsia_hardware_network::DeviceClass::Ethernet => PortType::Ethernet,
                fidl_fuchsia_hardware_network::DeviceClass::Wlan
                | fidl_fuchsia_hardware_network::DeviceClass::WlanAp => PortType::WiFi,
                fidl_fuchsia_hardware_network::DeviceClass::Ppp
                | fidl_fuchsia_hardware_network::DeviceClass::Bridge
                | fidl_fuchsia_hardware_network::DeviceClass::Virtual => PortType::Unknown,
            },
        }
    }
}

/// A trait for types containing reachability state that should be compared without the timestamp.
trait StateEq {
    /// Returns true iff `self` and `other` have equivalent reachability state.
    fn compare_state(&self, other: &Self) -> bool;
}

/// `StateEvent` records a state and the time it was reached.
// NB PartialEq is derived only for tests to avoid unintentionally making a comparison that
// includes the timestamp.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq))]
struct StateEvent {
    /// `state` is the current reachability state.
    state: State,
    /// The time of this event.
    time: fasync::Time,
}

impl StateEvent {
    /// Overwrite `self` with `other` if the state is different, returning the previous and current
    /// values (which may be equal).
    fn update(&mut self, other: Self) -> Delta<Self> {
        let previous = Some(*self);
        if self.state != other.state {
            *self = other;
        }
        Delta { previous, current: *self }
    }
}

impl StateEq for StateEvent {
    fn compare_state(&self, &Self { state, time: _ }: &Self) -> bool {
        self.state == state
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Delta<T> {
    current: T,
    previous: Option<T>,
}

impl<T: StateEq> Delta<T> {
    fn change_observed(&self) -> bool {
        match &self.previous {
            Some(previous) => !previous.compare_state(&self.current),
            None => true,
        }
    }
}

// NB PartialEq is derived only for tests to avoid unintentionally making a comparison that
// includes the timestamp in `StateEvent`.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
struct StateDelta {
    port: IpVersions<Delta<StateEvent>>,
    system: IpVersions<Delta<SystemState>>,
}

#[derive(Clone, Default, Debug, PartialEq)]
struct IpVersions<T> {
    ipv4: T,
    ipv6: T,
}

impl<T> IpVersions<T> {
    fn with_version<F: FnMut(Proto, &T)>(&self, mut f: F) {
        let () = f(Proto::IPv4, &self.ipv4);
        let () = f(Proto::IPv6, &self.ipv6);
    }
}

type Id = u64;

// NB PartialEq is derived only for tests to avoid unintentionally making a comparison that
// includes the timestamp in `StateEvent`.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
struct SystemState {
    id: Id,
    state: StateEvent,
}

impl SystemState {
    fn max(self, other: Self) -> Self {
        if other.state.state > self.state.state {
            other
        } else {
            self
        }
    }
}

impl StateEq for SystemState {
    fn compare_state(&self, &Self { id, state: StateEvent { state, time: _ } }: &Self) -> bool {
        self.id == id && self.state.state == state
    }
}

/// `StateInfo` keeps the reachability state.
// NB PartialEq is derived only for tests to avoid unintentionally making a comparison that
// includes the timestamp in `StateEvent`.
#[derive(Debug, Default, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct StateInfo {
    /// Mapping from interface ID to reachability information.
    per_interface: HashMap<Id, IpVersions<StateEvent>>,
    /// Interface IDs with the best reachability state per IP version.
    system: IpVersions<Option<Id>>,
}

impl StateInfo {
    /// Get the reachability info associated with an interface.
    fn get(&self, id: Id) -> Option<&IpVersions<StateEvent>> {
        self.per_interface.get(&id)
    }

    /// Get the system-wide IPv4 reachability info.
    fn get_system_ipv4(&self) -> Option<SystemState> {
        self.system.ipv4.map(|id| SystemState {
            id,
            state: self
                .get(id)
                .unwrap_or_else(|| {
                    panic!("inconsistent system IPv4 state: no interface with ID {:?}", id)
                })
                .ipv4,
        })
    }

    /// Get the system-wide IPv6 reachability info.
    fn get_system_ipv6(&self) -> Option<SystemState> {
        self.system.ipv6.map(|id| SystemState {
            id,
            state: self
                .get(id)
                .unwrap_or_else(|| {
                    panic!("inconsistent system IPv6 state: no interface with ID {:?}", id)
                })
                .ipv6,
        })
    }

    pub fn system_has_internet(&self) -> bool {
        self.system_has_state(&State::Internet)
    }

    // A system is considered to have gateway reachability if it has State::Gateway
    // or State::Internet.
    pub fn system_has_gateway(&self) -> bool {
        self.system_has_state(&State::Gateway) || self.system_has_state(&State::Internet)
    }

    fn system_has_state(&self, want_state: &State) -> bool {
        let v4_state = self.get_system_ipv4();
        let v6_state = self.get_system_ipv6();

        return [v4_state, v6_state]
            .iter()
            .filter_map(|state| state.as_ref())
            .any(|state| &state.state.state == want_state);
    }

    /// Report the duration of the current state for each interface and each protocol.
    fn report(&self) {
        let time = fasync::Time::now();
        debug!("system reachability state IPv4 {:?}", self.get_system_ipv4());
        debug!("system reachability state IPv6 {:?}", self.get_system_ipv6());
        for (id, IpVersions { ipv4, ipv6 }) in self.per_interface.iter() {
            debug!(
                "reachability state {:?} IPv4 {:?} with duration {:?}",
                id,
                ipv4,
                time - ipv4.time
            );
            debug!(
                "reachability state {:?} IPv6 {:?} with duration {:?}",
                id,
                ipv6,
                time - ipv6.time
            );
        }
    }

    /// Update interface `id` with its new reachability info.
    ///
    /// Returns the protocols and their new reachability states iff a change was observed.
    fn update(&mut self, id: Id, new_reachability: IpVersions<StateEvent>) -> StateDelta {
        let previous_system_ipv4 = self.get_system_ipv4();
        let previous_system_ipv6 = self.get_system_ipv6();
        let port = match self.per_interface.entry(id) {
            Entry::Occupied(mut occupied) => {
                let IpVersions { ipv4, ipv6 } = occupied.get_mut();
                let IpVersions { ipv4: new_ipv4, ipv6: new_ipv6 } = new_reachability;

                IpVersions { ipv4: ipv4.update(new_ipv4), ipv6: ipv6.update(new_ipv6) }
            }
            Entry::Vacant(vacant) => {
                let IpVersions { ipv4, ipv6 } = vacant.insert(new_reachability);
                IpVersions {
                    ipv4: Delta { previous: None, current: *ipv4 },
                    ipv6: Delta { previous: None, current: *ipv6 },
                }
            }
        };

        let IpVersions { ipv4: system_ipv4, ipv6: system_ipv6 } = self.per_interface.iter().fold(
            {
                let IpVersions {
                    ipv4: Delta { previous: _, current: ipv4 },
                    ipv6: Delta { previous: _, current: ipv6 },
                } = port;
                IpVersions {
                    ipv4: SystemState { id, state: ipv4 },
                    ipv6: SystemState { id, state: ipv6 },
                }
            },
            |IpVersions { ipv4: system_ipv4, ipv6: system_ipv6 },
             (&id, &IpVersions { ipv4, ipv6 })| {
                IpVersions {
                    ipv4: system_ipv4.max(SystemState { id, state: ipv4 }),
                    ipv6: system_ipv6.max(SystemState { id, state: ipv6 }),
                }
            },
        );

        self.system = IpVersions { ipv4: Some(system_ipv4.id), ipv6: Some(system_ipv6.id) };

        StateDelta {
            port,
            system: IpVersions {
                ipv4: Delta { previous: previous_system_ipv4, current: system_ipv4 },
                ipv6: Delta { previous: previous_system_ipv6, current: system_ipv6 },
            },
        }
    }
}

/// Provides a view into state for a specific system interface.
#[derive(Copy, Clone, Debug)]
pub struct InterfaceView<'a> {
    pub properties: &'a fnet_interfaces_ext::Properties,
    pub routes: &'a [fnet_stack::ForwardingEntry],
    pub neighbors: Option<&'a InterfaceNeighborCache>,
}

/// `Monitor` monitors the reachability state.
pub struct Monitor {
    state: StateInfo,
    stats: Stats,
    inspector: Option<&'static Inspector>,
    system_node: Option<InspectInfo>,
    nodes: HashMap<Id, InspectInfo>,
}

impl Monitor {
    /// Create the monitoring service.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Monitor {
            state: Default::default(),
            stats: Default::default(),
            inspector: None,
            system_node: None,
            nodes: HashMap::new(),
        })
    }

    pub fn state(&self) -> &StateInfo {
        &self.state
    }

    /// Reports all information.
    pub fn report_state(&self) {
        self.state.report();
        debug!("reachability stats {:?}", self.stats);
    }

    /// Sets the inspector.
    pub fn set_inspector(&mut self, inspector: &'static Inspector) {
        self.inspector = Some(inspector);

        let system_node = InspectInfo::new(inspector.root(), "system", "");
        self.system_node = Some(system_node);
    }

    fn interface_node(&mut self, id: Id, name: &str) -> Option<&mut InspectInfo> {
        self.inspector.map(move |inspector| {
            self.nodes.entry(id).or_insert_with_key(|id| {
                InspectInfo::new(inspector.root(), &format!("{:?}", id), name)
            })
        })
    }

    /// Update state based on the new reachability info.
    fn update_state(&mut self, id: Id, name: &str, reachability: IpVersions<StateEvent>) {
        let StateDelta { port, system } = self.state.update(id, reachability);

        let () = port.with_version(|proto, delta| {
            if delta.change_observed() {
                let &Delta { previous, current } = delta;
                if let Some(previous) = previous {
                    info!(
                        "interface updated {:?} {:?} current: {:?} previous: {:?}",
                        id, proto, current, previous
                    );
                } else {
                    info!("new interface {:?} {:?}: {:?}", id, proto, current);
                }
                let () = log_state(self.interface_node(id, name), proto, current.state);
                *self.stats.state_updates.entry(id).or_insert(0) += 1;
            }
        });

        let () = system.with_version(|proto, delta| {
            if delta.change_observed() {
                let &Delta { previous, current } = delta;
                if let Some(previous) = previous {
                    info!(
                        "system updated {:?} current: {:?}, previous: {:?}",
                        proto, current, previous,
                    );
                } else {
                    info!("initial system state {:?}: {:?}", proto, current);
                }
                let () = log_state(self.system_node.as_mut(), proto, current.state.state);
            }
        });
    }

    /// Compute the reachability state of an interface.
    ///
    /// The interface may have been recently-discovered, or the properties of a known interface may
    /// have changed.
    pub async fn compute_state(
        &mut self,
        InterfaceView { properties, routes, neighbors }: InterfaceView<'_>,
    ) {
        if let Some(info) = compute_state(properties, &routes, &ping::Pinger, neighbors).await {
            let id = Id::from(properties.id);
            let () = self.update_state(id, &properties.name, info);
        }
    }

    /// Handle an interface removed event.
    pub fn handle_interface_removed(
        &mut self,
        fnet_interfaces_ext::Properties { id, name, .. }: fnet_interfaces_ext::Properties,
    ) {
        let time = fasync::Time::now();
        if let Some(mut reachability) = self.state.get(id.into()).cloned() {
            reachability.ipv4 = StateEvent { state: State::Removed, time };
            reachability.ipv6 = StateEvent { state: State::Removed, time };
            let () = self.update_state(id.into(), &name, reachability);
        }
    }
}

fn log_state(info: Option<&mut InspectInfo>, proto: Proto, state: State) {
    info.into_iter().for_each(|info| info.log_state(proto, state))
}

/// `compute_state` processes an event and computes the reachability based on the event and
/// system observations.
async fn compute_state(
    &fnet_interfaces_ext::Properties {
        id,
        ref name,
        device_class,
        online,
        addresses: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    }: &fnet_interfaces_ext::Properties,
    routes: &[fnet_stack::ForwardingEntry],
    pinger: &dyn Ping,
    neighbors: Option<&InterfaceNeighborCache>,
) -> Option<IpVersions<StateEvent>> {
    if PortType::from(device_class) == PortType::Loopback {
        return None;
    }

    if !online {
        return Some(IpVersions {
            ipv4: StateEvent { state: State::Down, time: fasync::Time::now() },
            ipv6: StateEvent { state: State::Down, time: fasync::Time::now() },
        });
    }

    // TODO(https://fxbug.dev/74517) Check if packet count has increased, and if so upgrade the
    // state to LinkLayerUp.

    let (ipv4, ipv6) = futures::join!(
        network_layer_state(
            &name,
            id,
            routes,
            pinger,
            neighbors,
            IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS
        )
        .map(|state| StateEvent { state, time: fasync::Time::now() }),
        network_layer_state(
            &name,
            id,
            routes,
            pinger,
            neighbors,
            IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS
        )
        .map(|state| StateEvent { state, time: fasync::Time::now() })
    );
    Some(IpVersions { ipv4, ipv6 })
}

// `network_layer_state` determines the L3 reachability state.
async fn network_layer_state(
    name: &str,
    interface_id: u64,
    routes: &[fnet_stack::ForwardingEntry],
    p: &dyn Ping,
    neighbors: Option<&InterfaceNeighborCache>,
    internet_ping_address: std::net::IpAddr,
) -> State {
    use std::convert::TryInto as _;

    let device_routes: Vec<_> = routes
        .into_iter()
        .filter(|fnet_stack::ForwardingEntry { subnet: _, device_id, next_hop: _, metric: _ }| {
            *device_id == interface_id
        })
        .collect();

    let (discovered_healthy_neighbors, discovered_healthy_gateway) = match neighbors {
        None => (false, false),
        Some(neighbors) => {
            neighbors
                .iter_health()
                .fold_while(
                    (false, false),
                    |(discovered_healthy_neighbors, _discovered_healthy_gateway),
                     (neighbor, health)| {
                        let is_gateway = device_routes.iter().any(|route| {
                            route
                                .next_hop
                                .as_ref()
                                .map(|next_hop| neighbor == &**next_hop)
                                .unwrap_or(false)
                        });
                        match health {
                            // When we find an unhealthy or unknown neighbor, continue,
                            // keeping whether we've previously found a healthy neighbor.
                            neighbor_cache::NeighborHealth::Unhealthy { .. }
                            | neighbor_cache::NeighborHealth::Unknown => {
                                itertools::FoldWhile::Continue((
                                    discovered_healthy_neighbors,
                                    false,
                                ))
                            }
                            // If there's a healthy gateway, then we're done. If it's
                            // not a gateway, then we know we have a healthy neighbor, but
                            // not a healthy gateway.
                            neighbor_cache::NeighborHealth::Healthy { .. } => {
                                if !is_gateway {
                                    return itertools::FoldWhile::Continue((true, false));
                                }
                                itertools::FoldWhile::Done((true, true))
                            }
                        }
                    },
                )
                .into_inner()
        }
    };

    let relevant_routes = device_routes.iter().filter(
        |fnet_stack::ForwardingEntry { subnet, device_id: _, next_hop: _, metric: _ }| {
            *subnet == UNSPECIFIED_V4 || *subnet == UNSPECIFIED_V6
        },
    );

    let gateway_pingable = relevant_routes
        .filter_map(
            move |fnet_stack::ForwardingEntry { subnet: _, device_id, next_hop, metric: _ }| {
                next_hop.as_ref().and_then(|next_hop| {
                    let fnet_ext::IpAddress(next_hop) = (**next_hop).into();
                    match next_hop {
                        std::net::IpAddr::V4(v4) => {
                            Some(std::net::SocketAddr::V4(std::net::SocketAddrV4::new(v4, 0)))
                        }
                        std::net::IpAddr::V6(v6) => match (*device_id).try_into() {
                            Err(std::num::TryFromIntError { .. }) => {
                                error!("device id {} doesn't fit in u32", device_id);
                                None
                            }
                            Ok(device_id) => {
                                if device_id == 0
                                    && net_types::ip::Ipv6Addr::from_bytes(v6.octets()).scope()
                                        != net_types::ip::Ipv6Scope::Global
                                {
                                    None
                                } else {
                                    Some(std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
                                        v6, 0, 0, device_id,
                                    )))
                                }
                            }
                        },
                    }
                })
            },
        )
        .map(|next_hop| p.ping(name, next_hop))
        .collect::<futures::stream::FuturesUnordered<_>>()
        .filter(|ok| futures::future::ready(*ok))
        .next()
        .await
        .is_some();

    // The neighbor table is not always up to date within each iteration of the probe
    // because the neighbors table is updated in a separate stream. Therefore, we can
    // consider response to ping as a signal of an active gateway or use the neighbor
    // discovery signal. Potentially, both signals could be inaccurate, so we
    // continue to ping internet even if local network signals fail.

    // In the match suite below, all possible states for the three booleans have been
    // included. A few states should never be reached, but are included for
    // completeness and metrics tracking.
    // -----------------------------------------------------------------------------
    // | discovered | discovered | gateway  |                                      |
    // | healthy    | healthy    | pingable | state                                |
    // | neighbors  | gateway    |          |                                      |
    // |------------|------------|----------|--------------------------------------|
    // | false      | false      | false    | If there are no routes on the device,|
    // |            |            |          | Up, or else Local.                   |
    // |------------|------------|----------|--------------------------------------|
    // | false      | false      | true     | If internet is pingable, Internet,   |
    // |            |            |          | or else Gateway.                     |
    // |------------|------------|----------|--------------------------------------|
    // | false      | true       | false    | Invalid state                        |
    // |------------|------------|----------|--------------------------------------|
    // | false      | true       | true     | Invalid state                        |
    // |------------|------------|----------|--------------------------------------|
    // | true       | false      | false    | If internet is pingable, Internet, or|
    // |            |            |          | else Local.                          |
    // |------------|------------|----------|--------------------------------------|
    // | true       | false      | true     | If internet is pingable, Internet, or|
    // |            |            |          | else Gateway.                        |
    // |------------|------------|----------|--------------------------------------|
    // | true       | true       | false    | If internet is pingable, Internet, or|
    // |            |            |          | else Gateway.                        |
    // |------------|------------|----------|--------------------------------------|
    // | true       | true       | true     | If internet is pingable, Internet, or|
    // |            |            |          | else Gateway.                        |
    // -----------------------------------------------------------------------------

    return match (discovered_healthy_neighbors, discovered_healthy_gateway, gateway_pingable) {
        // TODO(fxbug.dev/120510): Add metrics for tracking abnormal states
        (false, false, false) => {
            if device_routes.is_empty() {
                return State::Up;
            }
            State::Local
        }
        (false, false, true) => {
            if !p.ping(name, std::net::SocketAddr::new(internet_ping_address, 0)).await {
                return State::Gateway;
            }
            tracing::info!("gateway ARP/ND  failing with internet available");
            return State::Internet;
        }
        (false, true, _) => {
            error!(
                "invalid state: cannot have gateway discovered while neighbors are undiscovered"
            );
            State::None
        }
        (true, false, gateway_pingable) => {
            if !p.ping(name, std::net::SocketAddr::new(internet_ping_address, 0)).await {
                if gateway_pingable {
                    return State::Gateway;
                }
                return State::Local;
            }
            tracing::info!("gateway ARP/ND failing with internet available");
            if !gateway_pingable {
                tracing::info!("gateway ping failing with internet available");
            }
            State::Internet
        }
        (true, true, gateway_pingable) => {
            if !p.ping(name, std::net::SocketAddr::new(internet_ping_address, 0)).await {
                return State::Gateway;
            }
            if !gateway_pingable {
                tracing::info!("gateway ping failing with internet available");
            }
            State::Internet
        }
    };
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::neighbor_cache::{NeighborHealth, NeighborState},
        async_trait::async_trait,
        fidl_fuchsia_net as fnet, fuchsia_async as fasync,
        net_declare::{fidl_ip, fidl_subnet, std_ip},
        std::task::Poll,
        test_case::test_case,
    };

    const ETHERNET_INTERFACE_NAME: &str = "eth1";
    const ID1: u64 = 1;
    const ID2: u64 = 2;

    // A trait for writing helper constructors.
    //
    // Note that this trait differs from `std::convert::From` only in name, but will almost always
    // contain shortcuts that would be too surprising for an actual `From` implementation.
    trait Construct<T> {
        fn construct(_: T) -> Self;
    }

    impl Construct<State> for StateEvent {
        fn construct(state: State) -> Self {
            Self { state, time: fasync::Time::INFINITE }
        }
    }

    impl Construct<StateEvent> for IpVersions<StateEvent> {
        fn construct(state: StateEvent) -> Self {
            Self { ipv4: state, ipv6: state }
        }
    }

    #[test]
    fn test_port_type() {
        assert_eq!(
            PortType::from(fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {})),
            PortType::Loopback
        );
        assert_eq!(
            PortType::from(fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::Ethernet
            )),
            PortType::Ethernet
        );
        assert_eq!(
            PortType::from(fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::Wlan
            )),
            PortType::WiFi
        );
        assert_eq!(
            PortType::from(fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::WlanAp
            )),
            PortType::WiFi
        );
        assert_eq!(
            PortType::from(fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::Virtual
            )),
            PortType::Unknown
        );
    }

    #[derive(Default)]
    struct FakePing {
        gateway_addrs: std::collections::HashSet<std::net::IpAddr>,
        gateway_response: bool,
        internet_response: bool,
    }

    #[async_trait]
    impl Ping for FakePing {
        async fn ping(&self, _interface_name: &str, addr: std::net::SocketAddr) -> bool {
            let Self { gateway_addrs, gateway_response, internet_response } = self;
            let ip = addr.ip();
            if [IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS, IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS]
                .contains(&ip)
            {
                *internet_response
            } else if gateway_addrs.contains(&ip) {
                *gateway_response
            } else {
                false
            }
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_network_layer_state_ipv4() {
        test_network_layer_state(
            fidl_ip!("1.2.3.0"),
            fidl_ip!("1.2.3.4"),
            fidl_ip!("1.2.3.1"),
            fidl_ip!("2.2.3.0"),
            fidl_ip!("2.2.3.1"),
            UNSPECIFIED_V4,
            fidl_subnet!("0.0.0.0/1"),
            IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
            24,
        )
        .await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_network_layer_state_ipv6() {
        test_network_layer_state(
            fidl_ip!("123::"),
            fidl_ip!("123::4"),
            fidl_ip!("123::1"),
            fidl_ip!("223::"),
            fidl_ip!("223::1"),
            UNSPECIFIED_V6,
            fidl_subnet!("::/1"),
            IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
            64,
        )
        .await
    }

    async fn test_network_layer_state(
        net1: fnet::IpAddress,
        _net1_addr: fnet::IpAddress,
        net1_gateway: fnet::IpAddress,
        net2: fnet::IpAddress,
        net2_gateway: fnet::IpAddress,
        unspecified_addr: fnet::Subnet,
        non_default_addr: fnet::Subnet,
        ping_internet_addr: std::net::IpAddr,
        prefix_len: u8,
    ) {
        let route_table = [
            fnet_stack::ForwardingEntry {
                subnet: unspecified_addr,
                device_id: ID1,
                next_hop: Some(Box::new(net1_gateway)),
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fnet::Subnet { addr: net1, prefix_len },
                device_id: ID1,
                next_hop: None,
                metric: 0,
            },
        ];
        let route_table_2 = [
            fnet_stack::ForwardingEntry {
                subnet: unspecified_addr,
                device_id: ID1,
                next_hop: Some(Box::new(net2_gateway)),
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fnet::Subnet { addr: net1, prefix_len },
                device_id: ID1,
                next_hop: None,
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fnet::Subnet { addr: net2, prefix_len },
                device_id: ID1,
                next_hop: None,
                metric: 0,
            },
        ];
        let route_table_3 = [
            fnet_stack::ForwardingEntry {
                subnet: unspecified_addr,
                device_id: ID2,
                next_hop: Some(Box::new(net1_gateway)),
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fnet::Subnet { addr: net1, prefix_len },
                device_id: ID2,
                next_hop: None,
                metric: 0,
            },
        ];
        let route_table_4 = [
            fnet_stack::ForwardingEntry {
                subnet: non_default_addr,
                device_id: ID1,
                next_hop: Some(Box::new(net1_gateway)),
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fnet::Subnet { addr: net1, prefix_len },
                device_id: ID1,
                next_hop: None,
                metric: 0,
            },
        ];

        let fnet_ext::IpAddress(net1_gateway_ext) = net1_gateway.into();

        // TODO(fxrev.dev/120580): Extract test cases into variants/helper function
        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: true,
                },
                None,
                ping_internet_addr,
            )
            .await,
            State::Internet,
            "All is good. Can reach internet"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: false,
                    internet_response: true,
                },
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1_gateway,
                        NeighborState::new(NeighborHealth::Healthy {
                            last_observed: fuchsia_zircon::Time::default(),
                        })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr
            )
            .await,
            State::Internet,
            "Can reach internet, gateway responding via ARP/ND"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: false,
                    internet_response: true,
                },
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1,
                        NeighborState::new(NeighborHealth::Healthy {
                            last_observed: fuchsia_zircon::Time::default(),
                        })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr,
            )
            .await,
            State::Internet,
            "Gateway not responding via ping or ARP/ND. Can reach internet"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_4,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: true,
                },
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1_gateway,
                        NeighborState::new(NeighborHealth::Healthy {
                            last_observed: fuchsia_zircon::Time::default(),
                        })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr,
            )
            .await,
            State::Internet,
            "No default route, but healthy gateway with internet/gateway response"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: false,
                },
                None,
                ping_internet_addr,
            )
            .await,
            State::Gateway,
            "Can reach gateway via ping"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing::default(),
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1_gateway,
                        NeighborState::new(NeighborHealth::Healthy {
                            last_observed: fuchsia_zircon::Time::default(),
                        })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr
            )
            .await,
            State::Gateway,
            "Can reach gateway via ARP/ND"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: false,
                    internet_response: false,
                },
                None,
                ping_internet_addr,
            )
            .await,
            State::Local,
            "Local only, Cannot reach gateway"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_2,
                &FakePing::default(),
                None,
                ping_internet_addr,
            )
            .await,
            State::Local,
            "No default route"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_4,
                &FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: false,
                },
                None,
                ping_internet_addr,
            )
            .await,
            State::Local,
            "No default route, with only gateway response"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_2,
                &FakePing::default(),
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1,
                        NeighborState::new(NeighborHealth::Healthy {
                            last_observed: fuchsia_zircon::Time::default(),
                        })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr
            )
            .await,
            State::Local,
            "Local only, neighbors responsive with no default route"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_3,
                &FakePing::default(),
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1,
                        NeighborState::new(NeighborHealth::Healthy {
                            last_observed: fuchsia_zircon::Time::default(),
                        })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr
            )
            .await,
            State::Local,
            "Local only, neighbors responsive with no routes"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                &FakePing::default(),
                Some(&InterfaceNeighborCache {
                    neighbors: [
                        (
                            net1,
                            NeighborState::new(NeighborHealth::Healthy {
                                last_observed: fuchsia_zircon::Time::default(),
                            })
                        ),
                        (
                            net1_gateway,
                            NeighborState::new(NeighborHealth::Unhealthy { last_healthy: None })
                        )
                    ]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr
            )
            .await,
            State::Local,
            "Local only, gateway unhealthy with healthy neighbor"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_3,
                &FakePing::default(),
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1_gateway,
                        NeighborState::new(NeighborHealth::Unhealthy { last_healthy: None })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr
            )
            .await,
            State::Up,
            "No routes and unhealthy gateway"
        );

        assert_eq!(
            network_layer_state(
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_3,
                &FakePing::default(),
                None,
                ping_internet_addr
            )
            .await,
            State::Up,
            "No routes"
        );
    }

    #[test]
    fn test_compute_state() {
        let properties = fnet_interfaces_ext::Properties {
            id: ID1,
            name: ETHERNET_INTERFACE_NAME.to_string(),
            device_class: fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::Ethernet,
            ),
            has_default_ipv4_route: true,
            has_default_ipv6_route: true,
            online: true,
            addresses: vec![
                fnet_interfaces_ext::Address {
                    addr: fidl_subnet!("1.2.3.4/24"),
                    valid_until: fuchsia_zircon::Time::INFINITE.into_nanos(),
                },
                fnet_interfaces_ext::Address {
                    addr: fidl_subnet!("123::4/64"),
                    valid_until: fuchsia_zircon::Time::INFINITE.into_nanos(),
                },
            ],
        };
        let local_routes = [fnet_stack::ForwardingEntry {
            subnet: fidl_subnet!("1.2.3.4/24"),
            device_id: ID1,
            next_hop: None,
            metric: 0,
        }];
        let route_table = [
            fnet_stack::ForwardingEntry {
                subnet: fidl_subnet!("0.0.0.0/0"),
                device_id: ID1,
                next_hop: Some(Box::new(fidl_ip!("1.2.3.1"))),
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fidl_subnet!("::/0"),
                device_id: ID1,
                next_hop: Some(Box::new(fidl_ip!("123::1"))),
                metric: 0,
            },
        ];
        let route_table2 = [
            fnet_stack::ForwardingEntry {
                subnet: fidl_subnet!("0.0.0.0/0"),
                device_id: ID1,
                next_hop: Some(Box::new(fidl_ip!("2.2.3.1"))),
                metric: 0,
            },
            fnet_stack::ForwardingEntry {
                subnet: fidl_subnet!("::/0"),
                device_id: ID1,
                next_hop: Some(Box::new(fidl_ip!("223::1"))),
                metric: 0,
            },
        ];

        const NON_ETHERNET_INTERFACE_NAME: &str = "test01";

        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let time = fasync::Time::from_nanos(1_000_000_000);
        let () = exec.set_fake_time(time.into());

        fn run_compute_state(
            exec: &mut fasync::TestExecutor,
            properties: &fnet_interfaces_ext::Properties,
            routes: &[fnet_stack::ForwardingEntry],
            pinger: &dyn Ping,
        ) -> Result<Option<IpVersions<StateEvent>>, anyhow::Error> {
            let fut = compute_state(&properties, routes, pinger, None);
            futures::pin_mut!(fut);
            match exec.run_until_stalled(&mut fut) {
                Poll::Ready(got) => Ok(got),
                Poll::Pending => Err(anyhow::anyhow!("compute_state blocked unexpectedly")),
            }
        }

        let got = run_compute_state(
            &mut exec,
            &fnet_interfaces_ext::Properties {
                id: ID1,
                name: NON_ETHERNET_INTERFACE_NAME.to_string(),
                device_class: fnet_interfaces::DeviceClass::Device(
                    fidl_fuchsia_hardware_network::DeviceClass::Virtual,
                ),
                online: false,
                has_default_ipv4_route: false,
                has_default_ipv6_route: false,
                addresses: vec![],
            },
            &[],
            &FakePing::default(),
        )
        .expect(
            "error calling compute_state with non-ethernet interface, no addresses, interface down",
        );
        assert_eq!(got, Some(IpVersions::construct(StateEvent { state: State::Down, time })));

        let got = run_compute_state(
            &mut exec,
            &fnet_interfaces_ext::Properties { online: false, ..properties.clone() },
            &[],
            &FakePing::default(),
        )
        .expect("error calling compute_state, want Down state");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Down, time }));
        assert_eq!(got, want);

        let got = run_compute_state(
            &mut exec,
            &fnet_interfaces_ext::Properties {
                has_default_ipv4_route: false,
                has_default_ipv6_route: false,
                ..properties.clone()
            },
            &local_routes,
            &FakePing::default(),
        )
        .expect("error calling compute_state, want Local state due to no default routes");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Local, time }));
        assert_eq!(got, want);

        let got = run_compute_state(&mut exec, &properties, &route_table2, &FakePing::default())
            .expect(
                "error calling compute_state, want Local state due to no matching default route",
            );
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Local, time }));
        assert_eq!(got, want);

        let got = run_compute_state(
            &mut exec,
            &properties,
            &route_table,
            &mut FakePing {
                gateway_addrs: [std_ip!("1.2.3.1"), std_ip!("123::1")].iter().cloned().collect(),
                gateway_response: true,
                internet_response: false,
            },
        )
        .expect("error calling compute_state, want Gateway state");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Gateway, time }));
        assert_eq!(got, want);

        let got = run_compute_state(
            &mut exec,
            &properties,
            &route_table,
            &mut FakePing {
                gateway_addrs: [std_ip!("1.2.3.1"), std_ip!("123::1")].iter().cloned().collect(),
                gateway_response: true,
                internet_response: true,
            },
        )
        .expect("error calling compute_state, want Internet state");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Internet, time }));
        assert_eq!(got, want);
    }

    #[test]
    fn test_state_info_update() {
        fn update_delta(port: Delta<StateEvent>, system: Delta<SystemState>) -> StateDelta {
            StateDelta {
                port: IpVersions { ipv4: port.clone(), ipv6: port },
                system: IpVersions { ipv4: system.clone(), ipv6: system },
            }
        }

        let if1_local_event = StateEvent::construct(State::Local);
        let if1_local = IpVersions::<StateEvent>::construct(if1_local_event);
        // Post-update the system state should be Local due to interface 1.
        let mut state = StateInfo::default();
        let want = update_delta(
            Delta { previous: None, current: if1_local_event },
            Delta { previous: None, current: SystemState { id: ID1, state: if1_local_event } },
        );
        assert_eq!(state.update(ID1, if1_local.clone()), want);
        let want_state = StateInfo {
            per_interface: std::iter::once((ID1, if1_local.clone())).collect::<HashMap<_, _>>(),
            system: IpVersions { ipv4: Some(ID1), ipv6: Some(ID1) },
        };
        assert_eq!(state, want_state);

        let if2_gateway_event = StateEvent::construct(State::Gateway);
        let if2_gateway = IpVersions::<StateEvent>::construct(if2_gateway_event);
        // Pre-update, the system state is Local due to interface 1; post-update the system state
        // will be Gateway due to interface 2.
        let want = update_delta(
            Delta { previous: None, current: if2_gateway_event },
            Delta {
                previous: Some(SystemState { id: ID1, state: if1_local_event }),
                current: SystemState { id: ID2, state: if2_gateway_event },
            },
        );
        assert_eq!(state.update(ID2, if2_gateway.clone()), want);
        let want_state = StateInfo {
            per_interface: [(ID1, if1_local.clone()), (ID2, if2_gateway.clone())]
                .iter()
                .cloned()
                .collect::<HashMap<_, _>>(),
            system: IpVersions { ipv4: Some(ID2), ipv6: Some(ID2) },
        };
        assert_eq!(state, want_state);

        let if2_removed_event = StateEvent::construct(State::Removed);
        let if2_removed = IpVersions::<StateEvent>::construct(if2_removed_event);
        // Pre-update, the system state is Gateway due to interface 2; post-update the system state
        // will be Local due to interface 1.
        let want = update_delta(
            Delta { previous: Some(if2_gateway_event), current: if2_removed_event },
            Delta {
                previous: Some(SystemState { id: ID2, state: if2_gateway_event }),
                current: SystemState { id: ID1, state: if1_local_event },
            },
        );
        assert_eq!(state.update(ID2, if2_removed.clone()), want);
        let want_state = StateInfo {
            per_interface: [(ID1, if1_local.clone()), (ID2, if2_removed.clone())]
                .iter()
                .cloned()
                .collect::<HashMap<_, _>>(),
            system: IpVersions { ipv4: Some(ID1), ipv6: Some(ID1) },
        };
        assert_eq!(state, want_state);
    }

    #[test_case(None, None, false, false;
        "no interfaces available")]
    #[test_case(Some(&State::Local), Some(&State::Local), false, false;
        "no interfaces with gateway or internet state")]
    #[test_case(Some(&State::Local), Some(&State::Gateway), false, true;
        "only one interface with gateway state or above")]
    #[test_case(Some(&State::Local), Some(&State::Internet), true, true;
        "only one interface with internet state")]
    #[test_case(Some(&State::Internet), Some(&State::Internet), true, true;
        "all interfaces with internet")]
    #[test_case(Some(&State::Internet), None, true, true;
        "only one interface available, has internet state")]
    fn test_system_has_state(
        ipv4_state: Option<&State>,
        ipv6_state: Option<&State>,
        expect_internet: bool,
        expect_gateway: bool,
    ) {
        let if1 = ipv4_state
            .map(|state| IpVersions::<StateEvent>::construct(StateEvent::construct(*state)));
        let if2 = ipv6_state
            .map(|state| IpVersions::<StateEvent>::construct(StateEvent::construct(*state)));

        let mut system_interfaces: HashMap<u64, IpVersions<StateEvent>> = HashMap::new();

        let system_interface_ipv4 = if1.map(|interface| {
            let _ = system_interfaces.insert(ID1, interface);
            ID1
        });

        let system_interface_ipv6 = if2.map(|interface| {
            let _ = system_interfaces.insert(ID2, interface);
            ID2
        });

        let state = StateInfo {
            per_interface: system_interfaces,
            system: IpVersions { ipv4: system_interface_ipv4, ipv6: system_interface_ipv6 },
        };

        assert_eq!(state.system_has_internet(), expect_internet);
        assert_eq!(state.system_has_gateway(), expect_gateway);
    }
}
