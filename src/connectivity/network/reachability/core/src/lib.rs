// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(clippy::unused_async)]

mod inspect;
mod neighbor_cache;
pub mod ping;
pub mod route_table;
pub mod telemetry;
pub mod watchdog;

#[cfg(test)]
mod testutil;

use {
    crate::route_table::{Route, RouteTable},
    crate::telemetry::{TelemetryEvent, TelemetrySender},
    anyhow::anyhow,
    fidl_fuchsia_hardware_network, fidl_fuchsia_net_ext as fnet_ext,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fuchsia_async as fasync,
    fuchsia_inspect::{Inspector, Node as InspectNode},
    futures::channel::mpsc,
    inspect::InspectInfo,
    itertools::Itertools,
    named_timer::DeadlineId,
    net_declare::{fidl_subnet, std_ip},
    net_types::ScopeableAddress as _,
    num_derive::FromPrimitive,
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
#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Clone, Copy, FromPrimitive)]
#[repr(u8)]
pub enum State {
    /// State not yet determined.
    None = 1,
    /// Interface no longer present.
    Removed = 5,
    /// Interface is down.
    Down = 10,
    /// Interface is up, no packets seen yet.
    Up = 15,
    /// L3 Interface is up, local neighbors seen.
    Local = 20,
    /// L3 Interface is up, local gateway configured and reachable.
    Gateway = 25,
    /// Expected response seen from reachability test URL.
    Internet = 30,
}

impl State {
    fn has_interface_up(&self) -> bool {
        match self {
            State::None | State::Removed | State::Down => false,
            State::Up | State::Local | State::Gateway | State::Internet => true,
        }
    }

    fn has_internet(&self) -> bool {
        *self == State::Internet
    }

    fn has_gateway(&self) -> bool {
        *self == State::Gateway || *self == State::Internet
    }

    fn log_state_vals_inspect(node: &InspectNode, name: &str) {
        let child = node.create_child(name);
        for i in State::None as u32..=State::Internet as u32 {
            match <State as num_traits::FromPrimitive>::from_u32(i) {
                Some(state) => child.record_string(i.to_string(), format!("{:?}", state)),
                None => (),
            }
        }
        node.record(child);
    }
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
            "Local" => Ok(Self::Local),
            "Gateway" => Ok(Self::Gateway),
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

impl IpVersions<Option<SystemState>> {
    fn state(&self) -> IpVersions<Option<State>> {
        IpVersions {
            ipv4: self.ipv4.map(|s| s.state.state),
            ipv6: self.ipv6.map(|s| s.state.state),
        }
    }
}

impl IpVersions<Option<State>> {
    fn has_interface_up(&self) -> bool {
        self.satisfies(State::has_interface_up)
    }

    fn has_internet(&self) -> bool {
        self.satisfies(State::has_internet)
    }

    fn has_gateway(&self) -> bool {
        self.satisfies(State::has_gateway)
    }

    fn satisfies<F>(&self, f: F) -> bool
    where
        F: Fn(&State) -> bool,
    {
        return [self.ipv4, self.ipv6].iter().filter_map(|state| state.as_ref()).any(f);
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

    fn get_system(&self) -> IpVersions<Option<SystemState>> {
        IpVersions { ipv4: self.get_system_ipv4(), ipv6: self.get_system_ipv6() }
    }

    pub fn system_has_internet(&self) -> bool {
        self.get_system().state().has_internet()
    }

    pub fn system_has_gateway(&self) -> bool {
        self.get_system().state().has_gateway()
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
    pub routes: &'a RouteTable,
    pub neighbors: Option<&'a InterfaceNeighborCache>,
}

/// `NetworkCheckerOutcome` contains values indicating whether a network check completed or needs
/// resumption.
pub enum NetworkCheckerOutcome {
    /// The network check must be resumed via a call to `resume` to complete.
    MustResume,
    /// The network check is finished and the reachability state for the specified interface has
    /// been updated. A new network check can begin on the same interface via `begin`.
    Complete,
}

/// A Network Checker is a re-entrant, asynchronous state machine that monitors availability of
/// networks over a given network interface.
pub trait NetworkChecker {
    /// `begin` starts a re-entrant, asynchronous network check on the supplied interface. It
    /// returns whether the network check was completed, must be resumed, or if the supplied
    /// interface already had an ongoing network check.
    fn begin(&mut self, view: InterfaceView<'_>) -> Result<NetworkCheckerOutcome, anyhow::Error>;

    /// `resume` continues a network check that was not yet completed.
    fn resume(
        &mut self,
        cookie: NetworkCheckCookie,
        ping_success: bool,
    ) -> Result<NetworkCheckerOutcome, anyhow::Error>;
}

// States involved in `Monitor`'s implementation of NetworkChecker.
#[derive(Debug, Default)]
enum NetworkCheckState {
    // `Begin` starts a new network check. This state analyzes link properties. It can transition
    // to `PingGateway` when a default gateway is configured on the interface, to `PingInternet`
    // when off-link routes are configured but no default gateway, and `Idle` if analyzing link
    // properties allows determining that connectivity past the local network is not possible.
    #[default]
    Begin,
    // `PingGateway` sends a ping to each of the available gateways with a default route. It can
    // transition to `PingInternet` when a healthy gateway is detected through neighbor discovery,
    // or when at least one gateway ping successfully returns, and `Idle` if no healthy gateway is
    // detected and no gateway pings successfully return.
    PingGateway,
    // `PingInternet` sends a ping to an IPv4 and IPv6 external address. It can only transition to
    // `Idle` after it has completed internet pings.
    PingInternet,
    // `Idle` terminates a network check. The system is ready to begin processing another network
    // check for interface associated with this check.
    Idle,
}
impl std::fmt::Display for NetworkCheckState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkCheckState::Begin => write!(f, "Begin"),
            NetworkCheckState::PingGateway => write!(f, "Ping Gateway"),
            NetworkCheckState::PingInternet => write!(f, "Ping Internet"),
            NetworkCheckState::Idle => write!(f, "Idle"),
        }
    }
}

// Contains all information related to a network check on an interface.
struct NetworkCheckContext {
    // The current status of the state machine.
    checker_state: NetworkCheckState,
    // The list of addresses to ping (either gateway or internet).
    ping_addrs: Vec<std::net::SocketAddr>,
    // The quantity of pings sent.
    pings_expected: usize,
    // The quantity of pings that have been received.
    pings_completed: usize,
    // The current calculated v4 state.
    discovered_state_v4: State,
    // The current calculated v6 state.
    discovered_state_v6: State,
    // Whether the network check should ping internet regardless of if the gateway pings fail.
    always_ping_internet: bool,
    // Whether an online router was discoverable via neighbor discovery.
    router_discoverable: bool,
    // Whether the gateway successfully responded to pings.
    gateway_pingable: bool,
    // TODO (fxbug.dev/123591): Add tombstone marker to inform NetworkCheck that the interface has
    // been removed and we no longer need to run checks on this interface. This can occur when
    // receiving an interface remote event, but a network check for that interface is still in
    // progress.
}

impl NetworkCheckContext {
    fn set_global_state(&mut self, state: State) {
        self.discovered_state_v4 = state;
        self.discovered_state_v6 = state;
    }
}

impl Default for NetworkCheckContext {
    // Create a context for an interface's network check.
    fn default() -> Self {
        NetworkCheckContext {
            checker_state: Default::default(),
            ping_addrs: Vec::new(),
            pings_expected: 0usize,
            pings_completed: 0usize,
            discovered_state_v4: State::None,
            discovered_state_v6: State::None,
            always_ping_internet: true,
            router_discoverable: false,
            gateway_pingable: false,
        }
    }
}

/// NetworkCheckCookie is an opaque type used to continue an asynchronous network check.
#[derive(Clone)]
pub struct NetworkCheckCookie {
    /// The interface id.
    id: Id,
    /// The action associated with this request.
    action: NetworkCheckAction,
}

/// `NetworkCheckAction` describes the action to be completed before resuming the network check.
#[derive(Clone)]
pub enum NetworkCheckAction {
    Ping {
        /// The name of the interface sending the ping.
        interface_name: std::string::String,
        /// The address to ping.
        addr: std::net::SocketAddr,
    },
}

/// `Monitor` monitors the reachability state.
pub struct Monitor {
    state: StateInfo,
    stats: Stats,
    inspector: Option<&'static Inspector>,
    system_node: Option<InspectInfo>,
    nodes: HashMap<Id, InspectInfo>,
    telemetry_sender: Option<TelemetrySender>,
    /// In `Monitor`'s implementation of NetworkChecker, the sender is used to dispatch network
    /// checks to the eventloop to be run concurrently. The network check then will be resumed with
    /// the result of the `NetworkCheckAction`.
    network_check_sender: mpsc::UnboundedSender<(NetworkCheckAction, NetworkCheckCookie)>,
    interface_context: HashMap<Id, NetworkCheckContext>,
}

impl Monitor {
    /// Create the monitoring service.
    pub fn new(
        network_check_sender: mpsc::UnboundedSender<(NetworkCheckAction, NetworkCheckCookie)>,
    ) -> anyhow::Result<Self> {
        Ok(Monitor {
            state: Default::default(),
            stats: Default::default(),
            inspector: None,
            system_node: None,
            nodes: HashMap::new(),
            telemetry_sender: None,
            network_check_sender,
            interface_context: HashMap::new(),
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

        State::log_state_vals_inspect(inspector.root(), "state_vals");
    }

    pub fn set_telemetry_sender(&mut self, telemetry_sender: TelemetrySender) {
        self.telemetry_sender = Some(telemetry_sender);
    }

    fn interface_node(&mut self, id: Id, name: &str) -> Option<&mut InspectInfo> {
        self.inspector.map(move |inspector| {
            self.nodes.entry(id).or_insert_with_key(|id| {
                InspectInfo::new(inspector.root(), &format!("{:?}", id), name)
            })
        })
    }

    fn update_state_from_context(
        &mut self,
        id: Id,
        name: &str,
    ) -> Result<NetworkCheckerOutcome, anyhow::Error> {
        let ctx = self.interface_context.get_mut(&id).ok_or(anyhow!(
            "attempting to update state with context but context for id {} does not exist",
            id
        ))?;

        ctx.checker_state = NetworkCheckState::Idle;
        let info = IpVersions {
            ipv4: StateEvent { state: ctx.discovered_state_v4, time: fasync::Time::now() },
            ipv6: StateEvent { state: ctx.discovered_state_v6, time: fasync::Time::now() },
        };

        let telemetry_event_v4 = TelemetryEvent::GatewayProbe {
            gateway_discoverable: ctx.router_discoverable,
            gateway_pingable: ctx.gateway_pingable,
            internet_available: ctx.discovered_state_v4.has_internet(),
        };
        let telemetry_event_v6 = TelemetryEvent::GatewayProbe {
            gateway_discoverable: ctx.router_discoverable,
            gateway_pingable: ctx.gateway_pingable,
            internet_available: ctx.discovered_state_v6.has_internet(),
        };

        if let Some(telemetry_sender) = &mut self.telemetry_sender {
            telemetry_sender.send(telemetry_event_v4);
            telemetry_sender.send(telemetry_event_v6);
            telemetry_sender.send(TelemetryEvent::SystemStateUpdate {
                update: telemetry::SystemStateUpdate {
                    system_state: self.state.get_system().state(),
                },
            });
        }

        let () = self.update_state(id, &name, info);
        Ok(NetworkCheckerOutcome::Complete)
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

    fn handle_ping_action(
        &mut self,
        cookie: NetworkCheckCookie,
        success: bool,
    ) -> Result<NetworkCheckerOutcome, anyhow::Error> {
        let id = Id::from(cookie.id);
        let (interface_name, addr) = match cookie.action {
            NetworkCheckAction::Ping { interface_name, addr } => (interface_name, addr),
        };
        info!("handle ping: success: {}; interface id: {}; ping address: {}", success, id, addr);

        let ctx = self
            .interface_context
            .get_mut(&id)
            .ok_or(anyhow!("handle ping: interface id {} should already exist in map", id))?;

        ctx.pings_completed = ctx.pings_completed + 1;

        match ctx.checker_state {
            NetworkCheckState::Begin | NetworkCheckState::Idle => {
                return Err(anyhow!(
                    "handle ping: interface id: {}; state: {:?}; {}/{} pings completed",
                    ctx.checker_state,
                    id,
                    ctx.pings_completed,
                    ctx.pings_expected
                ));
            }
            NetworkCheckState::PingGateway | NetworkCheckState::PingInternet => {}
        }

        info!(
            "handle ping: after ping response {}/{} pings completed",
            ctx.pings_completed, ctx.pings_expected
        );

        if success {
            let () = Self::handle_ping_success(ctx, &addr);
        }

        if ctx.pings_completed == ctx.pings_expected {
            match ctx.checker_state {
                NetworkCheckState::PingGateway => {
                    // By default, attempt to ping internet whether or not gateway succeeds. With
                    // the following flag set to false, if the gateway pings have failed, do not
                    // attempt to ping internet.
                    let did_gateway_succeed = ctx.discovered_state_v4 == State::Gateway
                        || ctx.discovered_state_v6 == State::Gateway;
                    if !did_gateway_succeed && !ctx.always_ping_internet {
                        return self.update_state_from_context(id, &interface_name);
                    }

                    ctx.checker_state = NetworkCheckState::PingInternet;
                    let internet_ping_addrs = [
                        IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
                        IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
                    ];
                    ctx.ping_addrs = internet_ping_addrs
                        .iter()
                        .map(|addr| std::net::SocketAddr::new(*addr, 0))
                        .collect();
                    ctx.pings_expected = internet_ping_addrs.len();
                    ctx.pings_completed = 0;
                    ctx.ping_addrs
                        .iter()
                        .map(|addr| {
                            let action = NetworkCheckAction::Ping {
                                interface_name: interface_name.to_string().clone(),
                                addr: addr.clone(),
                            };
                            (action.clone(), NetworkCheckCookie { id, action: action.clone() })
                        })
                        .for_each(|message| {
                            match self.network_check_sender.unbounded_send(message) {
                                Ok(()) => {}
                                Err(e) => {
                                    debug!("unable to send network check internet msg: {:?}", e)
                                }
                            }
                        });
                }
                NetworkCheckState::PingInternet => {
                    return self.update_state_from_context(id, &interface_name);
                }
                NetworkCheckState::Begin | NetworkCheckState::Idle => {}
            }
        }
        Ok(NetworkCheckerOutcome::MustResume)
    }

    fn handle_ping_success(ctx: &mut NetworkCheckContext, addr: &std::net::SocketAddr) {
        match ctx.checker_state {
            NetworkCheckState::PingGateway => {
                ctx.gateway_pingable = true;
                match addr {
                    std::net::SocketAddr::V4 { .. } => ctx.discovered_state_v4 = State::Gateway,
                    std::net::SocketAddr::V6 { .. } => ctx.discovered_state_v6 = State::Gateway,
                }
            }
            NetworkCheckState::PingInternet => match addr {
                std::net::SocketAddr::V4 { .. } => ctx.discovered_state_v4 = State::Internet,
                std::net::SocketAddr::V6 { .. } => ctx.discovered_state_v6 = State::Internet,
            },
            NetworkCheckState::Begin | NetworkCheckState::Idle => {
                panic!("continue check had an invalid state")
            }
        }
    }
}

impl NetworkChecker for Monitor {
    fn begin(
        &mut self,
        InterfaceView {
            properties:
                &fnet_interfaces_ext::Properties {
                    id,
                    ref name,
                    device_class: _,
                    online,
                    addresses: _,
                    has_default_ipv4_route: _,
                    has_default_ipv6_route: _,
                },
            routes,
            neighbors,
        }: InterfaceView<'_>,
    ) -> Result<NetworkCheckerOutcome, anyhow::Error> {
        let id = Id::from(id);
        // Check to see if the current interface view is already in the map. If its state is not
        // Idle then another network check for the interface is already processing. In this case,
        // drop the `begin` request and log it.
        // It is expected for this to occur when an interface is experiencing many events in a
        // short period of time, for example changing between online and offline multiple times
        // over the span of a few seconds. It is safe that this happens, as the system is
        // eventually consistent.
        let ctx = self.interface_context.entry(id).or_insert(Default::default());

        match ctx.checker_state {
            NetworkCheckState::Begin => {}
            NetworkCheckState::Idle => {
                *ctx = Default::default();
            }
            NetworkCheckState::PingGateway | NetworkCheckState::PingInternet => {
                return Err(anyhow!("skipped, non-idle state found on interface {}", id));
            }
        }

        if !online {
            ctx.set_global_state(State::Down);
            return self.update_state_from_context(id, name);
        }

        // TODO(https://fxbug.dev/74517) Check if packet count has increased, and if so upgrade the
        // state to LinkLayerUp.
        let device_routes: Vec<_> = routes.device_routes(id).collect();

        let (discovered_online_neighbor, discovered_online_router) =
            scan_neighbor_health(neighbors, &device_routes);

        if !discovered_online_neighbor && discovered_online_router {
            return Err(anyhow!(
                "invalid state: cannot have router discovered while neighbors are undiscovered"
            ));
        }

        if !discovered_online_neighbor && !discovered_online_router {
            let discovered_state = if device_routes.is_empty() { State::Up } else { State::Local };
            ctx.set_global_state(discovered_state);

            if discovered_state == State::Up {
                return self.update_state_from_context(id, name);
            } else {
                // When a router is not discoverable via ND, the internet should only be pinged
                // if the gateway ping succeeds.
                ctx.always_ping_internet = false;
            }
        }

        let relevant_routes: Vec<_> = device_routes
            .iter()
            .filter(|Route { destination, outbound_interface: _, next_hop: _ }| {
                *destination == UNSPECIFIED_V4 || *destination == UNSPECIFIED_V6
            })
            .collect();

        let gateway_ping_addrs = relevant_routes
            .iter()
            .filter_map(move |Route { destination: _, outbound_interface, next_hop }| {
                next_hop.and_then(|next_hop| {
                    let fnet_ext::IpAddress(next_hop) = next_hop.into();
                    match next_hop.into() {
                        std::net::IpAddr::V4(v4) => {
                            Some(std::net::SocketAddr::V4(std::net::SocketAddrV4::new(v4, 0)))
                        }
                        std::net::IpAddr::V6(v6) => match (*outbound_interface).try_into() {
                            Err(std::num::TryFromIntError { .. }) => {
                                error!("device id {} doesn't fit in u32", outbound_interface);
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
            })
            .map(|next_hop| next_hop)
            .collect::<Vec<_>>();

        // A router is determined to be discoverable if it is online (marked as healthy by ND).
        ctx.router_discoverable = discovered_online_router;
        if gateway_ping_addrs.is_empty() {
            // When there are no gateway addresses to ping, the gateway is not pingable. The list
            // of Gateway addresses is obtained by filtering the default IPv4 and IPv6 routes.

            // We use the discovery of an online router as a separate opportunity to calculate
            // internet reachability because of the potential for various network configurations.
            // One potential case involves having an AP operating in bridge mode, and having a
            // separate device host DHCP. In this situation, it's possible to have routes that can
            // be used to send pings to the internet that are not default routes. In another case,
            // a router may have a very specific target prefix that is routable. The device could
            // access a remote set of addresses through this local router and not view it as being
            // accessed through a default route.
            if discovered_online_router {
                // Setup to ping internet addresses, skipping over gateway pings.
                // Internet can be pinged when either an online router is discovered or the gateway
                // is pingable. In this case, the discovery of a router enables the internet ping.
                // TODO(fxbug.dev/124034): Create an occurrence metric for this case
                ctx.checker_state = NetworkCheckState::PingInternet;
                let internet_ping_addrs = [
                    IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
                    IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
                ];
                ctx.ping_addrs = internet_ping_addrs
                    .iter()
                    .map(|addr| std::net::SocketAddr::new(*addr, 0))
                    .collect();
                ctx.pings_expected = internet_ping_addrs.len();
                ctx.ping_addrs
                    .iter()
                    .map(|addr| {
                        let action = NetworkCheckAction::Ping {
                            interface_name: name.clone(),
                            addr: addr.clone(),
                        };
                        (action.clone(), NetworkCheckCookie { id, action: action.clone() })
                    })
                    .for_each(|message| match self.network_check_sender.unbounded_send(message) {
                        Ok(()) => {}
                        Err(e) => {
                            debug!("begin: unable to send msg for internet ping: {:?}", e)
                        }
                    });
            } else {
                // The router is not online and the gateway cannot be pinged; therefore, the
                // internet pings can be skipped and the final reachability state can be
                // determined.
                ctx.set_global_state(State::Local);
                return self.update_state_from_context(id, name);
            }
        } else {
            // Setup to ping gateway addresses.
            ctx.checker_state = NetworkCheckState::PingGateway;
            ctx.ping_addrs = gateway_ping_addrs;
            ctx.pings_expected = ctx.ping_addrs.len();

            let discovered_state =
                if discovered_online_router { State::Gateway } else { State::Local };
            ctx.discovered_state_v4 = discovered_state;
            ctx.discovered_state_v6 = discovered_state;

            ctx.ping_addrs
                .iter()
                .map(|addr| {
                    let action = NetworkCheckAction::Ping {
                        interface_name: name.clone(),
                        addr: addr.clone(),
                    };
                    (action.clone(), NetworkCheckCookie { id, action: action.clone() })
                })
                .for_each(|message| match self.network_check_sender.unbounded_send(message) {
                    Ok(()) => {}
                    Err(e) => {
                        debug!("begin: unable to send msg for gateway ping: {:?}", e)
                    }
                });
        }
        Ok(NetworkCheckerOutcome::MustResume)
    }

    fn resume(
        &mut self,
        cookie: NetworkCheckCookie,
        success: bool,
    ) -> Result<NetworkCheckerOutcome, anyhow::Error> {
        match cookie.action {
            NetworkCheckAction::Ping { .. } => self.handle_ping_action(cookie, success),
        }
    }
}

fn log_state(info: Option<&mut InspectInfo>, proto: Proto, state: State) {
    info.into_iter().for_each(|info| info.log_state(proto, state))
}

// Determines whether any online neighbors or online gateways are discoverable via neighbor
// discovery. The definition of a Healthy neighbor correlates to a neighbor being online.
fn scan_neighbor_health(
    neighbors: Option<&InterfaceNeighborCache>,
    device_routes: &Vec<route_table::Route>,
) -> (bool, bool) {
    match neighbors {
        None => (false, false),
        Some(neighbors) => {
            neighbors
                .iter_health()
                .fold_while(
                    (false, false),
                    |(discovered_online_neighbor, _discovered_online_router),
                     (neighbor, health)| {
                        let is_router = device_routes.iter().any(
                            |Route { destination: _, outbound_interface: _, next_hop }| {
                                next_hop.map(|next_hop| *neighbor == next_hop).unwrap_or(false)
                            },
                        );
                        match health {
                            // When we find an unhealthy or unknown neighbor, continue,
                            // keeping whether we've previously found a healthy neighbor.
                            neighbor_cache::NeighborHealth::Unhealthy { .. }
                            | neighbor_cache::NeighborHealth::Unknown => {
                                itertools::FoldWhile::Continue((discovered_online_neighbor, false))
                            }
                            // If there's a healthy router, then we're done. If the neighbor
                            // is not a router, then we know we have a healthy neighbor, but
                            // not a healthy router.
                            neighbor_cache::NeighborHealth::Healthy { .. } => {
                                if !is_router {
                                    return itertools::FoldWhile::Continue((true, false));
                                }
                                itertools::FoldWhile::Done((true, true))
                            }
                        }
                    },
                )
                .into_inner()
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            neighbor_cache::{NeighborHealth, NeighborState},
            ping::Ping,
            route_table::Route,
            testutil,
        },
        async_trait::async_trait,
        fidl_fuchsia_net as fnet, fuchsia_async as fasync,
        fuchsia_inspect::assert_data_tree,
        futures::StreamExt as _,
        net_declare::{fidl_ip, fidl_subnet, std_ip, std_socket_addr},
        net_types::ip,
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
    fn test_log_state_vals_inspect() {
        let inspector = Inspector::default();
        State::log_state_vals_inspect(inspector.root(), "state_vals");
        assert_data_tree!(inspector, root: {
            state_vals: {
                "1": "None",
                "5": "Removed",
                "10": "Down",
                "15": "Up",
                "20": "Local",
                "25": "Gateway",
                "30": "Internet",
            }
        })
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

    #[test_case(NetworkCheckState::PingGateway, &[std_socket_addr!("1.2.3.0:8080")];
        "gateway ping on ipv4")]
    #[test_case(NetworkCheckState::PingGateway, &[std_socket_addr!("[123::]:0")];
        "gateway ping on ipv6")]
    #[test_case(NetworkCheckState::PingGateway, &[std_socket_addr!("1.2.3.0:8080"),
        std_socket_addr!("[123::]:0")]; "gateway ping on ipv4/ipv6")]
    #[test_case(NetworkCheckState::PingInternet, &[std_socket_addr!("8.8.8.8:0")];
        "internet ping on ipv4")]
    #[test_case(NetworkCheckState::PingInternet, &[std_socket_addr!("[2001:4860:4860::8888]:0")];
        "internet ping on ipv6")]
    #[test_case(NetworkCheckState::PingInternet, &[std_socket_addr!("8.8.8.8:0"),
        std_socket_addr!("[2001:4860:4860::8888]:0")]; "internet ping on ipv4/ipv6")]
    fn test_handle_ping_success(checker_state: NetworkCheckState, addrs: &[std::net::SocketAddr]) {
        let mut expected_state_v4: State = Default::default();
        let mut expected_state_v6: State = Default::default();

        let mut ctx = NetworkCheckContext { checker_state, ..Default::default() };
        // Initial state.
        assert_eq!(ctx.discovered_state_v4, expected_state_v4);
        assert_eq!(ctx.discovered_state_v6, expected_state_v6);

        let expected_state = match ctx.checker_state {
            NetworkCheckState::PingGateway => State::Gateway,
            NetworkCheckState::PingInternet => State::Internet,
            NetworkCheckState::Begin | NetworkCheckState::Idle => Default::default(),
        };

        addrs.iter().for_each(|addr| {
            // Run the function under test for each address.
            let () = Monitor::handle_ping_success(&mut ctx, addr);
            // Update the expected values accordingly.
            match addr {
                std::net::SocketAddr::V4 { .. } => {
                    expected_state_v4 = expected_state;
                }
                std::net::SocketAddr::V6 { .. } => {
                    expected_state_v6 = expected_state;
                }
            }
        });
        // Final state.
        assert_eq!(ctx.discovered_state_v4, expected_state_v4);
        assert_eq!(ctx.discovered_state_v6, expected_state_v6);
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

    struct NetworkCheckTestResponder {
        receiver: mpsc::UnboundedReceiver<(NetworkCheckAction, NetworkCheckCookie)>,
    }

    impl NetworkCheckTestResponder {
        fn new(
            receiver: mpsc::UnboundedReceiver<(NetworkCheckAction, NetworkCheckCookie)>,
        ) -> Self {
            Self { receiver }
        }

        async fn respond_to_messages<P: Ping>(&mut self, monitor: &mut Monitor, p: P) {
            loop {
                if let Some((action, cookie)) = self.receiver.next().await {
                    match action {
                        NetworkCheckAction::Ping { interface_name, addr } => {
                            let ping_success = p.ping(&interface_name, addr).await;
                            match monitor.resume(cookie, ping_success) {
                                // Has reached final state.
                                Ok(NetworkCheckerOutcome::Complete) => return,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    fn run_network_check_partial_properties<P: Ping>(
        exec: &mut fasync::TestExecutor,
        name: &str,
        interface_id: u64,
        routes: &RouteTable,
        pinger: P,
        neighbors: Option<&InterfaceNeighborCache>,
        internet_ping_address: std::net::IpAddr,
    ) -> State {
        let properties = &fnet_interfaces_ext::Properties {
            id: interface_id.try_into().expect("should be nonzero"),
            name: name.to_string(),
            device_class: fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::Ethernet,
            ),
            online: true,
            addresses: Default::default(),
            has_default_ipv4_route: Default::default(),
            has_default_ipv6_route: Default::default(),
        };

        match run_network_check(exec, properties, routes, neighbors, pinger) {
            Ok(Some(events)) => {
                // Implementation checks v4 and v6 connectivity concurrently, although these tests
                // only check for a single protocol at a time. The address being pinged determines
                // which protocol to use.
                return match internet_ping_address {
                    std::net::IpAddr::V4 { .. } => events.ipv4.state,
                    std::net::IpAddr::V6 { .. } => events.ipv6.state,
                };
            }
            Ok(None) => {
                error!("id for interface unexpectedly did not exist after network check");
                State::None
            }
            Err(e) => {
                error!("network check had an issue calculating state: {:?}", e);
                State::None
            }
        }
    }

    fn run_network_check<P: Ping>(
        exec: &mut fasync::TestExecutor,
        properties: &fnet_interfaces_ext::Properties,
        routes: &RouteTable,
        neighbors: Option<&InterfaceNeighborCache>,
        pinger: P,
    ) -> Result<Option<IpVersions<StateEvent>>, anyhow::Error> {
        let (sender, receiver) = mpsc::unbounded::<(NetworkCheckAction, NetworkCheckCookie)>();
        let mut monitor = Monitor::new(sender).unwrap();
        let mut network_check_responder = NetworkCheckTestResponder::new(receiver);

        let view = InterfaceView { properties, routes, neighbors };
        let network_check_fut = async {
            match monitor.begin(view) {
                Ok(NetworkCheckerOutcome::Complete) => {}
                Ok(NetworkCheckerOutcome::MustResume) => {
                    let () =
                        network_check_responder.respond_to_messages(&mut monitor, pinger).await;
                }
                Err(e) => {
                    error!("begin had an issue calculating state: {:?}", e)
                }
            }
            monitor.state().get(properties.id.get())
        };

        futures::pin_mut!(network_check_fut);
        match exec.run_until_stalled(&mut network_check_fut) {
            Poll::Ready(got) => Ok(got.cloned()),
            Poll::Pending => Err(anyhow::anyhow!("network_check blocked unexpectedly")),
        }
    }

    #[test]
    fn test_network_check_ethernet_ipv4() {
        test_network_check_ethernet::<ip::Ipv4>(
            fidl_ip!("1.2.3.0"),
            fidl_ip!("1.2.3.4"),
            fidl_ip!("1.2.3.1"),
            fidl_ip!("2.2.3.0"),
            fidl_ip!("2.2.3.1"),
            UNSPECIFIED_V4,
            fidl_subnet!("0.0.0.0/1"),
            IPV4_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
            24,
        );
    }

    #[test]
    fn test_network_check_ethernet_ipv6() {
        test_network_check_ethernet::<ip::Ipv6>(
            fidl_ip!("123::"),
            fidl_ip!("123::4"),
            fidl_ip!("123::1"),
            fidl_ip!("223::"),
            fidl_ip!("223::1"),
            UNSPECIFIED_V6,
            fidl_subnet!("::/1"),
            IPV6_INTERNET_CONNECTIVITY_CHECK_ADDRESS,
            64,
        );
    }

    fn test_network_check_ethernet<I: ip::Ip>(
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
        let route_table = testutil::build_route_table_from_flattened_routes([
            Route {
                destination: unspecified_addr,
                outbound_interface: ID1,
                next_hop: Some(net1_gateway),
            },
            Route {
                destination: fnet::Subnet { addr: net1, prefix_len },
                outbound_interface: ID1,
                next_hop: None,
            },
        ]);
        let route_table_2 = testutil::build_route_table_from_flattened_routes([
            Route {
                destination: unspecified_addr,
                outbound_interface: ID1,
                next_hop: Some(net2_gateway),
            },
            Route {
                destination: fnet::Subnet { addr: net1, prefix_len },
                outbound_interface: ID1,
                next_hop: None,
            },
            Route {
                destination: fnet::Subnet { addr: net2, prefix_len },
                outbound_interface: ID1,
                next_hop: None,
            },
        ]);
        let route_table_3 = testutil::build_route_table_from_flattened_routes([
            Route {
                destination: unspecified_addr,
                outbound_interface: ID2,
                next_hop: Some(net1_gateway),
            },
            Route {
                destination: fnet::Subnet { addr: net1, prefix_len },
                outbound_interface: ID2,
                next_hop: None,
            },
        ]);
        let route_table_4 = testutil::build_route_table_from_flattened_routes([
            Route {
                destination: non_default_addr,
                outbound_interface: ID1,
                next_hop: Some(net1_gateway),
            },
            Route {
                destination: fnet::Subnet { addr: net1, prefix_len },
                outbound_interface: ID1,
                next_hop: None,
            },
        ]);

        let fnet_ext::IpAddress(net1_gateway_ext) = net1_gateway.into();
        let mut exec = fasync::TestExecutor::new();

        // TODO(fxrev.dev/120580): Extract test cases into variants/helper function
        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: true,
                },
                None,
                ping_internet_addr,
            ),
            State::Internet,
            "All is good. Can reach internet"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing {
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
                ping_internet_addr,
            ),
            State::Internet,
            "Can reach internet, gateway responding via ARP/ND"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing {
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
            ),
            State::Internet,
            "Gateway not responding via ping or ARP/ND. Can reach internet"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_4,
                FakePing {
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
            ),
            State::Internet,
            "No default route, but healthy gateway with internet/gateway response"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: false,
                },
                None,
                ping_internet_addr,
            ),
            State::Gateway,
            "Can reach gateway via ping"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing::default(),
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
            ),
            State::Gateway,
            "Can reach gateway via ARP/ND"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: false,
                    internet_response: false,
                },
                None,
                ping_internet_addr,
            ),
            State::Local,
            "Local only, Cannot reach gateway"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_2,
                FakePing::default(),
                None,
                ping_internet_addr,
            ),
            State::Local,
            "No default route"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_4,
                FakePing {
                    gateway_addrs: std::iter::once(net1_gateway_ext).collect(),
                    gateway_response: true,
                    internet_response: false,
                },
                None,
                ping_internet_addr,
            ),
            State::Local,
            "No default route, with only gateway response"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_2,
                FakePing::default(),
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
            ),
            State::Local,
            "Local only, neighbors responsive with no default route"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing::default(),
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
            ),
            State::Local,
            "Local only, neighbors responsive with a default route"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_3,
                FakePing::default(),
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
            ),
            State::Local,
            "Local only, neighbors responsive with no routes"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table,
                FakePing::default(),
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
                ping_internet_addr,
            ),
            State::Local,
            "Local only, gateway unhealthy with healthy neighbor"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_3,
                FakePing::default(),
                Some(&InterfaceNeighborCache {
                    neighbors: [(
                        net1_gateway,
                        NeighborState::new(NeighborHealth::Unhealthy { last_healthy: None })
                    )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<fnet::IpAddress, NeighborState>>()
                }),
                ping_internet_addr,
            ),
            State::Up,
            "No routes and unhealthy gateway"
        );

        assert_eq!(
            run_network_check_partial_properties(
                &mut exec,
                ETHERNET_INTERFACE_NAME,
                ID1,
                &route_table_3,
                FakePing::default(),
                None,
                ping_internet_addr,
            ),
            State::Up,
            "No routes",
        );
    }

    #[test]
    fn test_network_check_varying_properties() {
        let properties = fnet_interfaces_ext::Properties {
            id: ID1.try_into().expect("should be nonzero"),
            name: ETHERNET_INTERFACE_NAME.to_string(),
            device_class: fnet_interfaces::DeviceClass::Device(
                fidl_fuchsia_hardware_network::DeviceClass::Ethernet,
            ),
            has_default_ipv4_route: true,
            has_default_ipv6_route: true,
            online: true,
            addresses: vec![
                fnet_interfaces_ext::Address {
                    addr: fidl_subnet!("1.2.3.0/24"),
                    valid_until: fuchsia_zircon::Time::INFINITE.into_nanos(),
                    assignment_state: fnet_interfaces::AddressAssignmentState::Assigned,
                },
                fnet_interfaces_ext::Address {
                    addr: fidl_subnet!("123::4/64"),
                    valid_until: fuchsia_zircon::Time::INFINITE.into_nanos(),
                    assignment_state: fnet_interfaces::AddressAssignmentState::Assigned,
                },
            ],
        };
        let local_routes = testutil::build_route_table_from_flattened_routes([Route {
            destination: fidl_subnet!("1.2.3.0/24"),
            outbound_interface: ID1,
            next_hop: None,
        }]);
        let route_table = testutil::build_route_table_from_flattened_routes([
            Route {
                destination: fidl_subnet!("0.0.0.0/0"),
                outbound_interface: ID1,
                next_hop: Some(fidl_ip!("1.2.3.1")),
            },
            Route {
                destination: fidl_subnet!("::0/0"),
                outbound_interface: ID1,
                next_hop: Some(fidl_ip!("123::1")),
            },
        ]);
        let route_table2 = testutil::build_route_table_from_flattened_routes([
            Route {
                destination: fidl_subnet!("0.0.0.0/0"),
                outbound_interface: ID1,
                next_hop: Some(fidl_ip!("2.2.3.1")),
            },
            Route {
                destination: fidl_subnet!("::0/0"),
                outbound_interface: ID1,
                next_hop: Some(fidl_ip!("223::1")),
            },
        ]);

        const NON_ETHERNET_INTERFACE_NAME: &str = "test01";

        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let time = fasync::Time::from_nanos(1_000_000_000);
        let () = exec.set_fake_time(time.into());

        let got = run_network_check(
            &mut exec,
            &fnet_interfaces_ext::Properties {
                id: ID1.try_into().expect("should be nonzero"),
                name: NON_ETHERNET_INTERFACE_NAME.to_string(),
                device_class: fnet_interfaces::DeviceClass::Device(
                    fidl_fuchsia_hardware_network::DeviceClass::Virtual,
                ),
                online: false,
                has_default_ipv4_route: false,
                has_default_ipv6_route: false,
                addresses: vec![],
            },
            &Default::default(),
            None,
            FakePing::default(),
        )
        .expect(
            "error calling network check with non-ethernet interface, no addresses, interface down",
        );
        assert_eq!(got, Some(IpVersions::construct(StateEvent { state: State::Down, time })));

        let got = run_network_check(
            &mut exec,
            &fnet_interfaces_ext::Properties { online: false, ..properties.clone() },
            &Default::default(),
            None,
            FakePing::default(),
        )
        .expect("error calling network check, want Down state");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Down, time }));
        assert_eq!(got, want);

        let got = run_network_check(
            &mut exec,
            &fnet_interfaces_ext::Properties {
                has_default_ipv4_route: false,
                has_default_ipv6_route: false,
                ..properties.clone()
            },
            &local_routes,
            None,
            FakePing::default(),
        )
        .expect("error calling network check, want Local state due to no default routes");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Local, time }));
        assert_eq!(got, want);

        let got =
            run_network_check(&mut exec, &properties, &route_table2, None, FakePing::default())
                .expect(
                "error calling network check, want Local state due to no matching default route",
            );
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Local, time }));
        assert_eq!(got, want);

        let got = run_network_check(
            &mut exec,
            &properties,
            &route_table,
            None,
            FakePing {
                gateway_addrs: [std_ip!("1.2.3.1"), std_ip!("123::1")].iter().cloned().collect(),
                gateway_response: true,
                internet_response: false,
            },
        )
        .expect("error calling network check, want Gateway state");
        let want =
            Some(IpVersions::<StateEvent>::construct(StateEvent { state: State::Gateway, time }));
        assert_eq!(got, want);

        let got = run_network_check(
            &mut exec,
            &properties,
            &route_table,
            None,
            FakePing {
                gateway_addrs: [std_ip!("1.2.3.1"), std_ip!("123::1")].iter().cloned().collect(),
                gateway_response: true,
                internet_response: true,
            },
        )
        .expect("error calling network check, want Internet state");
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
