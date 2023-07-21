// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fastboot::{get_var, network::tcp::TcpNetworkFactory, open_interface_with_serial},
    logger::{streamer::DiagnosticsStreamer, Logger},
    overnet::host_pipe::{spawn, HostAddr, LogBuffer},
    FASTBOOT_MAX_AGE, MDNS_MAX_AGE, ZEDBOOT_MAX_AGE,
};
use addr::TargetAddr;
use anyhow::{anyhow, bail, Context, Error, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ffx::{TargetAddrInfo, TargetIpPort};
use ffx_daemon_core::events::{self, EventSynthesizer};
use ffx_daemon_events::{FastbootInterface, TargetConnectionState, TargetEvent, TargetInfo};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_ffx::TargetState;
use fidl_fuchsia_developer_remotecontrol::{IdentifyHostResponse, RemoteControlProxy};
use fidl_fuchsia_net::{IpAddress, Ipv4Address, Ipv6Address, Subnet};
use fuchsia_async::Task;
use netext::IsLocalAddr;
use rand::random;
use rcs::{knock_rcs, RcsConnection, RcsConnectionError};
use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeSet, HashSet},
    default::Default,
    fmt,
    fmt::Debug,
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use timeout::timeout;
use usb_bulk::AsyncInterface as Interface;

const IDENTIFY_HOST_TIMEOUT_MILLIS: u64 = 10000;
const DEFAULT_SSH_PORT: u16 = 22;

#[derive(Debug, Clone, Hash)]
pub enum TargetAddrType {
    Ssh,
    Manual(Option<SystemTime>),
    Netsvc,
    Fastboot(FastbootInterface),
}

#[derive(Debug, Clone)]
pub struct TargetAddrEntry {
    pub addr: TargetAddr,
    pub timestamp: DateTime<Utc>,
    pub addr_type: TargetAddrType,
}

impl Hash for TargetAddrEntry {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.addr.hash(state)
    }
}

impl PartialEq for TargetAddrEntry {
    fn eq(&self, other: &Self) -> bool {
        self.addr.eq(&other.addr)
    }
}

impl Eq for TargetAddrEntry {}

impl TargetAddrEntry {
    pub fn new(addr: TargetAddr, timestamp: DateTime<Utc>, addr_type: TargetAddrType) -> Self {
        Self { addr, timestamp, addr_type }
    }
}

/// This imple is intended mainly for testing.
impl From<TargetAddr> for TargetAddrEntry {
    fn from(addr: TargetAddr) -> Self {
        Self { addr, timestamp: Utc::now(), addr_type: TargetAddrType::Ssh }
    }
}

impl Ord for TargetAddrEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.addr.cmp(&other.addr)
    }
}

impl PartialOrd for TargetAddrEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildConfig {
    pub product_config: String,
    pub board_config: String,
}

// TargetEventSynthesizer resolves by weak reference the embedded event
// queue's need for a self reference.
#[derive(Default)]
struct TargetEventSynthesizer {
    target: RefCell<Weak<Target>>,
}

#[async_trait(?Send)]
impl EventSynthesizer<TargetEvent> for TargetEventSynthesizer {
    async fn synthesize_events(&self) -> Vec<TargetEvent> {
        match self.target.borrow().upgrade() {
            Some(target) => match target.get_connection_state() {
                TargetConnectionState::Rcs(_) => vec![TargetEvent::RcsActivated],
                _ => vec![],
            },
            None => vec![],
        }
    }
}

pub struct Target {
    pub events: events::Queue<TargetEvent>,

    pub(crate) host_pipe: RefCell<Option<Task<()>>>,
    logger: Rc<RefCell<Option<Task<()>>>>,

    // id is the locally created "primary identifier" for this target.
    id: u64,
    // ids keeps track of additional ids discovered over Overnet, these could
    // come from old Daemons, or other Daemons. The set should be used
    ids: RefCell<HashSet<u64>>,
    nodename: RefCell<Option<String>>,
    state: RefCell<TargetConnectionState>,
    pub(crate) last_response: RefCell<DateTime<Utc>>,
    pub(crate) addrs: RefCell<BTreeSet<TargetAddrEntry>>,
    // ssh_port if set overrides the global default configuration for ssh port,
    // for this target.
    ssh_port: RefCell<Option<u16>>,
    // used for Fastboot
    pub(crate) serial: RefCell<Option<String>>,
    pub(crate) fastboot_interface: RefCell<Option<FastbootInterface>>,
    pub(crate) build_config: RefCell<Option<BuildConfig>>,
    boot_timestamp_nanos: RefCell<Option<u64>>,
    diagnostics_info: Arc<DiagnosticsStreamer<'static>>,
    host_pipe_log_buffer: Rc<LogBuffer>,

    // The event synthesizer is retained on the target as a strong
    // reference, as the queue only retains a weak reference.
    target_event_synthesizer: Rc<TargetEventSynthesizer>,
    pub(crate) ssh_host_address: RefCell<Option<HostAddr>>,
    // A user provided address that should be used to SSH.
    preferred_ssh_address: RefCell<Option<TargetAddr>>,
}

impl Target {
    pub fn new() -> Rc<Self> {
        let target_event_synthesizer = Rc::new(TargetEventSynthesizer::default());
        let events = events::Queue::new(&target_event_synthesizer);

        let id = random::<u64>();
        let mut ids = HashSet::new();
        ids.insert(id.clone());

        let target = Rc::new(Self {
            id: id.clone(),
            ids: RefCell::new(ids),
            nodename: RefCell::new(None),
            last_response: RefCell::new(Utc::now()),
            state: RefCell::new(Default::default()),
            addrs: RefCell::new(BTreeSet::new()),
            ssh_port: RefCell::new(None),
            serial: RefCell::new(None),
            boot_timestamp_nanos: RefCell::new(None),
            build_config: Default::default(),
            diagnostics_info: Arc::new(DiagnosticsStreamer::default()),
            events,
            host_pipe: Default::default(),
            host_pipe_log_buffer: Rc::new(LogBuffer::new(5)),
            logger: Default::default(),
            target_event_synthesizer,
            fastboot_interface: RefCell::new(None),
            ssh_host_address: RefCell::new(None),
            preferred_ssh_address: RefCell::new(None),
        });
        target.target_event_synthesizer.target.replace(Rc::downgrade(&target));
        target
    }

    pub fn new_named<S>(nodename: S) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        target.nodename.replace(Some(nodename.into()));
        target
    }

    pub fn new_with_boot_timestamp<S>(nodename: S, boot_timestamp_nanos: u64) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new_named(nodename);
        target.boot_timestamp_nanos.replace(Some(boot_timestamp_nanos));
        target
    }

    pub fn new_with_addrs<S>(nodename: Option<S>, addrs: BTreeSet<TargetAddr>) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        target.nodename.replace(nodename.map(Into::into));
        let now = Utc::now();
        target.addrs_extend(
            addrs.iter().map(|addr| TargetAddrEntry::new(*addr, now.clone(), TargetAddrType::Ssh)),
        );
        target
    }

    pub fn new_with_addr_entries<S, I>(nodename: Option<S>, entries: I) -> Rc<Self>
    where
        S: Into<String>,
        I: Iterator<Item = TargetAddrEntry>,
    {
        let target = Self::new();
        target.nodename.replace(nodename.map(Into::into));
        target.addrs.replace(BTreeSet::from_iter(entries));
        target
    }

    pub fn new_with_fastboot_addrs<S>(
        nodename: Option<S>,
        serial: Option<String>,
        addrs: BTreeSet<TargetAddr>,
        interface: FastbootInterface,
    ) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        target.nodename.replace(nodename.map(Into::into));
        target.serial.replace(serial);
        target.addrs.replace(
            addrs
                .iter()
                .map(|e| {
                    TargetAddrEntry::new(
                        *e,
                        Utc::now(),
                        TargetAddrType::Fastboot(interface.clone()),
                    )
                })
                .collect(),
        );
        target.fastboot_interface.replace(Some(interface));
        target.update_connection_state(|_| TargetConnectionState::Fastboot(Instant::now()));
        target
    }

    pub fn new_with_netsvc_addrs<S>(nodename: Option<S>, addrs: BTreeSet<TargetAddr>) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        target.nodename.replace(nodename.map(Into::into));
        target.addrs.replace(
            addrs
                .iter()
                .map(|e| TargetAddrEntry::new(*e, Utc::now(), TargetAddrType::Netsvc))
                .collect(),
        );
        target.update_connection_state(|_| TargetConnectionState::Zedboot(Instant::now()));
        target
    }

    pub fn new_with_serial(serial: &str) -> Rc<Self> {
        let target = Self::new();
        target.serial.replace(Some(serial.to_string()));
        target.update_connection_state(|_| TargetConnectionState::Fastboot(Instant::now()));
        target
    }

    pub fn host_pipe_log_buffer(&self) -> Rc<LogBuffer> {
        self.host_pipe_log_buffer.clone()
    }

    /// Dependency injection constructor so we can insert a fake time for
    /// testing.
    #[cfg(test)]
    pub fn new_with_time<S: Into<String>>(nodename: S, time: DateTime<Utc>) -> Rc<Self> {
        let target = Self::new_named(nodename);
        target.last_response.replace(time);
        target
    }

    pub fn from_target_info(mut t: TargetInfo) -> Rc<Self> {
        if let Some(s) = t.serial {
            Self::new_with_serial(&s)
        } else {
            let res = Self::new_with_addrs(t.nodename.take(), t.addresses.drain(..).collect());
            *res.ssh_host_address.borrow_mut() = t.ssh_host_address.take().map(HostAddr::from);
            *res.ssh_port.borrow_mut() = t.ssh_port;
            res
        }
    }

    pub fn from_netsvc_target_info(mut t: TargetInfo) -> Rc<Self> {
        Self::new_with_netsvc_addrs(t.nodename.take(), t.addresses.drain(..).collect())
    }

    pub fn from_fastboot_target_info(mut t: TargetInfo) -> Result<Rc<Self>> {
        Ok(Self::new_with_fastboot_addrs(
            t.nodename.take(),
            t.serial.take(),
            t.addresses.drain(..).collect(),
            t.fastboot_interface.ok_or(anyhow!("No fastboot mode?"))?,
        ))
    }

    pub fn target_info(&self) -> TargetInfo {
        TargetInfo {
            nodename: self.nodename(),
            addresses: self.addrs(),
            serial: self.serial(),
            ssh_port: self.ssh_port(),
            fastboot_interface: self.fastboot_interface(),
            ssh_host_address: self.ssh_host_address.borrow().as_ref().map(|h| h.to_string()),
        }
    }

    // Get the locally minted identifier for the target
    pub fn id(&self) -> u64 {
        self.id
    }

    // Get all known ids for the target
    pub fn ids(&self) -> HashSet<u64> {
        self.ids.borrow().clone()
    }

    pub fn has_id<'a, I>(&self, ids: I) -> bool
    where
        I: Iterator<Item = &'a u64>,
    {
        let my_ids = self.ids.borrow();
        for id in ids {
            if my_ids.contains(id) {
                return true;
            }
        }
        false
    }

    pub fn merge_ids<'a, I>(&self, new_ids: I)
    where
        I: Iterator<Item = &'a u64>,
    {
        let mut my_ids = self.ids.borrow_mut();
        for id in new_ids {
            my_ids.insert(*id);
        }
    }

    /// ssh_address returns the SocketAddr of the next SSH address to connect to for this target.
    ///
    /// The sort algorithm for SSH address priority is in order of:
    /// - An address that matches the `preferred_ssh_address`.
    /// - Manual addresses first
    ///   - By recency of observation
    /// - Other addresses
    ///   - By link-local first
    ///   - By most recently observed
    ///
    /// The host-pipe connection mechanism will requests addresses from this function on each
    /// connection attempt.
    pub fn ssh_address(&self) -> Option<SocketAddr> {
        use itertools::Itertools;

        // Order e1 & e2 by most recent timestamp
        let recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| e2.timestamp.cmp(&e1.timestamp);

        // Order by link-local first, then by recency
        let link_local_recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| match (
            e1.addr.ip().is_link_local_addr(),
            e2.addr.ip().is_link_local_addr(),
        ) {
            (true, true) | (false, false) => recency(e1, e2),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        };

        let manual_link_local_recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| {
            // If the user specified a preferred address, then use it.
            if let Some(preferred_ssh_address) = *self.preferred_ssh_address.borrow() {
                if e1.addr == preferred_ssh_address {
                    return Ordering::Less;
                }

                if e2.addr == preferred_ssh_address {
                    return Ordering::Greater;
                }
            }

            match (&e1.addr_type, &e2.addr_type) {
                // Note: for manually added addresses, they are ordered strictly
                // by recency, not link-local first.
                (TargetAddrType::Manual(_), TargetAddrType::Manual(_)) => recency(e1, e2),
                (TargetAddrType::Manual(_), TargetAddrType::Ssh) => Ordering::Less,
                (TargetAddrType::Ssh, TargetAddrType::Manual(_)) => Ordering::Greater,
                (TargetAddrType::Ssh, TargetAddrType::Ssh) => link_local_recency(e1, e2),
                _ => Ordering::Less, // Should not get here due to filtering in next line.
            }
        };

        let target_addr = self
            .addrs
            .borrow()
            .iter()
            .filter(|t| match t.addr_type {
                TargetAddrType::Manual(_) | TargetAddrType::Ssh => true,
                _ => false,
            })
            .sorted_by(|e1, e2| manual_link_local_recency(e1, e2))
            .next()
            .map(|e| e.addr);

        target_addr.map(|target_addr| {
            let mut socket_addr: SocketAddr = target_addr.into();
            socket_addr.set_port(self.ssh_port().unwrap_or(DEFAULT_SSH_PORT));
            socket_addr
        })
    }

    pub fn netsvc_address(&self) -> Option<TargetAddr> {
        use itertools::Itertools;
        // Order e1 & e2 by most recent timestamp
        let recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| e2.timestamp.cmp(&e1.timestamp);
        self.addrs
            .borrow()
            .iter()
            .sorted_by(|e1, e2| recency(e1, e2))
            .find(|t| match t.addr_type {
                TargetAddrType::Netsvc => true,
                _ => false,
            })
            .map(|addr_entry| addr_entry.addr.clone())
    }

    pub fn fastboot_address(&self) -> Option<(TargetAddr, FastbootInterface)> {
        use itertools::Itertools;
        // Order e1 & e2 by most recent timestamp
        let recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| e2.timestamp.cmp(&e1.timestamp);
        self.addrs
            .borrow()
            .iter()
            .sorted_by(|e1, e2| recency(e1, e2))
            .find(|t| match t.addr_type {
                TargetAddrType::Fastboot(_) => true,
                _ => false,
            })
            .map(|addr_entry| match addr_entry.addr_type {
                TargetAddrType::Fastboot(ref f) => (addr_entry.addr.clone(), f.clone()),
                _ => unreachable!(),
            })
    }

    pub fn ssh_address_info(&self) -> Option<ffx::TargetAddrInfo> {
        let addr = self.ssh_address()?;
        let ip = match addr.ip() {
            IpAddr::V6(i) => IpAddress::Ipv6(Ipv6Address { addr: i.octets().into() }),
            IpAddr::V4(i) => IpAddress::Ipv4(Ipv4Address { addr: i.octets().into() }),
        };

        let scope_id = match addr {
            SocketAddr::V6(ref v6) => v6.scope_id(),
            _ => 0,
        };

        let port = self.ssh_port().unwrap_or(DEFAULT_SSH_PORT);

        Some(TargetAddrInfo::IpPort(TargetIpPort { ip, port, scope_id }))
    }

    pub fn ssh_host_address_info(&self) -> Option<ffx::SshHostAddrInfo> {
        self.ssh_host_address
            .borrow()
            .as_ref()
            .map(|addr| ffx::SshHostAddrInfo { address: addr.to_string() })
    }

    fn rcs_state(&self) -> ffx::RemoteControlState {
        match (self.is_host_pipe_running(), self.get_connection_state()) {
            (true, TargetConnectionState::Rcs(_)) => ffx::RemoteControlState::Up,
            (true, _) => ffx::RemoteControlState::Down,
            (_, _) => ffx::RemoteControlState::Unknown,
        }
    }

    pub fn nodename(&self) -> Option<String> {
        self.nodename.borrow().clone()
    }

    pub fn nodename_str(&self) -> String {
        self.nodename.borrow().clone().unwrap_or("<unknown>".to_owned())
    }

    pub fn set_nodename(&self, nodename: String) {
        if let Some(current_name) = self.nodename() {
            if nodename != current_name {
                tracing::debug!(
                    "Changing target {} nodename from {current_name} to {nodename}",
                    self.id()
                );
            }
        }
        self.nodename.borrow_mut().replace(nodename);
    }

    pub fn boot_timestamp_nanos(&self) -> Option<u64> {
        self.boot_timestamp_nanos.borrow().clone()
    }

    pub fn update_boot_timestamp(&self, ts: Option<u64>) {
        self.boot_timestamp_nanos.replace(ts);
    }

    pub fn stream_info(&self) -> Arc<DiagnosticsStreamer<'static>> {
        self.diagnostics_info.clone()
    }

    pub fn serial(&self) -> Option<String> {
        self.serial.borrow().clone()
    }

    pub fn state(&self) -> TargetConnectionState {
        self.state.borrow().clone()
    }

    /// Sets the target state (intended to be used for testing only).
    pub fn set_state(&self, state: TargetConnectionState) {
        // Note: Do not mark this function non-test, as it does not
        // enforce state transition control, such as ensuring that
        // manual targets do not enter the disconnected state. It must
        // only be used in tests.
        tracing::debug!(
            "Setting state directly for {name}@{id} from {old:?} to {new:?}",
            name = self.nodename_str(),
            id = self.id(),
            old = self.state(),
            new = state
        );
        self.state.replace(state);
    }

    pub fn get_connection_state(&self) -> TargetConnectionState {
        self.state()
    }

    /// Propose a target connection state transition from the state passed to the provided FnOnce to
    /// the state returned by the FnOnce. Some proposals are adjusted before application, as below.
    /// If the target state reaches RCS, an RcsActivated event is produced. If the proposal results
    /// in a state change, a ConnectionStateChanged event is produced.
    ///
    ///   RCS  ->   MDNS          =>  RCS (does not drop RCS state)
    ///   *    ->   Disconnected  =>  Manual if the device is manual
    #[tracing::instrument(skip(func))]
    pub fn update_connection_state<F>(&self, func: F)
    where
        F: FnOnce(TargetConnectionState) -> TargetConnectionState + Sized,
    {
        let former_state = self.get_connection_state();
        let mut new_state = (func)(former_state.clone());

        match &new_state {
            // A new disconnected state is always observed. Ideally this should only be triggered by
            // a call to .disconnect(). If the target is a manual target, it actually transitions to
            // the manual state.
            TargetConnectionState::Disconnected => {
                if self.is_manual() {
                    let timeout = self.get_manual_timeout();
                    let last_seen = if timeout.is_some() { Some(Instant::now()) } else { None };
                    if former_state.is_rcs() {
                        self.update_last_response(Utc::now());
                        new_state = TargetConnectionState::Manual(last_seen)
                    } else {
                        let current = SystemTime::now();
                        if timeout.is_none() || current < timeout.unwrap() {
                            new_state = TargetConnectionState::Manual(last_seen)
                        }
                    }
                }
            }
            // If a target is observed over mdns, as happens regularly due to broadcasts, or it is
            // re-added manually, if the target is presently in an RCS state, that state is
            // preserved, and the last response time is just adjusted to represent the observation.
            TargetConnectionState::Mdns(_) | TargetConnectionState::Manual(_) => {
                // Do not transition connection state for RCS -> MDNS.
                if former_state.is_rcs() {
                    self.update_last_response(Utc::now());
                    return;
                }
            }
            // If the target is observed in RCS, it is always desirable to transition to that state.
            // If it was already in an RCS state, this could indicate that we missed a peer node ID
            // drop, and perhaps that could be tracked/logged in more detail in future. Ideally we
            // would preserve all potentially active overnet peer id's for a target, however, it's
            // also most likely that a target should only have one overnet peer ID at a time, as it
            // should only have one overnetstack, but it is possible for it to have more than one.
            TargetConnectionState::Rcs(_) => {}
            // The following states are unconditional transitions, as they're states that are
            // difficult to otherwise interrogate, but also states that are known to invalidate all
            // other states.
            TargetConnectionState::Fastboot(_) => {}
            TargetConnectionState::Zedboot(_) => {}
        }

        if former_state == new_state {
            tracing::debug!(
                "State unchanged for {}@{} from {:?}",
                self.nodename_str(),
                self.id(),
                former_state
            );
            return;
        }
        tracing::debug!(
            "Updating state for {}@{} from {:?} to {:?}",
            self.nodename_str(),
            self.id(),
            former_state,
            new_state
        );
        self.state.replace(new_state);

        if self.get_connection_state().is_rcs() {
            self.events.push(TargetEvent::RcsActivated).unwrap_or_else(|err| {
                tracing::warn!("unable to enqueue RCS activation event: {:#}", err)
            });
        }

        self.events
            .push(TargetEvent::ConnectionStateChanged(former_state, self.state.borrow().clone()))
            .unwrap_or_else(|e| {
                tracing::error!("Failed to push state change for {:?}: {:?}", self, e)
            });
    }

    pub fn from_manual_to_tcp_fastboot(&self) {
        let interface = FastbootInterface::Tcp;
        let repl_addr = self
            .addrs
            .borrow()
            .iter()
            .map(|e| {
                TargetAddrEntry::new(
                    e.addr,
                    Utc::now(),
                    TargetAddrType::Fastboot(interface.clone()),
                )
            })
            .collect();
        self.addrs.replace(repl_addr);
        self.fastboot_interface().replace(interface);
        self.update_connection_state(|_| TargetConnectionState::Fastboot(Instant::now()));
    }

    pub fn rcs(&self) -> Option<RcsConnection> {
        match self.get_connection_state() {
            TargetConnectionState::Rcs(conn) => Some(conn),
            _ => None,
        }
    }

    pub fn usb(&self) -> Result<(String, Interface)> {
        match self.serial.borrow().as_ref() {
            Some(s) => Ok((
                s.to_string(),
                open_interface_with_serial(s)
                    .context("Failed to open target usb interface by serial")?,
            )),
            None => Err(anyhow!("No usb serial available to connect to")),
        }
    }

    pub fn last_response(&self) -> DateTime<Utc> {
        self.last_response.borrow().clone()
    }

    pub fn build_config(&self) -> Option<BuildConfig> {
        self.build_config.borrow().clone()
    }

    pub fn addrs(&self) -> Vec<TargetAddr> {
        let mut addrs = self.addrs.borrow().iter().cloned().collect::<Vec<_>>();
        addrs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        addrs.drain(..).map(|e| e.addr).collect()
    }

    pub fn drop_unscoped_link_local_addrs(&self) {
        let mut addrs = self.addrs.borrow_mut();

        *addrs = addrs
            .clone()
            .into_iter()
            .filter(|entry| match (&entry.addr_type, &entry.addr.ip()) {
                (TargetAddrType::Manual(_), _) => true,
                (_, IpAddr::V6(v)) => entry.addr.scope_id() != 0 || !v.is_link_local_addr(),
                _ => true,
            })
            .collect();
    }

    /// Drops all loopback addresses on the target that do not have a port set
    pub fn drop_loopback_addrs(&self) {
        let mut addrs = self.addrs.borrow_mut();

        *addrs = addrs
            .clone()
            .into_iter()
            .filter(|entry| match (&entry.addr_type, &entry.addr.ip(), entry.addr.port()) {
                (TargetAddrType::Manual(_), _, _) => true,
                (_, IpAddr::V4(v), p) => !(v.is_loopback() && p == 0),
                _ => true,
            })
            .collect();
    }

    pub fn overnet_node_id(&self) -> Option<u64> {
        if let TargetConnectionState::Rcs(conn) = self.get_connection_state() {
            Some(conn.overnet_id.id)
        } else {
            None
        }
    }

    pub fn ssh_port(&self) -> Option<u16> {
        self.ssh_port.borrow().clone()
    }

    pub fn fastboot_interface(&self) -> Option<FastbootInterface> {
        self.fastboot_interface.borrow().clone()
    }

    pub fn set_ssh_port(&self, port: Option<u16>) {
        if *self.ssh_port.borrow() != port {
            tracing::debug!(
                "Setting ssh port for {} from {:?} to {:?}",
                self.nodename_str(),
                self.ssh_port.borrow(),
                port
            );
            self.ssh_port.replace(port);
        }
    }

    pub fn manual_addrs(&self) -> Vec<TargetAddr> {
        self.addrs
            .borrow()
            .iter()
            .filter_map(|entry| match entry.addr_type {
                TargetAddrType::Manual(_) => Some(entry.addr.clone()),
                _ => None,
            })
            .collect()
    }

    /// Intended for testing only.
    pub fn addrs_insert(&self, t: TargetAddr) {
        self.addrs.borrow_mut().replace(t.into());
    }

    /// Intended for testing only.
    pub fn new_autoconnected(n: &str) -> Rc<Self> {
        let s = Self::new_named(n);
        s.update_connection_state(|s| {
            assert_eq!(s, TargetConnectionState::Disconnected);
            TargetConnectionState::Mdns(Instant::now())
        });
        s
    }

    /// Intended for testing only.
    pub fn addrs_insert_entry(&self, t: TargetAddrEntry) {
        self.addrs.borrow_mut().replace(t);
    }

    pub(crate) fn addrs_extend<T>(&self, new_addrs: T)
    where
        T: IntoIterator<Item = TargetAddrEntry>,
    {
        let mut addrs = self.addrs.borrow_mut();

        for mut addr in new_addrs.into_iter() {
            // Do not add localhost to the collection during extend.
            // Note: localhost addresses are added sometimes by direct
            // insertion, in the manual add case.
            // IPv4 is allowed so that emulators and tunneled devices are handled correctly.
            let localhost_v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
            if addr.addr.ip() == localhost_v6 {
                continue;
            }

            // Subtle:
            // Some sources of addresses can not be scoped, such as those which come from queries
            // over Overnet.
            // Link-local IPv6 addresses require scopes in order to be routable, and mdns events will
            // provide us with valid scopes. As such, if an incoming address is not scoped, try to
            // find an existing address entry with a scope, and carry the scope forward.
            // If the incoming address has a scope, it is likely to be more recent than one that was
            // originally present, for example if a directly connected USB target has restarted,
            // wherein the scopeid could be incremented due to the device being given a new
            // interface id allocation.
            if addr.addr.ip().is_ipv6() && addr.addr.scope_id() == 0 {
                if let Some(entry) = addrs.get(&addr) {
                    addr.addr.set_scope_id(entry.addr.scope_id());
                }

                // Note: not adding ipv6 link-local addresses without scopes here is deliberate!
                if addr.addr.ip().is_link_local_addr() && addr.addr.scope_id() == 0 {
                    continue;
                }
            }
            addrs.replace(addr);
        }
    }

    pub(crate) fn update_last_response(&self, other: DateTime<Utc>) {
        let mut last_response = self.last_response.borrow_mut();
        if *last_response < other {
            *last_response = other;
        }
    }

    #[tracing::instrument]
    pub fn from_identify(identify: IdentifyHostResponse) -> Result<Rc<Self>, Error> {
        // TODO(raggi): allow targets to truly be created without a nodename.
        let nodename = match identify.nodename {
            Some(n) => n,
            None => bail!("Target identification missing a nodename: {:?}", identify),
        };

        let target = Target::new_named(nodename);
        target.update_last_response(Utc::now().into());
        if let Some(ids) = identify.ids {
            target.merge_ids(ids.iter());
        }
        *target.build_config.borrow_mut() =
            if identify.board_config.is_some() || identify.product_config.is_some() {
                let p = identify.product_config.unwrap_or("<unknown>".to_string());
                let b = identify.board_config.unwrap_or("<unknown>".to_string());
                Some(BuildConfig { product_config: p, board_config: b })
            } else {
                None
            };

        if let Some(serial) = identify.serial_number {
            target.serial.borrow_mut().replace(serial);
        }
        if let Some(t) = identify.boot_timestamp_nanos {
            target.boot_timestamp_nanos.borrow_mut().replace(t);
        }
        if let Some(addrs) = identify.addresses {
            let mut taddrs = target.addrs.borrow_mut();
            let now = Utc::now();
            for addr in addrs.iter().copied().map(|Subnet { addr, prefix_len: _ }| {
                let addr = match addr {
                    IpAddress::Ipv4(Ipv4Address { addr }) => addr.into(),
                    IpAddress::Ipv6(Ipv6Address { addr }) => addr.into(),
                };
                TargetAddrEntry::new(TargetAddr::new(addr, 0, 0), now.clone(), TargetAddrType::Ssh)
            }) {
                taddrs.insert(addr);
            }
        }
        Ok(target)
    }

    #[tracing::instrument]
    pub async fn from_rcs_connection(rcs: RcsConnection) -> Result<Rc<Self>, RcsConnectionError> {
        let identify_result =
            timeout(Duration::from_millis(IDENTIFY_HOST_TIMEOUT_MILLIS), rcs.proxy.identify_host())
                .await
                .map_err(|e| RcsConnectionError::ConnectionTimeoutError(e))?;

        let identify = match identify_result {
            Ok(res) => match res {
                Ok(target) => target,
                Err(e) => return Err(RcsConnectionError::RemoteControlError(e)),
            },
            Err(e) => return Err(RcsConnectionError::FidlConnectionError(e)),
        };
        let target =
            Target::from_identify(identify).map_err(|e| RcsConnectionError::TargetError(e))?;
        target.update_connection_state(move |_| TargetConnectionState::Rcs(rcs));
        Ok(target)
    }

    /// Sets the preferred SSH address.
    ///
    /// Returns true if successful (the `target_addr` exists). Otherwise,
    /// returns false. If the `target_addr` should be used immediately, then
    /// callers should invoke `maybe_reconnect` after calling this method.
    pub fn set_preferred_ssh_address(&self, target_addr: TargetAddr) -> bool {
        let address_exists = self
            .addrs
            .borrow()
            .iter()
            .any(|target_addr_entry| target_addr_entry.addr == target_addr);

        if !address_exists {
            return false;
        }

        self.preferred_ssh_address.borrow_mut().replace(target_addr);
        true
    }

    /// Drops the existing connection (if any) and re-initializes the
    /// `HostPipe`.
    pub fn maybe_reconnect(self: &Rc<Self>) {
        if self.host_pipe.borrow().is_some() {
            drop(self.host_pipe.take());
            tracing::debug!("Reconnecting host_pipe for {}@{}", self.nodename_str(), self.id());
            self.run_host_pipe();
        }
    }

    pub fn clear_preferred_ssh_address(&self) {
        self.preferred_ssh_address.borrow_mut().take();
    }

    #[tracing::instrument]
    pub fn run_host_pipe(self: &Rc<Self>) {
        if self.host_pipe.borrow().is_some() {
            tracing::debug!("Host pipe is already set for {}@{}.", self.nodename_str(), self.id());
            return;
        }

        let weak_target = Rc::downgrade(self);
        let target_name_str = format!("{}@{}", self.nodename_str(), self.id());
        self.host_pipe.borrow_mut().replace(Task::local(async move {
            // The purpose of a host pipe is to ultimately get us connected to RCS and let us transition
            // to the RCS connected state. If we're already in that state, and the RCS connection is
            // active, we don't need a host pipe. This will start happening more as we introduce USB
            // links, where the first thing we hear about a target is its appearance as an Overnet peer,
            // and thus we have an RCS connection from inception.
            {
                let Some(target) = weak_target.upgrade() else {
                    // weird that self is already gone, but ¯\_(ツ)_/¯
                    return;
                };
                let state = target.state.borrow().clone();
                if let TargetConnectionState::Rcs(rcs) = state {
                    if knock_rcs(&rcs.proxy).await.is_ok() {
                        return;
                    }
                }
            }

            let watchdogs: bool =
                ffx_config::get("watchdogs.host_pipe.enabled").await.unwrap_or(false);
            let nr = spawn(weak_target.clone(), watchdogs).await;
            match nr {
                Ok(mut hp) => {
                    tracing::debug!("host pipe spawn returned OK for {target_name_str}");
                    let r = hp.wait().await;
                    // XXX(raggi): decide what to do with this log data:
                    tracing::info!("HostPipeConnection returned: {:?}", r);
                }
                Err(e) => {
                    tracing::warn!("Host pipe spawn {:?}", e);
                }
            }

            weak_target.upgrade().and_then(|target| {
                tracing::debug!("Exiting run_host_pipe for {target_name_str}");
                target.host_pipe.borrow_mut().take()
            });
        }));
    }

    pub fn is_host_pipe_running(&self) -> bool {
        self.host_pipe.borrow().is_some()
    }

    #[tracing::instrument]
    pub fn run_logger(self: &Rc<Self>) {
        if self.logger.borrow().is_none() {
            let logger = Rc::downgrade(&self.logger);
            let weak_target = Rc::downgrade(self);
            self.logger.replace(Some(Task::local(async move {
                let r = Logger::new(weak_target).start().await;
                // XXX(raggi): decide what to do with this log data:
                tracing::info!("Logger returned: {:?}", r);
                logger.upgrade().and_then(|logger| logger.replace(None));
            })));
        }
    }

    pub fn is_logger_running(&self) -> bool {
        self.logger.borrow().is_some()
    }

    #[tracing::instrument]
    pub async fn init_remote_proxy(self: &Rc<Self>) -> Result<RemoteControlProxy> {
        // Ensure auto-connect has at least started.
        self.run_host_pipe();
        match self.events.wait_for(None, |e| e == TargetEvent::RcsActivated).await {
            Ok(()) => (),
            Err(e) => {
                tracing::warn!("{}", e);
                bail!("RCS connection issue")
            }
        }
        self.rcs().ok_or(anyhow!("rcs dropped after event fired")).map(|r| r.proxy)
    }

    pub async fn is_fastboot_tcp(&self) -> Result<bool> {
        let mut factory = TcpNetworkFactory::new();
        let mut interface = factory.open_with_retry(self, 1, 1).await?;
        // Dont care what the result is, just need to get it
        let _result = get_var(&mut interface, &"version".to_string()).await?;
        Ok(true)
    }

    /// Check the current target state, and if it is a state that expires (such
    /// as mdns) perform the appropriate state transition. The daemon target
    /// collection expiry loop calls this function regularly.
    #[tracing::instrument]
    pub fn expire_state(&self) {
        self.update_connection_state(|current_state| {
            let expire_duration = match current_state {
                TargetConnectionState::Mdns(_) => MDNS_MAX_AGE,
                TargetConnectionState::Fastboot(_) => FASTBOOT_MAX_AGE,
                TargetConnectionState::Zedboot(_) => ZEDBOOT_MAX_AGE,
                TargetConnectionState::Manual(_) => MDNS_MAX_AGE,
                _ => Duration::default(),
            };

            let new_state = match &current_state {
                TargetConnectionState::Mdns(ref last_seen)
                | TargetConnectionState::Fastboot(ref last_seen)
                | TargetConnectionState::Zedboot(ref last_seen)
                    if last_seen.elapsed() > expire_duration =>
                {
                    Some(TargetConnectionState::Disconnected)
                }
                TargetConnectionState::Manual(ref last_seen)
                    if last_seen.map(|time| time.elapsed() > expire_duration).unwrap_or(false) =>
                {
                    Some(TargetConnectionState::Disconnected)
                }
                _ => None,
            };

            if let Some(ref new_state) = new_state {
                tracing::debug!(
                    "Target {:?} state {:?} => {:?} due to expired state after {:?}.",
                    self,
                    &current_state,
                    new_state,
                    expire_duration
                );
            }

            new_state.unwrap_or(current_state)
        });
    }

    pub fn is_connected(&self) -> bool {
        self.state.borrow().is_connected()
    }

    pub fn is_manual(&self) -> bool {
        self.addrs
            .borrow()
            .iter()
            .any(|addr_entry| matches!(addr_entry.addr_type, TargetAddrType::Manual(_)))
    }

    pub fn get_manual_timeout(&self) -> Option<SystemTime> {
        let addrs = self.addrs.borrow();
        let entry = addrs
            .iter()
            .find(|addr_entry| matches!(addr_entry.addr_type, TargetAddrType::Manual(_)))?;
        match entry.addr_type {
            TargetAddrType::Manual(timeout) => timeout,
            _ => None,
        }
    }

    pub fn disconnect(&self) {
        drop(self.host_pipe.take());
        tracing::debug!("Disconnecting host_pipe for {}@{}", self.nodename_str(), self.id());
        self.update_connection_state(|_| TargetConnectionState::Disconnected);
    }
}

impl From<&Target> for ffx::TargetInfo {
    #[tracing::instrument]
    fn from(target: &Target) -> Self {
        let (product_config, board_config) = target
            .build_config()
            .map(|b| (Some(b.product_config), Some(b.board_config)))
            .unwrap_or((None, None));

        Self {
            nodename: target.nodename(),
            serial_number: target.serial(),
            addresses: Some(target.addrs().into_iter().map(|a| a.into()).collect()),
            age_ms: Some(match Utc::now()
                .signed_duration_since(target.last_response())
                .num_milliseconds()
            {
                dur if dur < 0 => {
                    tracing::trace!(
                        "negative duration encountered on target '{}': {}",
                        target.nodename_str(),
                        dur
                    );
                    0
                }
                dur => dur,
            } as u64),
            product_config,
            board_config,
            rcs_state: Some(target.rcs_state()),
            target_state: Some(match target.state() {
                TargetConnectionState::Disconnected => TargetState::Disconnected,
                TargetConnectionState::Manual(_)
                | TargetConnectionState::Mdns(_)
                | TargetConnectionState::Rcs(_) => TargetState::Product,
                TargetConnectionState::Fastboot(_) => TargetState::Fastboot,
                TargetConnectionState::Zedboot(_) => TargetState::Zedboot,
            }),
            ssh_address: target.ssh_address_info(),
            // TODO(awdavies): Gather more information here when possible.
            target_type: Some(ffx::TargetType::Unknown),
            ssh_host_address: target.ssh_host_address_info(),
            ..Default::default()
        }
    }
}

impl Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("id", &self.id)
            .field("ids", &self.ids.borrow().clone())
            .field("nodename", &self.nodename.borrow().clone())
            .field("state", &self.state.borrow().clone())
            .field("last_response", &self.last_response.borrow().clone())
            .field("addrs", &self.addrs.borrow().clone())
            .field("ssh_port", &self.ssh_port.borrow().clone())
            .field("serial", &self.serial.borrow().clone())
            .field("boot_timestamp_nanos", &self.boot_timestamp_nanos.borrow().clone())
            // TODO(raggi): add task fields
            .finish()
    }
}

/// Convert a TargetAddrInfo to a SocketAddr preserving the port number if
/// provided, otherwise the returned SocketAddr will have port number 0.
pub fn target_addr_info_to_socketaddr(tai: TargetAddrInfo) -> SocketAddr {
    let mut sa = SocketAddr::from(TargetAddr::from(&tai));
    // TODO(raggi): the port special case needed here indicates a general problem in our
    // addressing strategy that is worth reviewing.
    if let TargetAddrInfo::IpPort(ref ipp) = tai {
        sa.set_port(ipp.port)
    }
    sa
}

#[cfg(test)]
pub(crate) fn clone_target(target: &Target) -> Rc<Target> {
    let new = Target::new();
    new.nodename.replace(target.nodename());
    // Note: ID is omitted deliberately, as ID merging is unconditional on
    // match, which breaks some uses of this helper function.
    new.ids.replace(target.ids.borrow().clone());
    new.state.replace(target.state.borrow().clone());
    new.addrs.replace(target.addrs.borrow().clone());
    new.ssh_port.replace(target.ssh_port.borrow().clone());
    new.serial.replace(target.serial.borrow().clone());
    new.boot_timestamp_nanos.replace(target.boot_timestamp_nanos.borrow().clone());
    new.build_config.replace(target.build_config.borrow().clone());
    new.last_response.replace(target.last_response.borrow().clone());
    // TODO(raggi): there are missing fields here, as there were before the
    // refactor in which I introduce this comment. It should be a goal to
    // remove this helper function over time.
    new
}

#[cfg(test)]
impl PartialEq for Target {
    fn eq(&self, o: &Target) -> bool {
        self.nodename() == o.nodename()
            && *self.last_response.borrow() == *o.last_response.borrow()
            && self.addrs() == o.addrs()
            && *self.state.borrow() == *o.state.borrow()
            && self.build_config() == o.build_config()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use chrono::TimeZone;
    use ffx::TargetIp;
    use fidl;
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
    use fidl_fuchsia_overnet_protocol::NodeId;
    use fuchsia_async::Timer;
    use futures::{channel, prelude::*};
    use hoist::Hoist;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

    const DEFAULT_PRODUCT_CONFIG: &str = "core";
    const DEFAULT_BOARD_CONFIG: &str = "x64";
    const TEST_SERIAL: &'static str = "test-serial";

    fn setup_fake_remote_control_service(
        send_internal_error: bool,
        nodename_response: String,
    ) -> RemoteControlProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>().unwrap();

        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    rcs::RemoteControlRequest::IdentifyHost { responder } => {
                        if send_internal_error {
                            let _ = responder
                                .send(Err(rcs::IdentifyHostError::ListInterfacesFailed))
                                .context("sending testing error response")
                                .unwrap();
                        } else {
                            let result = vec![Subnet {
                                addr: IpAddress::Ipv4(Ipv4Address { addr: [192, 168, 0, 1] }),
                                prefix_len: 24,
                            }];
                            let serial = String::from(TEST_SERIAL);
                            let nodename = if nodename_response.len() == 0 {
                                None
                            } else {
                                Some(nodename_response.clone())
                            };
                            responder
                                .send(Ok(&rcs::IdentifyHostResponse {
                                    nodename,
                                    serial_number: Some(serial),
                                    addresses: Some(result),
                                    product_config: Some(DEFAULT_PRODUCT_CONFIG.to_owned()),
                                    board_config: Some(DEFAULT_BOARD_CONFIG.to_owned()),
                                    ..Default::default()
                                }))
                                .context("sending testing response")
                                .unwrap();
                        }
                    }
                    _ => assert!(false),
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_unresponsive_remote_control_service(
        done: channel::oneshot::Receiver<()>,
    ) -> RemoteControlProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>().unwrap();

        fuchsia_async::Task::local(async move {
            while let Ok(Some(_req)) = stream.try_next().await {}
            // Dropping the request immediately would also drop the responder,
            // resulting in the other side receiving a PEER_CLOSED
            let _ = done.await;
        })
        .detach();

        proxy
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_from_rcs_connection_internal_err() {
        let local_hoist = Hoist::new().unwrap();

        // TODO(awdavies): Do some form of PartialEq implementation for
        // the RcsConnectionError enum to avoid the nested matches.
        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(true, "foo".to_owned()),
            &NodeId { id: 123 },
        );
        match Target::from_rcs_connection(conn).await {
            Ok(_) => assert!(false),
            Err(e) => match e {
                RcsConnectionError::RemoteControlError(rce) => match rce {
                    rcs::IdentifyHostError::ListInterfacesFailed => (),
                    _ => assert!(false),
                },
                _ => assert!(false),
            },
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_from_rcs_connection_nodename_none() {
        let local_hoist = Hoist::new().unwrap();

        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "".to_owned()),
            &NodeId { id: 123456 },
        );
        match Target::from_rcs_connection(conn).await {
            Ok(_) => assert!(false),
            Err(e) => match e {
                RcsConnectionError::TargetError(_) => (),
                _ => assert!(false),
            },
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_from_rcs_connection_no_err() {
        let local_hoist = Hoist::new().unwrap();

        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "foo".to_owned()),
            &NodeId { id: 1234 },
        );
        match Target::from_rcs_connection(conn).await {
            Ok(t) => {
                assert_eq!(t.nodename().unwrap(), "foo".to_string());
                assert_eq!(t.rcs().unwrap().overnet_id.id, 1234u64);
                assert_eq!(t.addrs().len(), 1);
                assert_eq!(
                    t.build_config().unwrap(),
                    BuildConfig {
                        product_config: DEFAULT_PRODUCT_CONFIG.to_string(),
                        board_config: DEFAULT_BOARD_CONFIG.to_string()
                    }
                );
                assert_eq!(t.serial().unwrap(), String::from(TEST_SERIAL));
            }
            Err(_) => assert!(false),
        }
    }

    // Most of this is now handled in `task.rs`
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_disconnect_multiple_invocations() {
        let t = Rc::new(Target::new_named("flabbadoobiedoo"));
        {
            let addr: TargetAddr = TargetAddr::new(IpAddr::from([192, 168, 0, 1]), 0, 0);
            t.addrs_insert(addr);
        }
        // Assures multiple "simultaneous" invocations to start the target
        // doesn't put it into a bad state that would hang.
        t.run_host_pipe();
        t.run_host_pipe();
        t.run_host_pipe();
    }

    struct RcsStateTest {
        loop_started: bool,
        rcs_is_some: bool,
        expected: ffx::RemoteControlState,
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_rcs_states() {
        let local_hoist = Hoist::new().unwrap();

        for test in vec![
            RcsStateTest {
                loop_started: true,
                rcs_is_some: false,
                expected: ffx::RemoteControlState::Down,
            },
            RcsStateTest {
                loop_started: true,
                rcs_is_some: true,
                expected: ffx::RemoteControlState::Up,
            },
            RcsStateTest {
                loop_started: false,
                rcs_is_some: true,
                expected: ffx::RemoteControlState::Unknown,
            },
            RcsStateTest {
                loop_started: false,
                rcs_is_some: false,
                expected: ffx::RemoteControlState::Unknown,
            },
        ] {
            let t = Target::new_named("schlabbadoo");
            let a2 = IpAddr::V6(Ipv6Addr::new(
                0xfe80, 0xcafe, 0xf00d, 0xf000, 0xb412, 0xb455, 0x1337, 0xfeed,
            ));
            t.addrs_insert(TargetAddr::new(a2, 2, 0));
            if test.loop_started {
                t.run_host_pipe();
            }
            {
                *t.state.borrow_mut() = if test.rcs_is_some {
                    TargetConnectionState::Rcs(RcsConnection::new_with_proxy(
                        &local_hoist,
                        setup_fake_remote_control_service(true, "foobiedoo".to_owned()),
                        &NodeId { id: 123 },
                    ))
                } else {
                    TargetConnectionState::Disconnected
                };
            }
            assert_eq!(t.rcs_state(), test.expected);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_into_bridge_target() {
        let t = Target::new_named("cragdune-the-impaler");
        let a1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let a2 = IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0xcafe, 0xf00d, 0xf000, 0xb412, 0xb455, 0x1337, 0xfeed,
        ));
        *t.build_config.borrow_mut() = Some(BuildConfig {
            board_config: DEFAULT_BOARD_CONFIG.to_owned(),
            product_config: DEFAULT_PRODUCT_CONFIG.to_owned(),
        });
        t.addrs_insert(TargetAddr::new(a1, 1, 0));
        t.addrs_insert(TargetAddr::new(a2, 1, 0));

        let t_conv: ffx::TargetInfo = t.as_ref().into();
        assert_eq!(t.nodename().unwrap(), t_conv.nodename.unwrap().to_string());
        let addrs = t.addrs();
        let conv_addrs = t_conv.addresses.unwrap();
        assert_eq!(addrs.len(), conv_addrs.len());

        // Will crash if any addresses are missing.
        for address in conv_addrs {
            let address = TargetAddr::from(address);
            assert!(addrs.iter().any(|&a| a == address));
        }
        assert_eq!(t_conv.board_config.unwrap(), DEFAULT_BOARD_CONFIG.to_owned(),);
        assert_eq!(t_conv.product_config.unwrap(), DEFAULT_PRODUCT_CONFIG.to_owned(),);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_event_synthesis_wait() {
        let local_hoist = Hoist::new().unwrap();

        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "foo".to_owned()),
            &NodeId { id: 1234 },
        );
        let t = match Target::from_rcs_connection(conn).await {
            Ok(t) => {
                assert_eq!(t.nodename().unwrap(), "foo".to_string());
                assert_eq!(t.rcs().unwrap().overnet_id.id, 1234u64);
                assert_eq!(t.addrs().len(), 1);
                t
            }
            Err(_) => unimplemented!("this branch should never happen"),
        };
        // This will hang forever if no synthesis happens.
        t.events.wait_for(None, |e| e == TargetEvent::RcsActivated).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_event_fire() {
        let local_hoist = Hoist::new().unwrap();

        let t = Target::new_named("balaowihf");
        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "balaowihf".to_owned()),
            &NodeId { id: 1234 },
        );

        let fut = t.events.wait_for(None, |e| e == TargetEvent::RcsActivated);
        t.update_connection_state(|_| TargetConnectionState::Rcs(conn));
        fut.await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_update_connection_state() {
        let t = Target::new_named("have-you-seen-my-cat");
        let instant = Instant::now();
        let instant_clone = instant.clone();
        t.update_connection_state(move |s| {
            assert_eq!(s, TargetConnectionState::Disconnected);

            TargetConnectionState::Mdns(instant_clone)
        });
        assert_eq!(TargetConnectionState::Mdns(instant), t.get_connection_state());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_connection_state_will_not_drop_rcs_on_mdns_events() {
        let local_hoist = Hoist::new().unwrap();

        let t = Target::new_named("hello-kitty");
        let rcs_state = TargetConnectionState::Rcs(
            RcsConnection::new(local_hoist.clone(), &mut NodeId { id: 1234 }).unwrap(),
        );
        t.set_state(rcs_state.clone());

        // Attempt to set the state to TargetConnectionState::Mdns, this transition should fail, as in
        // this transition RCS should be retained.
        t.update_connection_state(|_| TargetConnectionState::Mdns(Instant::now()));

        assert_eq!(t.get_connection_state(), rcs_state);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_connection_state_will_not_drop_rcs_on_manual_events() {
        let local_hoist = Hoist::new().unwrap();

        let t = Target::new_named("hello-kitty");
        let rcs_state = TargetConnectionState::Rcs(
            RcsConnection::new(local_hoist.clone(), &mut NodeId { id: 1234 }).unwrap(),
        );
        t.set_state(rcs_state.clone());

        // Attempt to set the state to TargetConnectionState::Manual, this transition should fail, as in
        // this transition RCS should be retained.
        t.update_connection_state(|_| TargetConnectionState::Manual(None));

        assert_eq!(t.get_connection_state(), rcs_state);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expire_state_mdns() {
        let t = Target::new_named("yo-yo-ma-plays-that-cello-ya-hear");
        let then = Instant::now() - (MDNS_MAX_AGE + Duration::from_secs(1));
        t.update_connection_state(|_| TargetConnectionState::Mdns(then));

        t.expire_state();

        t.events
            .wait_for(None, move |e| {
                e == TargetEvent::ConnectionStateChanged(
                    TargetConnectionState::Mdns(then),
                    TargetConnectionState::Disconnected,
                )
            })
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expire_state_fastboot() {
        let t = Target::new_named("platypodes-are-venomous");
        let then = Instant::now() - (FASTBOOT_MAX_AGE + Duration::from_secs(1));
        t.update_connection_state(|_| TargetConnectionState::Fastboot(then));

        t.expire_state();

        t.events
            .wait_for(None, move |e| {
                e == TargetEvent::ConnectionStateChanged(
                    TargetConnectionState::Fastboot(then),
                    TargetConnectionState::Disconnected,
                )
            })
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expire_state_zedboot() {
        let t = Target::new_named("platypodes-are-venomous");
        let then = Instant::now() - (ZEDBOOT_MAX_AGE + Duration::from_secs(1));
        t.update_connection_state(|_| TargetConnectionState::Zedboot(then));

        t.expire_state();

        t.events
            .wait_for(None, move |e| {
                e == TargetEvent::ConnectionStateChanged(
                    TargetConnectionState::Zedboot(then),
                    TargetConnectionState::Disconnected,
                )
            })
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_addresses_order_preserved() {
        let t = Target::new_named("this-is-a-target-i-guess");
        let addrs_pre = vec![
            SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 0, 0, 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(129, 0, 0, 1), 0)),
            SocketAddr::V6(SocketAddrV6::new("f111::3".parse().unwrap(), 0, 0, 0)),
            SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 0, 0, 0)),
            SocketAddr::V6(SocketAddrV6::new("fe80::2".parse().unwrap(), 0, 0, 2)),
        ];
        let mut addrs_post = addrs_pre
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, e)| {
                TargetAddrEntry::new(
                    TargetAddr::from(e),
                    Utc.ymd(2014 + (i as i32), 10, 31).and_hms(9, 10, 12),
                    TargetAddrType::Ssh,
                )
            })
            .collect::<Vec<TargetAddrEntry>>();
        for a in addrs_post.iter().cloned() {
            t.addrs_insert_entry(a);
        }

        // Removes expected duplicate address. Should be marked as a duplicate
        // and also removed from the very beginning as a more-recent version
        // is added later.
        addrs_post.remove(0);
        // The order should be: last one inserted should show up first.
        addrs_post.reverse();
        assert_eq!(addrs_post.drain(..).map(|e| e.addr).collect::<Vec<_>>(), t.addrs());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_addresses_order() {
        let t = Target::new_named("hi-hi-hi");
        let expected = SocketAddr::V6(SocketAddrV6::new(
            "fe80::4559:49b2:462d:f46b".parse().unwrap(),
            0,
            0,
            8,
        ));
        let addrs_pre = vec![
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 70, 68), 0)),
            expected.clone(),
        ];
        let addrs_post = addrs_pre
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, e)| {
                TargetAddrEntry::new(
                    TargetAddr::from(e),
                    Utc.ymd(2014 + (i as i32), 10, 31).and_hms(9, 10, 12),
                    TargetAddrType::Ssh,
                )
            })
            .collect::<Vec<TargetAddrEntry>>();
        for a in addrs_post.iter().cloned() {
            t.addrs_insert_entry(a);
        }
        assert_eq!(t.addrs().into_iter().next().unwrap(), TargetAddr::from(expected));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_preferred_ssh_address() {
        let target_addr: TargetAddr = TargetAddr::new("fe80::2".parse().unwrap(), 1, 0);
        let target = Target::new_with_addr_entries(
            Some("foo"),
            vec![TargetAddrEntry::new(target_addr, Utc::now(), TargetAddrType::Ssh)].into_iter(),
        );

        assert!(target.set_preferred_ssh_address(target_addr));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_preferred_ssh_address_with_non_existent_address() {
        let target = Target::new_with_addr_entries(
            Some("foo"),
            vec![TargetAddrEntry::new(
                TargetAddr::new("::1".parse::<IpAddr>().unwrap().into(), 0, 0),
                Utc::now(),
                TargetAddrType::Ssh,
            )]
            .into_iter(),
        );

        assert!(!target.set_preferred_ssh_address(TargetAddr::new(
            "fe80::2".parse().unwrap(),
            1,
            0
        )));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_ssh_address_priority() {
        let name = Some("bubba");
        let start = std::time::SystemTime::now();
        use std::iter::FromIterator;

        // An empty set returns nothing.
        let addrs = BTreeSet::<TargetAddrEntry>::new();
        assert_eq!(Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(), None);

        // Given two addresses, from the exact same time, neither manual, prefer any link-local address.
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                start.into(),
                TargetAddrType::Ssh,
            ),
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 2, 0),
                start.into(),
                TargetAddrType::Ssh,
            ),
        ]);
        assert_eq!(
            Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(),
            Some("[fe80::1%2]:22".parse().unwrap())
        );

        // Given two addresses, one link local the other not, prefer the link local even if older.
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                start.into(),
                TargetAddrType::Ssh,
            ),
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 2, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrType::Ssh,
            ),
        ]);
        assert_eq!(
            Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(),
            Some("[fe80::1%2]:22".parse().unwrap())
        );

        // Given two addresses, both link-local, pick the one most recent.
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(
                TargetAddr::new("fe80::2".parse().unwrap(), 1, 0),
                start.into(),
                TargetAddrType::Ssh,
            ),
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 2, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrType::Ssh,
            ),
        ]);
        assert_eq!(
            Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(),
            Some("[fe80::2%1]:22".parse().unwrap())
        );

        // Given two addresses, one manual, old and non-local, prefer the manual entry.
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(
                TargetAddr::new("fe80::2".parse().unwrap(), 1, 0),
                start.into(),
                TargetAddrType::Ssh,
            ),
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrType::Manual(None),
            ),
        ]);
        assert_eq!(
            Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(),
            Some("[2000::1]:22".parse().unwrap())
        );

        // Given two addresses, neither local, neither manual, prefer the most recent.
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                start.into(),
                TargetAddrType::Ssh,
            ),
            TargetAddrEntry::new(
                TargetAddr::new("2000::2".parse().unwrap(), 0, 0),
                (start + Duration::from_secs(1)).into(),
                TargetAddrType::Ssh,
            ),
        ]);
        assert_eq!(
            Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(),
            Some("[2000::2]:22".parse().unwrap())
        );

        let preferred_target_addr: TargetAddr = TargetAddr::new("fe80::2".parse().unwrap(), 1, 0);
        // User expressed a preferred SSH address. Prefer it over all other
        // addresses (even manual).
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(preferred_target_addr, start.into(), TargetAddrType::Ssh),
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrType::Manual(None),
            ),
        ]);

        let target = Target::new_with_addr_entries(name, addrs.into_iter());
        target.set_preferred_ssh_address(preferred_target_addr);
        assert_eq!(target.ssh_address(), Some("[fe80::2%1]:22".parse().unwrap()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_ssh_address_info_no_port_provides_default_port() {
        let target = Target::new_with_addr_entries(
            Some("foo"),
            vec![TargetAddrEntry::new(
                TargetAddr::new("::1".parse::<IpAddr>().unwrap().into(), 0, 0),
                Utc::now(),
                TargetAddrType::Ssh,
            )]
            .into_iter(),
        );

        let (ip, port) = match target.ssh_address_info().unwrap() {
            TargetAddrInfo::IpPort(TargetIpPort { ip, port, .. }) => match ip {
                IpAddress::Ipv4(i) => (IpAddr::from(i.addr), port),
                IpAddress::Ipv6(i) => (IpAddr::from(i.addr), port),
            },
            _ => panic!("unexpected type"),
        };

        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(port, 22);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_ssh_address_info_with_port() {
        let target = Target::new_with_addr_entries(
            Some("foo"),
            vec![TargetAddrEntry::new(
                TargetAddr::new("::1".parse::<IpAddr>().unwrap().into(), 0, 0),
                Utc::now(),
                TargetAddrType::Ssh,
            )]
            .into_iter(),
        );
        target.set_ssh_port(Some(8022));

        let (ip, port) = match target.ssh_address_info().unwrap() {
            TargetAddrInfo::IpPort(TargetIpPort { ip, port, .. }) => match ip {
                IpAddress::Ipv4(i) => (IpAddr::from(i.addr), port),
                IpAddress::Ipv6(i) => (IpAddr::from(i.addr), port),
            },
            _ => panic!("unexpected type"),
        };

        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(port, 8022);
    }

    #[test]
    fn test_target_addr_info_to_socketaddr() {
        let tai = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            port: 8022,
            scope_id: 0,
        });

        let sa = "127.0.0.1:8022".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
        });

        let sa = "127.0.0.1:0".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            port: 8022,
            scope_id: 0,
        });

        let sa = "[::1]:8022".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            scope_id: 1,
        });

        let sa = "[fe80::1%1]:0".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            port: 8022,
            scope_id: 1,
        });

        let sa = "[fe80::1%1]:8022".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_netsvc_target_has_no_ssh() {
        use std::iter::FromIterator;
        let target = Target::new_with_netsvc_addrs(
            Some("foo"),
            BTreeSet::from_iter(
                vec!["[fe80::1%1]:0".parse::<SocketAddr>().unwrap().into()].into_iter(),
            ),
        );
        assert_eq!(target.ssh_address(), None);

        let target = Target::new();
        target.addrs_insert_entry(
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                Utc::now().into(),
                TargetAddrType::Netsvc,
            )
            .into(),
        );
        target.addrs_insert_entry(
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 0, 0),
                Utc::now().into(),
                TargetAddrType::Ssh,
            )
            .into(),
        );
        assert_eq!(target.ssh_address(), Some("[fe80::1%0]:22".parse::<SocketAddr>().unwrap()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_netsvc_ssh_address_info_should_be_none() {
        let ip = "f111::4".parse().unwrap();
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip, 0xbadf00d, 0));
        let target = Target::new_with_netsvc_addrs(Some("foo"), addr_set);

        assert!(target.ssh_address_info().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_is_manual() {
        let target = Target::new();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrType::Manual(None),
        ));
        assert!(target.is_manual());

        let target = Target::new();
        assert!(!target.is_manual());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_get_manual_timeout() {
        let target = Target::new();
        assert_eq!(target.get_manual_timeout(), None);

        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrType::Manual(None),
        ));
        assert!(target.is_manual());
        assert_eq!(target.get_manual_timeout(), None);

        let target = Target::new();
        let now = SystemTime::now();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrType::Manual(Some(now)),
        ));
        assert!(target.is_manual());
        assert_eq!(target.get_manual_timeout(), Some(now));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_update_connection_state_manual_disconnect() {
        let local_hoist = Hoist::new().unwrap();

        let target = Target::new();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrType::Manual(None),
        ));
        target.set_state(TargetConnectionState::Manual(None));

        // Attempting to transition a manual target into the disconnected state remains in manual,
        // if the target has no timeout set.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_matches!(target.get_connection_state(), TargetConnectionState::Manual(_));

        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "abc".to_owned()),
            &NodeId { id: 1234 },
        );
        // A manual target can enter the RCS state.
        target.update_connection_state(|_| TargetConnectionState::Rcs(conn));
        assert_matches!(target.get_connection_state(), TargetConnectionState::Rcs(_));

        // A manual target exiting the RCS state to disconnected returns to manual instead.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_matches!(target.get_connection_state(), TargetConnectionState::Manual(_));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_update_connection_state_expired_ephemeral_disconnect() {
        let local_hoist = Hoist::new().unwrap();

        let target = Target::new();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrType::Manual(Some(SystemTime::now() - Duration::from_secs(3600))),
        ));
        target.set_state(TargetConnectionState::Manual(Some(Instant::now())));

        // Attempting to transition a manual target into the disconnected state can disconnect,
        // if the target has a timeout set and it's expired.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_matches!(target.get_connection_state(), TargetConnectionState::Disconnected);

        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "abc".to_owned()),
            &NodeId { id: 1234 },
        );
        // A manual target can enter the RCS state.
        target.update_connection_state(|_| TargetConnectionState::Rcs(conn));
        assert_matches!(target.get_connection_state(), TargetConnectionState::Rcs(_));

        // A manual target exiting the RCS state to disconnected returns to manual instead.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_matches!(target.get_connection_state(), TargetConnectionState::Manual(_));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_update_connection_state_ephemeral_disconnect() {
        let local_hoist = Hoist::new().unwrap();

        let target = Target::new();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrType::Manual(Some(SystemTime::now() + Duration::from_secs(3600))),
        ));
        target.set_state(TargetConnectionState::Manual(Some(Instant::now())));

        // Attempting to transition a manual target into the disconnected state returns to manual
        // if the target has a timeout set but there's still time before expiry.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_matches!(target.get_connection_state(), TargetConnectionState::Manual(_));

        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_remote_control_service(false, "abc".to_owned()),
            &NodeId { id: 1234 },
        );
        // A manual target can enter the RCS state.
        target.update_connection_state(|_| TargetConnectionState::Rcs(conn));
        assert_matches!(target.get_connection_state(), TargetConnectionState::Rcs(_));

        // A manual target exiting the RCS state to disconnected returns to manual instead.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_matches!(target.get_connection_state(), TargetConnectionState::Manual(_));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_disconnect() {
        let target = Target::new();
        target.set_state(TargetConnectionState::Mdns(Instant::now()));
        target.host_pipe.borrow_mut().replace(Task::local(future::pending()));

        target.disconnect();

        assert_eq!(TargetConnectionState::Disconnected, target.get_connection_state());
        assert!(target.host_pipe.borrow().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_state_borrow() {
        let local_hoist = Hoist::new().unwrap();
        let (done_send, done_recv) = channel::oneshot::channel::<()>();

        let target = Target::new();
        // We want run_host_pipe() to reach the point of knock_rcs(), so we
        // need to set up an RCS first. But we'll set it up so it doesn't respond
        let conn = RcsConnection::new_with_proxy(
            &local_hoist,
            setup_fake_unresponsive_remote_control_service(done_recv),
            &NodeId { id: 1234 },
        );
        target.set_state(TargetConnectionState::Rcs(conn));
        target.run_host_pipe();
        // Let run_host_pipe()'s spawned task run
        Timer::new(Duration::from_millis(50)).await;
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        done_send.send(()).expect("send failed")
        // No assertion -- we are making sure run_host_pipe() doesn't panic
    }
}
