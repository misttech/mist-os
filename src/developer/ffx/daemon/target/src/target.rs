// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::overnet::host_pipe::{
    spawn, HostPipeChildBuilder, HostPipeChildDefaultBuilder, LogBuffer,
};
#[cfg(not(target_os = "macos"))]
use crate::overnet::usb::spawn_usb;
use crate::overnet::vsock::spawn_vsock;
use crate::{FASTBOOT_MAX_AGE, MDNS_MAX_AGE, ZEDBOOT_MAX_AGE};
use addr::{TargetAddr, TargetIpAddr};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use compat_info::{CompatibilityInfo, CompatibilityState};
use ffx::{TargetIpAddrInfo, TargetIpPort};
use ffx_daemon_core::events::{self, EventSynthesizer};
use ffx_daemon_events::{TargetConnectionState, TargetEvent};
use ffx_fastboot_connection_factory::{
    ConnectionFactory, FastbootConnectionFactory, FastbootConnectionKind,
};
use ffx_ssh::parse::HostAddr;
use ffx_target::{Description, FastbootInterface, UNKNOWN_TARGET_NAME};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_ffx::TargetState;
use fidl_fuchsia_developer_remotecontrol::{IdentifyHostResponse, RemoteControlProxy};
use fidl_fuchsia_net::{IpAddress, Ipv4Address, Ipv6Address};
use fuchsia_async::Task;
use futures::channel;
use netext::IsLocalAddr;
use rand::random;
use rcs::{knock_rcs, RcsConnection};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::default::Default;
use std::fmt;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::rc::{Rc, Weak};
use std::sync::Arc;
#[cfg(not(target_os = "macos"))]
use std::sync::Mutex;
use std::time::{Duration, Instant};
use usb_bulk::AsyncInterface as Interface;
use usb_fastboot_discovery::open_interface_with_serial;
#[cfg(not(target_os = "macos"))]
use usb_vsock_host::UsbVsockHost;

mod identity;
mod update;
pub use self::identity::{Identity, IdentityCmp};
pub use self::update::{TargetUpdate, TargetUpdateBuilder};

const DEFAULT_SSH_PORT: u16 = 22;
const CONFIG_HOST_PIPE_SSH_TIMEOUT: &str = "daemon.host_pipe_ssh_timeout";
const CONFIG_ENABLE_VSOCK: &str = "connectivity.enable_vsock";
#[cfg(not(target_os = "macos"))]
const CONFIG_ENABLE_USB: &str = "connectivity.enable_usb";

pub(crate) type SharedIdentity = Rc<Identity>;
pub(crate) type WeakIdentity = Weak<Identity>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TargetAddrStatus {
    // TODO(b/308703153): Add timestamp once the address is turned into a map key.
    pub(crate) protocol: TargetProtocol,
    pub(crate) transport: TargetTransport,
    pub(crate) source: TargetSource,
}

impl TargetAddrStatus {
    pub fn usb() -> Self {
        Self {
            protocol: TargetProtocol::Vsock,
            transport: TargetTransport::Usb,
            source: TargetSource::Discovered,
        }
    }

    pub fn vsock() -> Self {
        Self {
            protocol: TargetProtocol::Vsock,
            transport: TargetTransport::Network,
            source: TargetSource::Discovered,
        }
    }

    pub fn ssh() -> Self {
        Self {
            protocol: TargetProtocol::Ssh,
            transport: TargetTransport::Network,
            source: TargetSource::Discovered,
        }
    }

    pub fn fastboot(interface: TargetTransport) -> Self {
        Self {
            protocol: TargetProtocol::Fastboot,
            transport: interface,
            source: TargetSource::Discovered,
        }
    }

    pub fn netsvc() -> Self {
        Self {
            protocol: TargetProtocol::Netsvc,
            transport: TargetTransport::Network,
            source: TargetSource::Discovered,
        }
    }

    pub fn manually_added(self) -> Self {
        Self { source: TargetSource::Manual, ..self }
    }

    pub fn protocol(&self) -> TargetProtocol {
        self.protocol
    }

    pub fn transport(&self) -> TargetTransport {
        self.transport
    }

    pub fn source(&self) -> TargetSource {
        self.source
    }

    pub fn is_manual(&self) -> bool {
        self.source == TargetSource::Manual
    }
}

#[derive(Debug, Clone)]
pub struct TargetAddrEntry {
    pub addr: TargetAddr,
    pub timestamp: DateTime<Utc>,
    pub(crate) status: TargetAddrStatus,
}

impl std::ops::Deref for TargetAddrEntry {
    type Target = TargetAddrStatus;

    fn deref(&self) -> &Self::Target {
        &self.status
    }
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
    pub fn new(addr: TargetAddr, timestamp: DateTime<Utc>, status: TargetAddrStatus) -> Self {
        Self { addr, timestamp, status }
    }

    pub fn is_ip(&self) -> bool {
        matches!(self.addr, TargetAddr::Net(_))
    }
}

/// This imple is intended mainly for testing.
impl From<TargetAddr> for TargetAddrEntry {
    fn from(addr: TargetAddr) -> Self {
        Self { addr, timestamp: Utc::now(), status: TargetAddrStatus::ssh() }
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TargetProtocol {
    Ssh,
    Vsock,
    Netsvc,
    Fastboot,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TargetTransport {
    Network,
    // Kept to describe Fastboot over UDP.
    // Otherwise we don't need this granularity.
    NetworkUdp,
    Usb,
}

impl Into<FastbootInterface> for TargetTransport {
    fn into(self) -> FastbootInterface {
        match self {
            TargetTransport::Network => FastbootInterface::Tcp,
            TargetTransport::NetworkUdp => FastbootInterface::Udp,
            TargetTransport::Usb => FastbootInterface::Usb,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TargetSource {
    Manual,
    Discovered,
}

impl TargetSource {
    fn merge(&mut self, other: Self) {
        use TargetSource::*;
        *self = match (*self, other) {
            (Discovered, _) => other,
            _ => Manual,
        };
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

pub(crate) enum RemoteOvernetIdState {
    Pending(Vec<channel::oneshot::Sender<Option<u64>>>),
    Ready(Option<u64>),
}
pub(crate) struct HostPipeState {
    pub task: Task<()>,
    pub overnet_node: Arc<overnet_core::Router>,
    pub(crate) ssh_addr: Option<SocketAddr>,
    pub(crate) remote_overnet_id: RemoteOvernetIdState,
}

impl HostPipeState {
    fn flush_waiters(&mut self) {
        if let RemoteOvernetIdState::Pending(waiters) =
            std::mem::replace(&mut self.remote_overnet_id, RemoteOvernetIdState::Ready(None))
        {
            for sender in waiters.into_iter() {
                let _ = sender.send(None);
            }
        } else {
            unreachable!()
        }
    }
}

#[cfg(not(target_os = "macos"))]
static USB_VSOCK_HOST: Mutex<Option<Arc<UsbVsockHost<fuchsia_async::Socket>>>> = Mutex::new(None);

pub struct Target {
    pub events: events::Queue<TargetEvent>,

    pub(crate) host_pipe: RefCell<Option<HostPipeState>>,

    // id is the locally created "primary identifier" for this target.
    id: u64,
    // ids keeps track of additional ids discovered over Overnet, these could
    // come from old Daemons, or other Daemons. The set should be used
    ids: RefCell<HashSet<u64>>,
    // Shared identity across duplicated targets. Not all targets have an identity.
    identity: Cell<Option<SharedIdentity>>,
    state: RefCell<TargetConnectionState>,
    // Discovered targets default to disabled and are marked as enabled during OpenTarget.
    // Manually added targets are always considered enabled.
    // Disabled targets are filtered by default in the public facing TargetCollection API,
    // except within `discover_target`.
    enabled: Cell<bool>,
    // Indicates a target was manually added for a single ffx invocation and can be immediately
    // disabled when not actively used.
    // This transient bit is cleared when the target next transitions out of the `Disconnected`
    // state.
    transient: Cell<bool>,
    keep_alive: Cell<Weak<KeepAliveHandle>>,
    pub(crate) last_response: RefCell<DateTime<Utc>>,
    pub(crate) addrs: RefCell<BTreeSet<TargetAddrEntry>>,
    // ssh_port if set overrides the global default configuration for ssh port,
    // for this target.
    ssh_port: RefCell<Option<u16>>,
    pub(crate) fastboot_interface: RefCell<Option<FastbootInterface>>,
    pub(crate) build_config: RefCell<Option<BuildConfig>>,
    boot_timestamp_nanos: RefCell<Option<u64>>,
    host_pipe_log_buffer: Rc<LogBuffer>,

    // The event synthesizer is retained on the target as a strong
    // reference, as the queue only retains a weak reference.
    target_event_synthesizer: Rc<TargetEventSynthesizer>,
    pub(crate) ssh_host_address: RefCell<Option<HostAddr>>,
    // A user provided address that should be used to SSH.
    preferred_ssh_address: RefCell<Option<TargetIpAddr>>,

    compatibility_status: RefCell<Option<CompatibilityInfo>>,
}

impl Target {
    pub(crate) fn new_with_id(id: u64) -> Rc<Self> {
        let target_event_synthesizer = Rc::new(TargetEventSynthesizer::default());
        let events = events::Queue::new(&target_event_synthesizer);

        let mut ids = HashSet::new();
        ids.insert(id);

        let target = Rc::new(Self {
            id,
            ids: RefCell::new(ids),
            identity: None.into(),
            last_response: RefCell::new(Utc::now()),
            state: RefCell::new(Default::default()),
            enabled: Cell::new(false),
            transient: Cell::new(false),
            keep_alive: Cell::new(Weak::new()),
            addrs: RefCell::new(BTreeSet::new()),
            ssh_port: RefCell::new(None),
            boot_timestamp_nanos: RefCell::new(None),
            build_config: Default::default(),
            events,
            host_pipe: Default::default(),
            host_pipe_log_buffer: Rc::new(LogBuffer::new(5)),
            target_event_synthesizer,
            fastboot_interface: RefCell::new(None),
            ssh_host_address: RefCell::new(None),
            preferred_ssh_address: RefCell::new(None),
            compatibility_status: RefCell::new(None),
        });
        target.target_event_synthesizer.target.replace(Rc::downgrade(&target));
        target
    }

    #[cfg(not(target_os = "macos"))]
    pub fn init_usb_vsock_host(
    ) -> Option<channel::mpsc::Receiver<usb_vsock_host::UsbVsockHostEvent>> {
        if ffx_config::get(CONFIG_ENABLE_USB).unwrap_or(false) {
            let (sender, receiver) = channel::mpsc::channel(1);

            if USB_VSOCK_HOST
                .lock()
                .unwrap()
                .replace(UsbVsockHost::new(Vec::<std::path::PathBuf>::new(), true, sender))
                .is_some()
            {
                log::warn!("Re-initializing USB VSock host");
            }
            Some(receiver)
        } else {
            None
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn get_usb_vsock_host() -> Option<Arc<UsbVsockHost<fuchsia_async::Socket>>> {
        (*USB_VSOCK_HOST.lock().unwrap()).clone()
    }

    pub fn new() -> Rc<Self> {
        Self::new_with_id(random::<u64>())
    }

    pub fn new_named<S>(nodename: S) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        target.replace_identity(Identity::from_name(nodename.into()));
        target
    }

    pub fn new_with_addrs<S>(nodename: Option<S>, addrs: BTreeSet<TargetIpAddr>) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        if let Some(nodename) = nodename {
            target.replace_identity(Identity::from_name(nodename));
        }
        let now = Utc::now();
        target.addrs_extend(
            addrs.iter().map(|addr| {
                TargetAddrEntry::new(addr.into(), now.clone(), TargetAddrStatus::ssh())
            }),
        );
        target
    }

    pub fn new_with_addr_entries<S, I>(nodename: Option<S>, entries: I) -> Rc<Self>
    where
        S: Into<String>,
        I: Iterator<Item = TargetAddrEntry>,
    {
        let target = Self::new();
        if let Some(nodename) = nodename {
            target.replace_identity(Identity::from_name(nodename));
        }
        target.addrs.replace(BTreeSet::from_iter(entries));
        target
    }

    pub fn new_with_fastboot_addrs<S>(
        nodename: Option<S>,
        serial: Option<String>,
        addrs: BTreeSet<TargetIpAddr>,
        interface: FastbootInterface,
    ) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();

        let identity = Identity::try_from_name_serial(nodename, serial);

        if let Some(identity) = identity {
            target.replace_identity(identity);
        }

        let transport = match interface {
            FastbootInterface::Tcp => TargetTransport::Network,
            FastbootInterface::Udp => TargetTransport::NetworkUdp,
            FastbootInterface::Usb => TargetTransport::Usb,
        };

        target.addrs.replace(
            addrs
                .iter()
                .map(|e| {
                    TargetAddrEntry::new(
                        e.into(),
                        Utc::now(),
                        TargetAddrStatus::fastboot(transport),
                    )
                })
                .collect(),
        );
        target.fastboot_interface.replace(Some(interface));
        target.update_connection_state(|_| TargetConnectionState::Fastboot(Instant::now()));
        target
    }

    pub fn new_with_netsvc_addrs<S>(nodename: Option<S>, addrs: BTreeSet<TargetIpAddr>) -> Rc<Self>
    where
        S: Into<String>,
    {
        let target = Self::new();
        if let Some(nodename) = nodename {
            target.replace_identity(Identity::from_name(nodename));
        }
        target.addrs.replace(
            addrs
                .iter()
                .map(|e| TargetAddrEntry::new(e.into(), Utc::now(), TargetAddrStatus::netsvc()))
                .collect(),
        );
        target.update_connection_state(|_| TargetConnectionState::Zedboot(Instant::now()));
        target
    }

    /// Constructs a Target based on the given serial number.
    /// Assumes the Target is in Fastboot mode and connected via USB
    pub fn new_for_usb(serial: &str) -> Rc<Self> {
        let target = Self::new();
        target.replace_identity(Identity::from_serial(serial));
        target.fastboot_interface.replace(Some(FastbootInterface::Usb));
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

    pub fn from_target_event_info(mut t: Description) -> Rc<Self> {
        if let Some(s) = t.serial {
            Self::new_for_usb(&s)
        } else {
            let res = Self::new_with_addrs(
                t.nodename.take(),
                t.addresses.drain(..).filter_map(|x| x.try_into().ok()).collect(),
            );
            *res.ssh_host_address.borrow_mut() = t.ssh_host_address.take().map(HostAddr::from);
            *res.ssh_port.borrow_mut() = t.ssh_port;
            res
        }
    }

    fn infer_fastboot_interface(&self) -> Option<FastbootInterface> {
        match self.fastboot_interface() {
            None => {
                // We take the first address which is of type Fastboot as
                // Fuchsia devices expose only one Fastboot interface
                // at a time.
                if let Some(f_addr) = self
                    .addrs
                    .borrow()
                    .clone()
                    .into_iter()
                    .filter(|addr| matches!(addr.protocol, TargetProtocol::Fastboot))
                    .take(1)
                    .next()
                {
                    Some(f_addr.transport.into())
                } else {
                    // We did not get any addresses that were fastboot addresses
                    // do we have a serial number? If so throw it as USB
                    if self.identity().as_deref().map(Identity::serial).flatten().is_some() {
                        Some(FastbootInterface::Usb)
                    } else {
                        None
                    }
                }
            }
            Some(s) => Some(s),
        }
    }

    pub fn target_info(&self) -> Description {
        let fastboot_interface = self.infer_fastboot_interface();

        Description {
            nodename: self.nodename(),
            addresses: self.addrs(),
            serial: self.serial(),
            ssh_port: self.ssh_port(),
            fastboot_interface,
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

    fn clear_ids(&self) {
        let mut ids = self.ids.borrow_mut();
        ids.clear();
        ids.insert(self.id);
    }

    pub fn has_vsock_or_usb_addr(&self) -> bool {
        self.addrs.borrow().iter().any(|x| !x.is_ip())
    }

    pub(crate) fn replace_identity(&self, ident: Identity) {
        self.identity.replace(Some(Rc::new(ident.into())));
    }

    pub(crate) fn take_identity(&self) -> Option<SharedIdentity> {
        self.identity.take()
    }

    pub(crate) fn replace_shared_identity(&self, ident: SharedIdentity) {
        self.identity.replace(Some(ident));
    }

    pub(crate) fn try_with_identity<F: FnOnce(&SharedIdentity) -> T, T>(&self, f: F) -> Option<T> {
        let id = self.identity.take()?;
        let res = f(&id);
        self.identity.replace(Some(id));
        Some(res)
    }

    pub fn has_identity(&self) -> bool {
        self.try_with_identity(|_| ()).is_some()
    }

    pub fn identity(&self) -> Option<SharedIdentity> {
        self.try_with_identity(Rc::clone)
    }

    pub fn identity_matches(&self, other: &Target) -> bool {
        self.try_with_identity(|self_id| {
            other.try_with_identity(|other_id| self_id.is_same(other_id))
        })
        .flatten()
        .unwrap_or(false)
    }

    pub fn rcs_address(&self) -> Option<SocketAddr> {
        self.host_pipe.borrow().as_ref().and_then(|hp| hp.ssh_addr)
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

        if let Some(addr) = self.rcs_address() {
            return Some(addr);
        }

        // Order e1 & e2 by most recent timestamp
        let recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| e2.timestamp.cmp(&e1.timestamp);

        // Order by link-local first, then by recency
        let link_local_recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| match (
            e1.addr.ip().map(|x| x.is_link_local_addr()).unwrap_or(false),
            e2.addr.ip().map(|x| x.is_link_local_addr()).unwrap_or(false),
        ) {
            (true, true) | (false, false) => recency(e1, e2),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        };

        let manual_link_local_recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| {
            // If the user specified a preferred address, then use it.
            if let Some(preferred_ssh_address) = *self.preferred_ssh_address.borrow() {
                if TargetIpAddr::try_from(e1.addr).ok() == Some(preferred_ssh_address) {
                    return Ordering::Less;
                }

                if TargetIpAddr::try_from(e2.addr).ok() == Some(preferred_ssh_address) {
                    return Ordering::Greater;
                }
            }

            match (e1.is_manual(), e2.is_manual()) {
                // Note: for manually added addresses, they are ordered strictly
                // by recency, not link-local first.
                (true, true) => recency(e1, e2),
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => link_local_recency(e1, e2),
            }
        };

        let target_addr = self
            .addrs
            .borrow()
            .iter()
            .filter(|t| matches!(t.protocol, TargetProtocol::Ssh))
            .sorted_by(|e1, e2| manual_link_local_recency(e1, e2))
            .find_map(|e| TargetIpAddr::try_from(e.addr).map(Into::into).ok());

        target_addr.map(|mut socket_addr: SocketAddr| {
            socket_addr.set_port(self.ssh_port().unwrap_or(DEFAULT_SSH_PORT));
            socket_addr
        })
    }

    pub fn netsvc_address(&self) -> Option<TargetIpAddr> {
        use itertools::Itertools;
        // Order e1 & e2 by most recent timestamp
        let recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| e2.timestamp.cmp(&e1.timestamp);
        self.addrs
            .borrow()
            .iter()
            .sorted_by(|e1, e2| recency(e1, e2))
            .filter(|t| matches!(t.protocol, TargetProtocol::Netsvc))
            .find_map(|addr_entry| addr_entry.addr.try_into().ok())
    }

    pub fn fastboot_address(&self) -> Option<(TargetIpAddr, FastbootInterface)> {
        use itertools::Itertools;
        // Order e1 & e2 by most recent timestamp
        let recency = |e1: &TargetAddrEntry, e2: &TargetAddrEntry| e2.timestamp.cmp(&e1.timestamp);
        self.addrs
            .borrow()
            .iter()
            .sorted_by(|e1, e2| recency(e1, e2))
            .filter(|t| matches!(t.protocol, TargetProtocol::Fastboot))
            .find_map(|addr_entry| {
                addr_entry.addr.try_into().ok().map(|x| (x, addr_entry.transport.into()))
            })
    }

    pub fn ssh_address_info(&self) -> Option<ffx::TargetIpAddrInfo> {
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

        Some(TargetIpAddrInfo::IpPort(TargetIpPort { ip, port, scope_id }))
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
        self.try_with_identity(|id| id.name().map(String::from)).flatten()
    }

    pub fn nodename_str(&self) -> String {
        self.nodename().unwrap_or_else(|| UNKNOWN_TARGET_NAME.to_owned())
    }

    pub fn boot_timestamp_nanos(&self) -> Option<u64> {
        self.boot_timestamp_nanos.borrow().clone()
    }

    pub fn update_boot_timestamp(&self, ts: Option<u64>) {
        self.boot_timestamp_nanos.replace(ts);
    }

    pub fn serial(&self) -> Option<String> {
        self.try_with_identity(|id| id.serial().map(String::from)).flatten()
    }

    pub fn get_compatibility_status(&self) -> Option<CompatibilityInfo> {
        self.compatibility_status.borrow().clone()
    }

    pub fn set_compatibility_status(&self, status: &Option<CompatibilityInfo>) {
        let current_status = self.get_compatibility_status();
        let new_status = match current_status {
            Some(_) => {
                log::debug!(
                    "ignoring status change from id:{} {:?} to {:?}",
                    self.id(),
                    current_status,
                    status
                );
                return;
            }
            None if status.is_some() => {
                // Make compatibility status change more obvious to the user in the info logs.
                // Leave the detailed status struct in the debug logs in case it is needed.
                log::info!(
                    "Compatibility status changed to ['{:#?}'] for target: [{}]",
                    status.as_ref().unwrap().status,
                    self.id()
                );
                log::debug!("{:#?}", status);
                status.clone()
            }
            _ => None,
        };

        self.compatibility_status.replace(new_status);
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
        log::debug!(
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
                self.clear_ids();
                if self.is_manual() {
                    let last_seen = None;
                    if former_state.is_rcs() {
                        self.update_last_response(Utc::now());
                        new_state = TargetConnectionState::Manual;
                    } else {
                        new_state = match former_state {
                            TargetConnectionState::Fastboot(old_last_seen) => {
                                TargetConnectionState::Fastboot(last_seen.unwrap_or(old_last_seen))
                            }
                            TargetConnectionState::Zedboot(old_last_seen) => {
                                TargetConnectionState::Zedboot(last_seen.unwrap_or(old_last_seen))
                            }
                            _ => TargetConnectionState::Manual,
                        };
                    }
                }
            }
            // If a target is observed over mdns, as happens regularly due to broadcasts, or it is
            // re-added manually, if the target is presently in an RCS state, that state is
            // preserved, and the last response time is just adjusted to represent the observation.
            TargetConnectionState::Mdns(_)
            | TargetConnectionState::Manual
            | TargetConnectionState::Vsock(_) => {
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
            TargetConnectionState::Fastboot(_) | TargetConnectionState::Zedboot(_) => {
                self.update_last_response(Utc::now());
            }
        }

        if former_state == new_state {
            log::trace!(
                "State unchanged for {}@{} from {:?}",
                self.nodename_str(),
                self.id(),
                former_state
            );
            return;
        }

        log::debug!(
            "Updating state for {}@{} from {:?} to {:?}",
            self.nodename_str(),
            self.id(),
            former_state,
            new_state
        );

        let rediscovered = matches!(former_state, TargetConnectionState::Disconnected);
        let expired = matches!(new_state, TargetConnectionState::Disconnected);

        self.state.replace(new_state);

        if rediscovered {
            // If the target is going from the disconnected state to discovered, clear the transient
            // flag.
            if self.transient.get() {
                log::debug!("Cleared transient flag for target connection state transition");
                self.transient.set(false);
            }
        } else if expired && self.transient.get() && self.is_enabled() {
            log::debug!("Enabled transient target expired, closing...");
            self.disable();
        }

        if self.get_connection_state().is_rcs() {
            self.events.push(TargetEvent::RcsActivated).unwrap_or_else(|err| {
                log::warn!("unable to enqueue RCS activation event: {:#}", err)
            });
        }

        self.events
            .push(TargetEvent::ConnectionStateChanged(former_state, self.state.borrow().clone()))
            .unwrap_or_else(|e| log::error!("Failed to push state change for {:?}: {:?}", self, e));
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
                    TargetAddrStatus::fastboot(TargetTransport::Network).manually_added(),
                )
            })
            .collect();
        self.addrs.replace(repl_addr);
        self.fastboot_interface().replace(interface);
        self.update_connection_state(|_| TargetConnectionState::Fastboot(Instant::now()));

        // Persist the enabled bit since the target is no longer "manually added" at this point.
        self.enable();
    }

    pub fn rcs(&self) -> Option<RcsConnection> {
        match self.get_connection_state() {
            TargetConnectionState::Rcs(conn) => Some(conn),
            _ => None,
        }
    }

    pub async fn usb(&self) -> Result<(String, Interface)> {
        if !self.is_enabled() {
            return Err(anyhow!("Cannot open USB interface for disabled target"));
        }

        let id = self.identity();
        match id.as_ref().and_then(|i| i.serial()) {
            Some(s) => Ok((
                s.to_string(),
                open_interface_with_serial(s).await.with_context(|| {
                    format!("Failed to open target usb interface by serial {s}")
                })?,
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
            .filter(|entry| match (entry.is_manual(), &entry.addr.ip()) {
                (true, _) => true,
                (_, Some(IpAddr::V6(v))) => entry.addr.scope_id() != 0 || !v.is_link_local_addr(),
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
            .filter(|entry| match (entry.is_manual(), &entry.addr.ip(), entry.addr.port()) {
                (true, _, _) => true,
                (_, Some(IpAddr::V4(v)), p) => !(v.is_loopback() && p == Some(0)),
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
            log::debug!(
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
            .filter_map(|entry| entry.is_manual().then_some(entry.addr))
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
        s.enable();
        s
    }

    /// Intended for testing only.
    pub fn addrs_insert_entry(&self, t: TargetAddrEntry) {
        self.addrs.borrow_mut().replace(t);
    }

    // Copy the addresses from other into ours -- used when we learn that target A
    // is actually the same as target B, just at a different address.
    pub fn extend_addrs_from_other(&self, other: Rc<Self>) {
        let mut addrs = self.addrs.borrow_mut();
        for addr in other.addrs.borrow().iter() {
            addrs.insert(addr.clone());
        }
    }

    pub(crate) fn addrs_extend<T>(&self, new_addrs: T)
    where
        T: IntoIterator<Item = TargetAddrEntry>,
    {
        let mut addrs = self.addrs.borrow_mut();

        for mut addr in new_addrs.into_iter() {
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
            if let TargetAddr::Net(ip_addr) = &addr.addr {
                if ip_addr.ip().is_ipv6() && addr.addr.scope_id() == 0 {
                    if let Some(entry) = addrs.get(&addr) {
                        addr.addr.set_scope_id(entry.addr.scope_id());
                    }

                    // Note: not adding ipv6 link-local addresses without scopes here is deliberate!
                    if addr.addr.ip().unwrap().is_link_local_addr() && addr.addr.scope_id() == 0 {
                        continue;
                    }
                }
            }

            if let Some(entry) = addrs.get(&addr) {
                // If the existing entry was not from discovery, unmark the incoming entry as well.
                addr.status.source.merge(entry.source);
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

    /// Sets the preferred SSH address.
    ///
    /// Returns true if successful (the `target_addr` exists). Otherwise,
    /// returns false. If the `target_addr` should be used immediately, then
    /// callers should invoke `maybe_reconnect` after calling this method.
    pub fn set_preferred_ssh_address(&self, target_addr: TargetIpAddr) -> bool {
        let address_exists = self
            .addrs
            .borrow()
            .iter()
            .any(|target_addr_entry| target_addr_entry.addr == target_addr.into());

        if !address_exists {
            return false;
        }

        self.preferred_ssh_address.borrow_mut().replace(target_addr);
        true
    }

    /// Drops the existing connection (if any) and re-initializes the
    /// `HostPipe`.
    pub fn maybe_reconnect(
        self: &Rc<Self>,
        roid_sender: Option<channel::oneshot::Sender<Option<u64>>>,
    ) {
        if self.host_pipe.borrow().is_some() {
            let HostPipeState { task, overnet_node, .. } = self.host_pipe.take().unwrap();
            // Anyone already waiting on an overnet-id will get an error
            // response as we drop the senders. (This is the correct behavior
            // -- we don't want to return Ok(None), that would imply that the
            // connection _did_ get made, but didn't provide an id.)
            drop(task);
            log::debug!("Reconnecting host_pipe for {}@{}", self.nodename_str(), self.id());
            self.run_host_pipe_with_sender(&overnet_node, roid_sender);
        }
    }

    pub fn clear_preferred_ssh_address(&self) {
        self.preferred_ssh_address.borrow_mut().take();
    }

    /// SSHs into the target just to update the `ssh_host_address` field,
    /// without starting a host pipe.
    pub fn refresh_ssh_host_addr(self: &Rc<Self>) {
        let Some(addr) = self.ssh_address() else {
            return;
        };

        let host_pipe_child_builder = HostPipeChildDefaultBuilder { ssh_path: String::from("ssh") };
        let target = Rc::clone(self);
        fuchsia_async::Task::local(async move {
            match host_pipe_child_builder.get_host_addr(addr).await {
                Ok(addr) => {
                    target.ssh_host_address.replace(Some(addr.into()));
                }
                Err(error) => log::debug!(error:? = error; "Error fetching ssh host address"),
            }
        })
        .detach();
    }

    pub fn run_host_pipe(self: &Rc<Self>, overnet_node: &Arc<overnet_core::Router>) {
        self.run_host_pipe_with_sender(overnet_node, None)
    }

    pub fn run_host_pipe_with_sender(
        self: &Rc<Self>,
        overnet_node: &Arc<overnet_core::Router>,
        roid_sender: Option<channel::oneshot::Sender<Option<u64>>>,
    ) {
        let host_pipe_child_builder = HostPipeChildDefaultBuilder { ssh_path: String::from("ssh") };
        self.run_host_pipe_with(overnet_node, roid_sender, host_pipe_child_builder)
    }

    // This function allows the caller to receive the remote-overnet-id of the target.
    // The r-o-id can be used to determine whether this host-pipe is connecting to the
    // same target as one we've already connected to, which is important when using
    // Overnet, because Overnet (due to its mesh-network topology) will not inform the
    // daemon of a "new peer" since it will consider the peer to be the same.
    // That being said, not all callers need the r-o-id, and not all targets will have
    // one (in particular, older targets will have have implemented this in their protocol).
    // If the caller asks for it, the host-pipe code _must_ return it, so every code path
    // will eventually send the resulting roid (even if it is None) back to the caller.
    //
    // This function can also take a HostPipeChildBuilder, for test purposes
    fn run_host_pipe_with<T>(
        self: &Rc<Self>,
        overnet_node: &Arc<overnet_core::Router>,
        roid_sender: Option<channel::oneshot::Sender<Option<u64>>>,
        host_pipe_child_builder: T,
    ) where
        T: HostPipeChildBuilder + Clone + 'static,
    {
        if !self.is_enabled() {
            if let Some(sender) = roid_sender {
                let _ = sender.send(None);
            }
            log::error!("Cannot run host pipe for device not in use");
            return;
        }

        if let Some(ref mut hp) = *self.host_pipe.borrow_mut() {
            // The host-pipe already exists
            if let Some(sender) = roid_sender {
                // The caller is waiting for a response
                match &mut hp.remote_overnet_id {
                    RemoteOvernetIdState::Pending(ref mut waiters) => waiters.push(sender),
                    RemoteOvernetIdState::Ready(roid) => {
                        log::debug!(
                        "Got request for host pipe overnet id for an already-running host-pipe -- sending back {roid:?}",
                    );
                        let _ = sender.send(*roid);
                    }
                }
            }
            // log::debug!("Host pipe is already set for {}@{}.", self.nodename_str(), self.id());
            return;
        }

        let weak_target = Rc::downgrade(self);
        let target_name_str = format!("{}@{}", self.nodename_str(), self.id());
        let node = Arc::clone(overnet_node);
        let overnet_node = Arc::clone(overnet_node);
        let roid_waiters =
            if let Some(roid_sender) = roid_sender { vec![roid_sender] } else { vec![] };
        let task = async move {
            // The purpose of a host pipe is to ultimately get us connected to RCS and let us transition
            // to the RCS connected state. If we're already in that state, and the RCS connection is
            // active, we don't need a host pipe. This will start happening more as we introduce USB
            // links, where the first thing we hear about a target is its appearance as an Overnet peer,
            // and thus we have an RCS connection from inception.
            let cid_and_usb_host = {
                let Some(target) = weak_target.upgrade() else {
                    // weird that self is already gone, but ¯\_(ツ)_/¯
                    // Unfortunately that means we have no access to its remote-overnet-id waiters :-(
                    return;
                };
                let state = target.state.borrow().clone();
                if let TargetConnectionState::Rcs(rcs) = state {
                    if knock_rcs(&rcs.proxy).await.is_ok() {
                        if let Some(host_pipe) = target.host_pipe.borrow_mut().as_mut() {
                            host_pipe.flush_waiters();
                        }
                        return;
                    }
                }

                target.addrs().into_iter().find_map(|addr| {
                    if let TargetAddr::VSockCtx(cid) = addr {
                        if ffx_config::get(CONFIG_ENABLE_VSOCK).unwrap_or(false) {
                            Some((cid, None))
                        } else {
                            None
                        }
                    } else if let TargetAddr::UsbCtx(cid) = addr {
                        #[cfg(not(target_os = "macos"))]
                        let ret = if let Some(host) = Target::get_usb_vsock_host() {
                            Some((cid, Some(host)))
                        } else {
                            None
                        };
                        #[cfg(target_os = "macos")]
                        let (ret, _) = (Option::<(u32, Option<()>)>::None, cid);
                        ret
                    } else {
                        None
                    }
                })
            };

            if let Some((cid, host)) = cid_and_usb_host {
                // We have a VSOCK. Use that to connect instead of SSH.

                let conn_type = if host.is_some() { "USB" } else { "VSOCK" };

                // We might be able to connect on VSOCK port 201 to call the
                // identify service and then we could get something to return to
                // these waiters.
                if let Some(target) = weak_target.upgrade() {
                    if let Some(host_pipe) = target.host_pipe.borrow_mut().as_mut() {
                        if let RemoteOvernetIdState::Pending(waiters) = std::mem::replace(
                            &mut host_pipe.remote_overnet_id,
                            RemoteOvernetIdState::Ready(None),
                        ) {
                            for waiter in waiters {
                                let _ = waiter.send(None);
                            }
                        }
                    }

                    if target.ssh_address().is_some() {
                        target.refresh_ssh_host_addr();
                    }
                }

                if let Some(host) = host {
                    let _ = &host;
                    #[cfg(not(target_os = "macos"))]
                    Box::pin(spawn_usb(host, cid, node)).await;
                } else {
                    spawn_vsock(cid, node).await;
                }

                weak_target.upgrade().and_then(|target| {
                    log::debug!(
                        "Exiting run_host_pipe for {target_name_str} ({conn_type} connection)"
                    );
                    target.host_pipe.borrow_mut().take()
                });
                return;
            }

            let watchdogs: bool = ffx_config::get("watchdogs.host_pipe.enabled").unwrap_or(false);

            let ssh_timeout: u16 = ffx_config::get(CONFIG_HOST_PIPE_SSH_TIMEOUT).unwrap_or(50);
            let nr = spawn(
                weak_target.clone(),
                watchdogs,
                ssh_timeout,
                std::sync::Arc::clone(&node),
                host_pipe_child_builder,
            )
            .await;

            match nr {
                Ok(mut hp) => {
                    log::debug!("host pipe spawn returned OK for {target_name_str}");
                    eprintln!("host pipe spawn returned OK for {target_name_str}");
                    let compatibility_status = hp.get_compatibility_status();

                    if let Some(target) = weak_target.upgrade() {
                        target.set_compatibility_status(&compatibility_status);

                        // If there's no host-pipe, it's because the target
                        // has dropped the task containing our host-pipe,
                        // so it's fine to not follow through and set target
                        // information that come from this connection.
                        // Clients waiting for the overnet_id will have
                        // gotten an error when the host-pipe was dropped.
                        if let Some(host_pipe) = target.host_pipe.borrow_mut().as_mut() {
                            host_pipe.ssh_addr = Some(hp.get_address());
                            let overnet_id = hp.overnet_id();
                            log::debug!(
                                "Got host pipe overnet id {:?} -- sending to waiters",
                                overnet_id
                            );
                            if let RemoteOvernetIdState::Pending(waiters) = std::mem::replace(
                                &mut host_pipe.remote_overnet_id,
                                RemoteOvernetIdState::Ready(overnet_id),
                            ) {
                                for sender in waiters.into_iter() {
                                    let _ = sender.send(overnet_id);
                                }
                            } else {
                                // We only go through this path once, so the state will always be Pending above.
                                unreachable!()
                            }
                        }
                    }

                    // wait for the host pipe to exit.
                    let _r = match hp.wait(&node).await {
                        Ok(r) => {
                            // This was an info. Moved to debug as this is not informational or
                            // actionable to end users.
                            log::debug!("HostPipeConnection returned: {:?}", r);
                        }
                        Err(r) => {
                            log::warn!(
                                "The host pipe connection to ['{target_name_str}'] returned: {:?}",
                                r
                            );
                        }
                    };
                }
                Err(e) => {
                    // Change this to a debug message (from warn). We will get any error from
                    // SSH client in the logs so this is redundant.
                    log::debug!("Host pipe spawn {:?}", e);
                    let compatibility_status = Some(CompatibilityInfo {
                        status: CompatibilityState::Error,
                        platform_abi: 0,
                        message: format!("Host connection failed: {e}"),
                    });
                    if let Some(target) = weak_target.upgrade() {
                        target.set_compatibility_status(&compatibility_status);
                        if let Some(host_pipe) = target.host_pipe.borrow_mut().as_mut() {
                            host_pipe.flush_waiters();
                        }
                    }
                }
            }

            weak_target.upgrade().and_then(|target| {
                log::debug!("Exiting run_host_pipe for {target_name_str}");
                target.host_pipe.borrow_mut().take()
            });
        };
        self.host_pipe.borrow_mut().replace(HostPipeState {
            task: Task::local(task),
            overnet_node,
            ssh_addr: None,
            remote_overnet_id: RemoteOvernetIdState::Pending(roid_waiters),
        });
    }

    pub fn is_host_pipe_running(&self) -> bool {
        self.host_pipe.borrow().is_some()
    }

    pub async fn init_remote_proxy(
        self: &Rc<Self>,
        overnet_node: &Arc<overnet_core::Router>,
    ) -> Result<RemoteControlProxy> {
        if !self.is_enabled() {
            return Err(anyhow!("Cannot open RCS for disabled target"));
        }

        // Ensure auto-connect has at least started.
        self.run_host_pipe(overnet_node);
        match self.events.wait_for(None, |e| e == TargetEvent::RcsActivated).await {
            Ok(()) => (),
            Err(e) => {
                log::warn!("{}", e);
                bail!("RCS connection issue")
            }
        }
        self.rcs().ok_or_else(|| anyhow!("rcs dropped after event fired")).map(|r| r.proxy)
    }

    pub async fn is_fastboot_tcp(&self) -> Result<bool> {
        match self.fastboot_address() {
            None => Ok(false),
            Some(addr) => {
                let target_name = self.nodename_str();
                let builder = ConnectionFactory {};
                let mut fastboot_interface = builder
                    .build_interface(FastbootConnectionKind::Tcp(
                        target_name,
                        SocketAddr::from(addr.0),
                    ))
                    .await?;
                // Dont care what the result is, just need to get it
                let _result = fastboot_interface.get_var("version").await?;
                Ok(true)
            }
        }
    }

    /// Check the current target state, and if it is a state that expires (such
    /// as mdns) perform the appropriate state transition. The daemon target
    /// collection expiry loop calls this function regularly.
    pub fn expire_state(&self) {
        self.update_connection_state(|current_state| {
            let expire_duration = match current_state {
                TargetConnectionState::Mdns(_) => MDNS_MAX_AGE,
                TargetConnectionState::Fastboot(_) => FASTBOOT_MAX_AGE,
                TargetConnectionState::Zedboot(_) => ZEDBOOT_MAX_AGE,
                TargetConnectionState::Manual => MDNS_MAX_AGE,
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
                _ => None,
            };

            if new_state.is_some() && self.is_transient() && self.has_keep_alive() {
                log::debug!("Not expiring state for transient target with keep alive");
                return current_state;
            }

            if let Some(ref new_state) = new_state {
                log::debug!(
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

    pub fn keep_alive(self: &Rc<Target>) -> Rc<KeepAliveHandle> {
        let weak = self.keep_alive.replace(Weak::new());
        if let Some(handle) = Weak::upgrade(&weak) {
            self.keep_alive.set(weak);
            return handle;
        }

        let handle = Rc::new(KeepAliveHandle { target: Rc::downgrade(self) });
        self.keep_alive.set(Rc::downgrade(&handle));
        handle
    }

    pub fn has_keep_alive(&self) -> bool {
        let weak = self.keep_alive.replace(Weak::new());
        let has_keep_alive = Weak::upgrade(&weak).is_some();
        self.keep_alive.set(weak);
        has_keep_alive
    }

    pub fn is_connected(&self) -> bool {
        // Only enabled targets are considered connected.
        self.is_enabled() && self.state.borrow().is_connected()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.get() || self.state.borrow().is_manual() || self.is_manual()
    }

    pub(crate) fn enable(&self) {
        self.enabled.set(true)
    }

    pub(crate) fn disable(&self) {
        self.disconnect();
        self.enabled.set(false);
    }

    pub fn is_transient(&self) -> bool {
        self.transient.get()
    }

    pub fn mark_transient(&self) {
        self.transient.set(true)
    }

    pub fn is_manual(&self) -> bool {
        self.addrs.borrow().iter().any(|addr_entry| addr_entry.is_manual())
    }

    pub fn is_waiting_for_rcs_identity(&self) -> bool {
        self.is_manual() && self.is_host_pipe_running() && self.identity().is_none()
    }

    pub fn disconnect(&self) {
        drop(self.host_pipe.take());
        log::debug!("Disconnecting host_pipe for {}@{}", self.nodename_str(), self.id());
        self.update_connection_state(|_| TargetConnectionState::Disconnected);
    }
}

impl From<&Target> for ffx::TargetInfo {
    fn from(target: &Target) -> Self {
        let (product_config, board_config) = target
            .build_config()
            .map(|b| (Some(b.product_config), Some(b.board_config)))
            .unwrap_or((None, None));

        let fastboot_interface = target.infer_fastboot_interface();
        let info = target.get_compatibility_status();

        Self {
            nodename: target.nodename(),
            serial_number: target.serial(),
            addresses: Some(target.addrs().into_iter().map(|a| a.into()).collect()),
            age_ms: Some(match Utc::now()
                .signed_duration_since(target.last_response())
                .num_milliseconds()
            {
                dur if dur < 0 => {
                    log::trace!(
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
                TargetConnectionState::Manual
                | TargetConnectionState::Mdns(_)
                | TargetConnectionState::Rcs(_)
                | TargetConnectionState::Vsock(_) => TargetState::Product,
                TargetConnectionState::Fastboot(_) => TargetState::Fastboot,
                TargetConnectionState::Zedboot(_) => TargetState::Zedboot,
            }),
            ssh_address: target.ssh_address_info(),
            ssh_host_address: target.ssh_host_address_info(),
            fastboot_interface: match fastboot_interface {
                None => None,
                Some(FastbootInterface::Usb) => Some(ffx::FastbootInterface::Usb),
                Some(FastbootInterface::Udp) => Some(ffx::FastbootInterface::Udp),
                Some(FastbootInterface::Tcp) => Some(ffx::FastbootInterface::Tcp),
            },
            compatibility: match info {
                Some(data) => Some(data.into()),
                None => None,
            },
            is_manual: Some(target.is_manual()),
            ..Default::default()
        }
    }
}

impl Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("id", &self.id)
            .field("ids", &self.ids.borrow().clone())
            .field("identity", &self.identity())
            .field("enabled", &self.enabled.get())
            .field("transient", &self.transient.get())
            .field("state", &self.state.borrow().clone())
            .field("last_response", &self.last_response.borrow().clone())
            .field("addrs", &self.addrs.borrow().clone())
            .field("ssh_port", &self.ssh_port.borrow().clone())
            .field("boot_timestamp_nanos", &self.boot_timestamp_nanos.borrow().clone())
            .field("compatibility_status", &self.compatibility_status.borrow().clone())
            .finish()
    }
}

/// A handle to a discovered target.
///
/// Allows basic info to be queried about a target. Use `TargetCollection::use_target` to declare
/// consent to use the target on behalf of an explicit ffx invocation.
#[derive(Clone, Debug)]
pub struct DiscoveredTarget(pub(crate) Rc<Target>);

impl DiscoveredTarget {
    pub fn id(&self) -> u64 {
        self.0.id()
    }
    pub fn nodename(&self) -> Option<String> {
        self.0.nodename()
    }
    pub fn nodename_str(&self) -> String {
        self.0.nodename_str()
    }
    pub fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }
}

impl From<Rc<Target>> for DiscoveredTarget {
    fn from(target: Rc<Target>) -> Self {
        Self(target)
    }
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
impl PartialEq for DiscoveredTarget {
    fn eq(&self, o: &DiscoveredTarget) -> bool {
        self.0 == o.0
    }
}

#[cfg(test)]
impl PartialEq<Target> for DiscoveredTarget {
    fn eq(&self, o: &Target) -> bool {
        (&*self.0) == o
    }
}

#[cfg(test)]
impl PartialEq<Rc<Target>> for DiscoveredTarget {
    fn eq(&self, o: &Rc<Target>) -> bool {
        (&self.0) == o
    }
}

/// Prevents a transient target from expiring until dropped.
pub struct KeepAliveHandle {
    target: Weak<Target>,
}

impl Drop for KeepAliveHandle {
    fn drop(&mut self) {
        if let Some(target) = Weak::upgrade(&self.target) {
            // Immediately expire target state in case it was already stale.
            target.expire_state();
        }
    }
}

#[cfg(test)]
mod test {
    use crate::overnet::host_pipe::HostPipeChild;

    use super::*;
    use assert_matches::assert_matches;
    use chrono::TimeZone;
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
    use fidl_fuchsia_net::Subnet;
    use fidl_fuchsia_overnet_protocol::NodeId;
    use fuchsia_async::Timer;
    use futures::prelude::*;
    use std::borrow::Borrow;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use std::process::Stdio;
    use std::str::FromStr;
    use std::sync;

    const DEFAULT_PRODUCT_CONFIG: &str = "core";
    const DEFAULT_BOARD_CONFIG: &str = "x64";
    const TEST_SERIAL: &'static str = "test-serial";

    fn setup_fake_remote_control_service(
        send_internal_error: bool,
        nodename_response: String,
    ) -> RemoteControlProxy {
        let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>();

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
        let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>();

        fuchsia_async::Task::local(async move {
            while let Ok(Some(_req)) = stream.try_next().await {}
            // Dropping the request immediately would also drop the responder,
            // resulting in the other side receiving a PEER_CLOSED
            let _ = done.await;
        })
        .detach();

        proxy
    }

    // Most of this is now handled in `task.rs`
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_disconnect_multiple_invocations() {
        let node = overnet_core::Router::new(None).unwrap();
        let t = Rc::new(Target::new_named("flabbadoobiedoo"));
        {
            let addr: TargetAddr = TargetAddr::new(IpAddr::from([192, 168, 0, 1]), 0, 0);
            t.addrs_insert(addr);
        }
        // Assures multiple "simultaneous" invocations to start the target
        // doesn't put it into a bad state that would hang.
        t.run_host_pipe(&node);
        t.run_host_pipe(&node);
        t.run_host_pipe(&node);
    }

    struct RcsStateTest {
        loop_started: bool,
        rcs_is_some: bool,
        expected: ffx::RemoteControlState,
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_rcs_states() {
        let local_node = overnet_core::Router::new(None).unwrap();

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
            t.enable();
            let a2 = IpAddr::V6(Ipv6Addr::new(
                0xfe80, 0xcafe, 0xf00d, 0xf000, 0xb412, 0xb455, 0x1337, 0xfeed,
            ));
            t.addrs_insert(TargetAddr::new(a2, 2, 0));
            if test.loop_started {
                t.run_host_pipe(&local_node);
            }
            {
                *t.state.borrow_mut() = if test.rcs_is_some {
                    TargetConnectionState::Rcs(RcsConnection::new_with_proxy(
                        Arc::clone(&local_node),
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
        let local_node = overnet_core::Router::new(None).unwrap();

        let conn = RcsConnection::new_with_proxy(
            local_node,
            setup_fake_remote_control_service(false, "foo".to_owned()),
            &NodeId { id: 1234 },
        );
        let t = Target::new();
        t.update_connection_state(|_| TargetConnectionState::Rcs(conn));
        // This will hang forever if no synthesis happens.
        t.events.wait_for(None, |e| e == TargetEvent::RcsActivated).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_event_fire() {
        let local_node = overnet_core::Router::new(None).unwrap();

        let t = Target::new_named("balaowihf");
        let conn = RcsConnection::new_with_proxy(
            local_node,
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
        let local_node = overnet_core::Router::new(None).unwrap();

        let t = Target::new_named("hello-kitty");
        let rcs_state = TargetConnectionState::Rcs(
            RcsConnection::new(local_node.clone(), &mut NodeId { id: 1234 }).unwrap(),
        );
        t.set_state(rcs_state.clone());

        // Attempt to set the state to TargetConnectionState::Mdns, this transition should fail, as in
        // this transition RCS should be retained.
        t.update_connection_state(|_| TargetConnectionState::Mdns(Instant::now()));

        assert_eq!(t.get_connection_state(), rcs_state);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_connection_state_will_not_drop_rcs_on_manual_events() {
        let local_node = overnet_core::Router::new(None).unwrap();

        let t = Target::new_named("hello-kitty");
        let rcs_state = TargetConnectionState::Rcs(
            RcsConnection::new(local_node.clone(), &mut NodeId { id: 1234 }).unwrap(),
        );
        t.set_state(rcs_state.clone());

        // Attempt to set the state to TargetConnectionState::Manual, this transition should fail, as in
        // this transition RCS should be retained.
        t.update_connection_state(|_| TargetConnectionState::Manual);

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
    async fn test_expire_state_manual_fastboot() {
        let t = Target::new_with_addr_entries(
            Some("platypodes-are-venomous"),
            [TargetAddrEntry::new(
                TargetAddr::new("::1".parse::<IpAddr>().unwrap().into(), 0, 0),
                Utc::now(),
                TargetAddrStatus::fastboot(TargetTransport::Network).manually_added(),
            )]
            .into_iter(),
        );
        assert!(t.is_manual());

        let then = Instant::now() - (FASTBOOT_MAX_AGE + Duration::from_secs(1));
        t.update_connection_state(|_| TargetConnectionState::Fastboot(then));

        t.expire_state();

        assert_eq!(t.get_connection_state(), TargetConnectionState::Fastboot(then));
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
                    Utc.with_ymd_and_hms(2014 + (i as i32), 10, 31, 9, 10, 12).unwrap(),
                    TargetAddrStatus::ssh(),
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
                    Utc.with_ymd_and_hms(2014 + (i as i32), 10, 31, 9, 10, 12).unwrap(),
                    TargetAddrStatus::ssh(),
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
        let target_addr: TargetIpAddr = TargetIpAddr::new("fe80::2".parse().unwrap(), 1, 0);
        let target = Target::new_with_addr_entries(
            Some("foo"),
            vec![TargetAddrEntry::new((&target_addr).into(), Utc::now(), TargetAddrStatus::ssh())]
                .into_iter(),
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
                TargetAddrStatus::ssh(),
            )]
            .into_iter(),
        );

        assert!(!target.set_preferred_ssh_address(TargetIpAddr::new(
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
                TargetAddrStatus::ssh(),
            ),
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 2, 0),
                start.into(),
                TargetAddrStatus::ssh(),
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
                TargetAddrStatus::ssh(),
            ),
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 2, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrStatus::ssh(),
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
                TargetAddrStatus::ssh(),
            ),
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 2, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrStatus::ssh(),
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
                TargetAddrStatus::ssh(),
            ),
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrStatus::ssh().manually_added(),
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
                TargetAddrStatus::ssh(),
            ),
            TargetAddrEntry::new(
                TargetAddr::new("2000::2".parse().unwrap(), 0, 0),
                (start + Duration::from_secs(1)).into(),
                TargetAddrStatus::ssh(),
            ),
        ]);
        assert_eq!(
            Target::new_with_addr_entries(name, addrs.into_iter()).ssh_address(),
            Some("[2000::2]:22".parse().unwrap())
        );

        let preferred_target_addr: TargetIpAddr =
            TargetIpAddr::new("fe80::2".parse().unwrap(), 1, 0);
        // User expressed a preferred SSH address. Prefer it over all other
        // addresses (even manual).
        let addrs = BTreeSet::from_iter(vec![
            TargetAddrEntry::new(
                (&preferred_target_addr).into(),
                start.into(),
                TargetAddrStatus::ssh(),
            ),
            TargetAddrEntry::new(
                TargetAddr::new("2000::1".parse().unwrap(), 0, 0),
                (start - Duration::from_secs(1)).into(),
                TargetAddrStatus::ssh().manually_added(),
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
                TargetAddrStatus::ssh(),
            )]
            .into_iter(),
        );

        let (ip, port) = match target.ssh_address_info().unwrap() {
            TargetIpAddrInfo::IpPort(TargetIpPort { ip, port, .. }) => match ip {
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
                TargetAddrStatus::ssh(),
            )]
            .into_iter(),
        );
        target.set_ssh_port(Some(8022));

        let (ip, port) = match target.ssh_address_info().unwrap() {
            TargetIpAddrInfo::IpPort(TargetIpPort { ip, port, .. }) => match ip {
                IpAddress::Ipv4(i) => (IpAddr::from(i.addr), port),
                IpAddress::Ipv6(i) => (IpAddr::from(i.addr), port),
            },
            _ => panic!("unexpected type"),
        };

        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(port, 8022);
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
                TargetAddrStatus::netsvc(),
            )
            .into(),
        );
        target.addrs_insert_entry(
            TargetAddrEntry::new(
                TargetAddr::new("fe80::1".parse().unwrap(), 0, 0),
                Utc::now().into(),
                TargetAddrStatus::ssh(),
            )
            .into(),
        );
        assert_eq!(target.ssh_address(), Some("[fe80::1%0]:22".parse::<SocketAddr>().unwrap()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_netsvc_ssh_address_info_should_be_none() {
        let ip = "f111::4".parse().unwrap();
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetIpAddr::new(ip, 0xbadf00d, 0));
        let target = Target::new_with_netsvc_addrs(Some("foo"), addr_set);

        assert!(target.ssh_address_info().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_is_manual() {
        let target = Target::new();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrStatus::ssh().manually_added(),
        ));
        assert!(target.is_manual());

        let target = Target::new();
        assert!(!target.is_manual());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_update_connection_state_manual_disconnect() {
        let local_node = overnet_core::Router::new(None).unwrap();

        let target = Target::new();
        target.addrs_insert_entry(TargetAddrEntry::new(
            TargetAddr::new("::1".parse().unwrap(), 0, 0),
            Utc::now(),
            TargetAddrStatus::ssh().manually_added(),
        ));
        target.set_state(TargetConnectionState::Manual);

        // Attempting to transition a manual target into the disconnected state remains in manual,
        // if the target has no timeout set.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_eq!(target.get_connection_state(), TargetConnectionState::Manual);

        let conn = RcsConnection::new_with_proxy(
            local_node,
            setup_fake_remote_control_service(false, "abc".to_owned()),
            &NodeId { id: 1234 },
        );
        // A manual target can enter the RCS state.
        target.update_connection_state(|_| TargetConnectionState::Rcs(conn));
        assert_matches!(target.get_connection_state(), TargetConnectionState::Rcs(_));

        // A manual target exiting the RCS state to disconnected returns to manual instead.
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        assert_eq!(target.get_connection_state(), TargetConnectionState::Manual);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_disconnect() {
        let local_node = overnet_core::Router::new(None).unwrap();
        let target = Target::new();
        target.set_state(TargetConnectionState::Mdns(Instant::now()));
        target.host_pipe.borrow_mut().replace(HostPipeState {
            task: Task::local(future::pending()),
            overnet_node: local_node,
            ssh_addr: None,
            remote_overnet_id: RemoteOvernetIdState::Ready(None),
        });

        target.disconnect();

        assert_eq!(TargetConnectionState::Disconnected, target.get_connection_state());
        assert!(target.host_pipe.borrow().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_state_borrow() {
        let local_node = overnet_core::Router::new(None).unwrap();
        let (done_send, done_recv) = channel::oneshot::channel::<()>();

        let target = Target::new();
        // We want run_host_pipe() to reach the point of knock_rcs(), so we
        // need to set up an RCS first. But we'll set it up so it doesn't respond
        let conn = RcsConnection::new_with_proxy(
            Arc::clone(&local_node),
            setup_fake_unresponsive_remote_control_service(done_recv),
            &NodeId { id: 1234 },
        );
        target.set_state(TargetConnectionState::Rcs(conn));
        target.run_host_pipe(&local_node);
        // Let run_host_pipe()'s spawned task run
        Timer::new(Duration::from_millis(50)).await;
        target.update_connection_state(|_| TargetConnectionState::Disconnected);
        done_send.send(()).expect("send failed")
        // No assertion -- we are making sure run_host_pipe() doesn't panic
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_from_target_for_targetinfo() {
        {
            let target = Target::new_for_usb("IANTHE");

            let info: ffx::TargetInfo = ffx::TargetInfo::from(target.borrow());
            assert_eq!(info.fastboot_interface, Some(ffx::FastbootInterface::Usb));
        }
        {
            let mut addrs = BTreeSet::new();
            addrs.insert(TargetIpAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0, 0));
            let interface = FastbootInterface::Tcp;
            let target = Target::new_with_fastboot_addrs(Some("Babs"), None, addrs, interface);

            let info: ffx::TargetInfo = ffx::TargetInfo::from(target.borrow());
            assert_eq!(info.fastboot_interface, Some(ffx::FastbootInterface::Tcp));
        }
        {
            let mut addrs = BTreeSet::new();
            addrs.insert(TargetIpAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0, 0));
            let interface = FastbootInterface::Udp;
            let target =
                Target::new_with_fastboot_addrs(Some("Coronabeth"), None, addrs, interface);

            let info: ffx::TargetInfo = ffx::TargetInfo::from(target.borrow());
            assert_eq!(info.fastboot_interface, Some(ffx::FastbootInterface::Udp));
        }
        {
            let target = Target::new_with_addr_entries(
                Some("manual"),
                [TargetAddrEntry::new(
                    TargetAddr::new("::1".parse::<IpAddr>().unwrap().into(), 0, 0),
                    Utc::now(),
                    TargetAddrStatus::ssh().manually_added(),
                )]
                .into_iter(),
            );

            let info: ffx::TargetInfo = ffx::TargetInfo::from(target.borrow());
            assert_eq!(info.is_manual, Some(true));
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_infer_fastboot_interface() {
        {
            let target = Target::new();

            let interface = target.infer_fastboot_interface();
            assert!(interface.is_none());
        }
        {
            let mut addrs = BTreeSet::new();
            addrs.insert(TargetIpAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0, 0));
            let interface = FastbootInterface::Tcp;
            let target = Target::new_with_fastboot_addrs(Some("Babs"), None, addrs, interface);

            // Purposefully remove the interface
            target.fastboot_interface.replace(None);

            assert_eq!(target.infer_fastboot_interface(), Some(FastbootInterface::Tcp));
        }
        {
            let mut addrs = BTreeSet::new();
            addrs.insert(TargetIpAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0, 0));
            let interface = FastbootInterface::Udp;
            let target =
                Target::new_with_fastboot_addrs(Some("Coronabeth"), None, addrs, interface);

            // Purposefully remove the interface
            target.fastboot_interface.replace(None);

            assert_eq!(target.infer_fastboot_interface(), Some(FastbootInterface::Udp));
        }
    }

    #[derive(Clone)]
    struct FakeHostPipeChildBuilder {
        overnet_id: Option<u64>,
    }

    #[async_trait(?Send)]
    impl crate::overnet::host_pipe::HostPipeChildBuilder for FakeHostPipeChildBuilder {
        async fn new(
            &self,
            _addr: SocketAddr,
            _id: u64,
            _stderr_buf: Rc<LogBuffer>,
            _event_queue: events::Queue<TargetEvent>,
            _watchdogs: bool,
            _ssh_timeout: u16,
            _node: sync::Arc<overnet_core::Router>,
        ) -> Result<(Option<HostAddr>, HostPipeChild), ffx_ssh::parse::PipeError> {
            Ok((
                Some(HostAddr("127.0.0.1".to_string())),
                HostPipeChild::fake_new(
                    tokio::process::Command::new("echo")
                        .arg("foo")
                        .stdout(Stdio::piped())
                        .stdin(Stdio::piped()),
                    self.overnet_id,
                ),
            ))
        }

        async fn get_host_addr(
            &self,
            _addr: SocketAddr,
        ) -> Result<String, ffx_ssh::parse::PipeError> {
            Ok("127.0.0.1".into())
        }

        fn ssh_path(&self) -> &str {
            todo!()
        }
    }

    #[fuchsia::test]
    async fn test_query_new_overnet_id() {
        let local_node = overnet_core::Router::new(None).unwrap();
        let target = crate::target::Target::new_with_addrs(
            Some("foo"),
            [TargetIpAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        target.set_state(TargetConnectionState::Mdns(Instant::now()));
        let (snd, rcv) = channel::oneshot::channel::<Option<u64>>();
        target.enable();
        let overnet_id = Some(123);
        target.run_host_pipe_with(&local_node, Some(snd), FakeHostPipeChildBuilder { overnet_id });
        let roid = rcv.await.expect("roid receiver failed");
        assert_eq!(roid, overnet_id);
    }

    #[fuchsia::test]
    async fn test_query_existing_overnet_id() {
        let local_node = overnet_core::Router::new(None).unwrap();
        let target = crate::target::Target::new_with_addrs(
            Some("foo"),
            [TargetIpAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let overnet_id = Some(123);
        target.set_state(TargetConnectionState::Mdns(Instant::now()));
        target.host_pipe.borrow_mut().replace(HostPipeState {
            task: Task::local(future::pending()),
            overnet_node: local_node.clone(),
            ssh_addr: None,
            remote_overnet_id: RemoteOvernetIdState::Ready(overnet_id),
        });
        let (snd, rcv) = channel::oneshot::channel::<Option<u64>>();
        target.enable();
        target.run_host_pipe_with_sender(&local_node, Some(snd));
        let roid = rcv.await.expect("roid receiver failed");
        assert_eq!(roid, overnet_id);
    }

    mod enabled {
        use super::*;

        #[fuchsia_async::run_singlethreaded(test)]
        async fn test_enable() {
            let target = Target::new();

            assert!(!target.is_enabled());
            target.enable();
            assert!(target.is_enabled());
        }

        #[fuchsia_async::run_singlethreaded(test)]
        async fn test_manual_targets_always_enable() {
            let target = Target::new_with_addr_entries(
                Some("foo"),
                std::iter::once(TargetAddrEntry::new(
                    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 22).into(),
                    chrono::DateTime::<Utc>::MIN_UTC,
                    TargetAddrStatus::ssh().manually_added(),
                )),
            );

            assert!(target.is_enabled());
            assert!(target.is_manual());
            target.disable();
            assert!(target.is_enabled());
        }

        #[fuchsia_async::run_singlethreaded(test)]
        async fn test_disable() {
            let target = Target::new_autoconnected("foo");

            assert!(target.is_enabled());
            target.disable();
            assert!(!target.is_enabled());
            assert!(!target.is_connected());
        }

        // Instant::now() - Duration panics on macOS builders.
        #[cfg(not(target_os = "macos"))]
        #[fuchsia_async::run_singlethreaded(test)]
        async fn test_transient_expire() {
            let target = Target::new_autoconnected("foo");

            target.mark_transient();
            assert!(target.is_transient());

            // A call to expire_state should not disable when not expired.
            assert!(target.is_enabled());
            target.expire_state();
            assert!(target.is_connected());
            assert!(target.is_enabled());

            target.update_connection_state(|_| {
                TargetConnectionState::Mdns(Instant::now() - Duration::from_secs(60 * 60))
            });

            // Should not expire with active keep alive handle.
            let handle = target.keep_alive();
            assert!(target.is_enabled());
            target.expire_state();
            assert!(target.is_connected());
            assert!(target.is_enabled());

            // But should disable when expired.
            drop(handle);
            assert!(!target.is_enabled());
            assert!(!target.is_connected());
        }

        // Instant::now() - Duration panics on macOS builders.
        #[cfg(not(target_os = "macos"))]
        #[fuchsia_async::run_singlethreaded(test)]
        async fn test_non_transient_expire() {
            let target = Target::new_autoconnected("foo");

            assert!(!target.is_transient());

            // A call to expire_state should not disable when not expired.
            assert!(target.is_enabled());
            target.expire_state();
            assert!(target.is_connected());
            assert!(target.is_enabled());

            target.update_connection_state(|_| {
                TargetConnectionState::Mdns(Instant::now() - Duration::from_secs(60 * 60))
            });

            // And should not disable when expired.
            assert!(target.is_enabled());
            target.expire_state();
            assert!(target.is_enabled());
        }

        #[fuchsia_async::run_singlethreaded(test)]
        async fn test_reset_transient_on_rediscovery() {
            let target = Target::new_autoconnected("foo");

            target.mark_transient();
            assert!(target.is_transient());

            target.disable();

            assert!(target.is_transient());

            target.update_connection_state(|_| TargetConnectionState::Mdns(Instant::now()));

            assert!(!target.is_transient());
        }
    }
}
