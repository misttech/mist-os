// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bind_fuchsia_google_platform_usb::{
    BIND_USB_PROTOCOL_VSOCK_BRIDGE, BIND_USB_SUBCLASS_VSOCK_BRIDGE, BIND_USB_VID_GOOGLE,
};
use bind_fuchsia_usb::BIND_USB_CLASS_VENDOR_SPECIFIC;
use fuchsia_async as fasync;
use futures::channel::mpsc;
use futures::future::{select, AbortHandle, Abortable, Either};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use rand::Rng;
use std::collections::HashMap;
use std::iter::IntoIterator;
use std::num::NonZero;
use std::path::Path;
use std::pin::pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use thiserror::Error;
use usb_vsock::{
    Address, Header, Packet, PacketType, UsbPacketBuilder, VsockPacketIterator, CID_ANY, CID_HOST,
    CID_LOOPBACK, VSOCK_MAGIC,
};

/// How long to wait for the USB protocol to synchronize.
const MAGIC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How much data to send in a single USB bulk transfer frame.
const MTU: usize = 1024;

/// Range from which we allocate random ports when making a connection.
/// Deliberately non-inclusive as (u32)-1 is a reserved value in the VSOCK spec.
const RANDOM_PORT_RANGE: std::ops::Range<u32> = 32768..u32::MAX;

/// Watches the usb devfs for new devices.
async fn listen_for_usb_devices(host: Weak<UsbVsockHost>) -> Result<(), usb_rs::Error> {
    tracing::info!("Listening for USB devices");
    let mut stream = usb_rs::wait_for_devices(true, false)?;
    while let Some(device) = stream.next().await.transpose()? {
        let usb_rs::DeviceEvent::Added(device) = device else {
            continue;
        };

        let Some(host) = host.upgrade() else {
            tracing::debug!("USB listening task observed host disappeared");
            return Ok(());
        };

        host.add_device(device);
    }

    tracing::warn!("USB listening stopped unexpectedly");
    Ok(())
}

/// Errors that can occur while exchanging magic packets when bringing up a USB VSOCK connection.
#[derive(Debug, Error)]
enum SyncError {
    #[error("Could not write to endpoint while synchronizing: {0}")]
    Send(usb_rs::Error),
    #[error("Could not read from endpoint while synchronizing: {0}")]
    Recv(usb_rs::Error),
    #[error("Timed out waiting for driver to synchronize")]
    TimedOut,
}

/// Creates a new magic packet used to synchronize a USB VSOCK connection.
fn sync_packet() -> Vec<u8> {
    let header = &mut Header::new(PacketType::Sync);
    header.payload_len = (VSOCK_MAGIC.len() as u32).into();
    header.host_cid = CID_HOST.into();
    header.host_port = 0.into();
    header.device_cid = CID_ANY.into();
    header.device_port = 0.into();
    let packet = Packet { header, payload: VSOCK_MAGIC };
    let mut packet_storage = vec![0; header.packet_size()];
    packet.write_to_unchecked(&mut packet_storage);
    packet_storage
}

/// Creates a new magic packet used to finish synchronizing a USB VSOCK connection.
fn sync_ack_packet(cid: u32) -> Vec<u8> {
    let header = &mut Header::new(PacketType::Sync);
    header.payload_len = (VSOCK_MAGIC.len() as u32).into();
    header.host_cid = CID_HOST.into();
    header.host_port = 0.into();
    header.device_cid = cid.into();
    header.device_port = 0.into();
    let packet = Packet { header, payload: VSOCK_MAGIC };
    let mut packet_storage = vec![0; header.packet_size()];
    packet.write_to_unchecked(&mut packet_storage);
    packet_storage
}

/// Creates a new magic packet used to synchronize a USB VSOCK connection.
fn echo_reply_packet(address: &Address, payload: &[u8]) -> Vec<u8> {
    let header = &mut Header::new(PacketType::EchoReply);
    header.set_address(address);
    header.payload_len = (payload.len() as u32).into();
    let packet = Packet { header, payload: payload };
    let mut packet_storage = vec![0; header.packet_size()];
    packet.write_to_unchecked(&mut packet_storage);
    packet_storage
}

/// Wait on a USB port for the magic packet indicating the start of our USB
/// VSOCK protocol.
async fn wait_for_magic(
    debug_name: &str,
    out_ep: &usb_rs::BulkOutEndpoint,
    in_ep: &usb_rs::BulkInEndpoint,
) -> Result<u32, SyncError> {
    let mut magic_timer = fasync::Timer::new(MAGIC_TIMEOUT);
    let mut buf = [0u8; MTU];
    out_ep.write(&sync_packet()).await.map_err(SyncError::Send)?;
    loop {
        let size = {
            tracing::trace!(device = debug_name, "Reading from in endpoint for magic string");
            let read_fut = in_ep.read(&mut buf);
            let read_fut = pin!(read_fut);
            match select(read_fut, &mut magic_timer).await {
                Either::Left((got, _)) => got.map_err(SyncError::Recv)?,
                Either::Right((_, fut)) => {
                    if let Some(got) = fut.now_or_never() {
                        got.map_err(SyncError::Recv)?
                    } else {
                        return Err(SyncError::TimedOut);
                    }
                }
            }
        };
        let buf = &buf[..size];

        let mut packets = VsockPacketIterator::new(&buf);
        while let Some(packet) = packets.next() {
            let Ok(packet) = packet else {
                tracing::warn!(device = debug_name, "Packet failed to parse, ignoring.");
                break;
            };
            match packet.header.packet_type {
                PacketType::Sync => {
                    let magic = <&[u8; 7]>::try_from(packet.payload).ok();

                    if magic.filter(|x| **x == *VSOCK_MAGIC).is_some() {
                        return Ok(packet.header.device_cid.get());
                    }

                    tracing::warn!(
                        device = debug_name,
                        "Invalid USB magic string (len = {}) received, ignoring and re-attempting sync",
                        packet.header.payload_len
                    );
                    out_ep.write(&sync_packet()).await.map_err(SyncError::Send)?;
                }
                PacketType::Echo => {
                    tracing::debug!(
                        device = debug_name,
                        "received echo packet while waiting for sync, responding."
                    );
                    out_ep
                        .write(&echo_reply_packet(&Address::from(packet.header), packet.payload))
                        .await
                        .map_err(SyncError::Send)?;
                }
                ty => {
                    tracing::warn!(
                        device = debug_name,
                        "Unexpected packet type '{ty:?}' waiting for packet synchronization, ignoring and re-attempting sync"
                    );
                    out_ep.write(&sync_packet()).await.map_err(SyncError::Send)?;
                }
            }
        }
    }
}

/// Errors which cause `run_usb_link` to fail.
#[derive(Debug, Error)]
enum LinkError {
    #[error(transparent)]
    SyncError(#[from] SyncError),
    #[error("In endpoint missing")]
    InMissing,
    #[error("Out endpoint missing")]
    OutMissing,
    #[error("Could not write to endpoint: {0}")]
    Send(usb_rs::Error),
    #[error("Could not read from endpoint: {0}")]
    Recv(usb_rs::Error),
    #[error("Error decoding packet: {0}")]
    PacketDecode(std::io::Error),
    #[error("Error handling packet: {0}")]
    PacketHandle(std::io::Error),
}

/// Handles sending packets from `usb_vsock::Connection` over the USB device,
/// and giving received packets from the USB device to the same connection.
async fn run_usb_link(
    host: Weak<UsbVsockHost>,
    debug_name: String,
    interface: usb_rs::Interface,
    cid_out: &mut Option<u32>,
) -> Result<(), LinkError> {
    tracing::info!("Setting up USB link for {debug_name}");
    let debug_name = debug_name.as_str();

    let mut in_ep = None;
    let mut out_ep = None;

    for endpoint in interface.endpoints() {
        match endpoint {
            usb_rs::Endpoint::BulkIn(endpoint) => {
                if in_ep.is_some() {
                    tracing::warn!(device = debug_name, "Multiple bulk in endpoints on interface");
                } else {
                    in_ep = Some(endpoint)
                }
            }
            usb_rs::Endpoint::BulkOut(endpoint) => {
                if out_ep.is_some() {
                    tracing::warn!(device = debug_name, "Multiple bulk out endpoints on interface");
                } else {
                    out_ep = Some(endpoint)
                }
            }
            _ => (),
        }
    }

    let in_ep = in_ep.ok_or(LinkError::InMissing)?;
    let out_ep = out_ep.ok_or(LinkError::OutMissing)?;

    let requested_cid = wait_for_magic(debug_name, &out_ep, &in_ep).await?;

    let (conn_state, incoming_requests) = ConnectionState::new();
    let connection = Arc::clone(&conn_state.connection);
    let cid = if let Some(host) = host.upgrade() {
        let cid = {
            let mut inner = host.inner.lock().unwrap();
            let cid = if requested_cid > CID_HOST
                && requested_cid != CID_ANY
                && !inner.conns.contains_key(&requested_cid)
            {
                requested_cid
            } else {
                host.next_cid.fetch_add(1, Ordering::Relaxed)
            };

            inner.conns.insert(cid, conn_state);
            *cid_out = Some(cid);
            cid
        };

        host.add_incoming_request_handler(cid, incoming_requests);

        let mut sender = host.event_sender.clone();
        host.scope.spawn(async move {
            let _ = sender.send(UsbVsockHostEvent::AddedCid(cid)).await;
        });

        cid
    } else {
        tracing::warn!(
            device = debug_name,
            "Host object disappeared before connection established"
        );
        return Ok(());
    };

    let debug_name = format!("usb:cid:{cid} ({debug_name})");

    out_ep.write(&sync_ack_packet(cid)).await.map_err(SyncError::Send)?;

    let tx_conn = connection.clone();
    let tx = async move {
        let mut builder = UsbPacketBuilder::new(vec![0u8; MTU]);
        loop {
            builder = tx_conn.fill_usb_packet(builder).await;
            out_ep.write(builder.take_usb_packet().unwrap()).await.map_err(LinkError::Send)?;
        }
    };
    let rx_conn = connection.clone();
    let rx = async move {
        let mut data = [0; MTU];
        loop {
            let size = in_ep.read(&mut data).await.map_err(LinkError::Recv)?;

            if size == 0 {
                continue;
            }

            let mut packets = VsockPacketIterator::new(&data[..size]);
            while let Some(packet) = packets.next() {
                rx_conn
                    .handle_vsock_packet(packet.map_err(LinkError::PacketDecode)?)
                    .await
                    .map_err(LinkError::PacketHandle)?;
            }
        }
    };

    let tx = pin!(tx);
    let rx = pin!(rx);
    match select(tx, rx).await {
        Either::Left((e, _)) => {
            if let Result::<(), LinkError>::Err(e) = e {
                tracing::warn!(usb_device = debug_name, "Transmit failed: {:?}", e);
                Err(e)
            } else {
                tracing::debug!(usb_device = debug_name, "Transmit closed");
                Ok(())
            }
        }
        Either::Right((e, _)) => {
            if let Result::<(), LinkError>::Err(e) = e {
                tracing::warn!(usb_device = debug_name, "Receive failed: {:?}", e);
                Err(e)
            } else {
                tracing::debug!(usb_device = debug_name, "Receive closed");
                Ok(())
            }
        }
    }
}

/// Contains senders for routing incoming connections when listening on a port.
#[derive(Clone)]
enum PortListening {
    AnyCid(mpsc::Sender<(fasync::Socket, usb_vsock::ConnectionState)>),
    ByCid(HashMap<u32, mpsc::Sender<(fasync::Socket, usb_vsock::ConnectionState)>>),
}

impl PortListening {
    /// Try to add an additional port to listen on to this listener. This also
    /// accounts for the case where the listener that's already registered is
    /// dead, so if the sender for new connections we have is hung up already
    /// this will quietly replace it.
    ///
    /// The `port` argument is for error reporting; `PortListening` values are
    /// implicitly 1:1 associated with ports already.
    fn add_listener(
        &mut self,
        cid: Option<u32>,
        port: u32,
        sender: mpsc::Sender<(fasync::Socket, usb_vsock::ConnectionState)>,
    ) -> Result<(), UsbVsockError> {
        match self {
            PortListening::AnyCid(old) if old.is_closed() => {
                if let Some(cid) = cid {
                    *self = PortListening::ByCid([(cid, sender)].into_iter().collect());
                } else {
                    *self = PortListening::AnyCid(sender);
                }
                Ok(())
            }
            PortListening::ByCid(map) => {
                if let Some(cid) = cid {
                    match map.entry(cid) {
                        std::collections::hash_map::Entry::Vacant(vacant_entry) => {
                            vacant_entry.insert(sender);
                            Ok(())
                        }
                        std::collections::hash_map::Entry::Occupied(mut occupied_entry) => {
                            if occupied_entry.get().is_closed() {
                                occupied_entry.insert(sender);
                                Ok(())
                            } else {
                                Err(UsbVsockError::PortInUse(port))
                            }
                        }
                    }
                } else if map.values().all(|x| x.is_closed()) {
                    *self = PortListening::AnyCid(sender);
                    Ok(())
                } else {
                    Err(UsbVsockError::PortInUse(port))
                }
            }
            _ => Err(UsbVsockError::PortInUse(port)),
        }
    }
}

/// State of a local port.
#[derive(Clone)]
enum PortState {
    /// This port is reserved. Usually means this is the near side of a
    /// connection that was established to some device via `connect()`.
    Reserved,

    /// We are listening on this port. Incoming connections are delivered via
    /// the sender.
    Listening(PortListening),
}

impl PortState {
    /// Tries to listen on this port at the given CID. The `port` argument is
    /// for error reporting and should name the port this `PortState` is
    /// associated with.
    fn add_listener(
        &mut self,
        cid: Option<u32>,
        port: u32,
        sender: mpsc::Sender<(fasync::Socket, usb_vsock::ConnectionState)>,
    ) -> Result<(), UsbVsockError> {
        match self {
            PortState::Reserved => Err(UsbVsockError::PortInUse(port)),
            PortState::Listening(l) => l.add_listener(cid, port, sender),
        }
    }
}

/// Holds a connection to a single USB device.
struct ConnectionState {
    connection: Arc<usb_vsock::Connection<Vec<u8>>>,
    _control_socket: fuchsia_async::Socket,
}

impl ConnectionState {
    /// Create a new connection state.
    fn new() -> (Self, mpsc::Receiver<usb_vsock::ConnectionRequest>) {
        let (incoming_requests_tx, incoming_requests) = mpsc::channel(1);
        let (control_socket, other_end) = fuchsia_async::emulated_handle::Socket::create_stream();
        let control_socket = fuchsia_async::Socket::from_socket(control_socket);
        let other_end = fuchsia_async::Socket::from_socket(other_end);
        let connection = Arc::new(usb_vsock::Connection::new(other_end, incoming_requests_tx));

        (
            ConnectionState {
                connection: Arc::clone(&connection),
                _control_socket: control_socket,
            },
            incoming_requests,
        )
    }
}

/// Errors returned from operations on [`UsbVsockHost`]
#[derive(Debug, Error)]
pub enum UsbVsockError {
    #[error("No target found with cid {0}")]
    NotFound(u32),
    #[error("Port {0} already in use")]
    PortInUse(u32),
    #[error("Connection failed")]
    ConnectFailed(std::io::Error),
    #[error("Port number was too large")]
    PortOutOfRange,
}

/// Lock-protected fields of `UsbVsockHost`.
struct UsbVsockHostInner {
    conns: HashMap<u32, ConnectionState>,
    port_states: HashMap<u32, PortState>,
}

/// Events coming from the `UsbVsockHost` indicating the appearance and
/// disappearance of CIDs.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum UsbVsockHostEvent {
    AddedCid(u32),
    RemovedCid(u32),
}

/// A container for connections to USB devices that is responsible for assigning
/// them CIDs and routing connections based on those CIDs.
pub struct UsbVsockHost {
    scope: fasync::Scope,
    inner: Mutex<UsbVsockHostInner>,
    next_cid: AtomicU32,
    event_sender: mpsc::Sender<UsbVsockHostEvent>,
}

impl std::fmt::Debug for UsbVsockHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsbVsockHost").finish()
    }
}

impl UsbVsockHost {
    /// Create a new USB VSOCK host.
    pub fn new(
        paths: impl IntoIterator<Item: AsRef<Path>>,
        discover: bool,
        events: mpsc::Sender<UsbVsockHostEvent>,
    ) -> Arc<Self> {
        let (tx, rx) = futures::channel::oneshot::channel();
        let ret = Arc::new_cyclic(|weak_self| {
            let ret = UsbVsockHost {
                scope: fasync::Scope::new(),
                inner: Mutex::new(UsbVsockHostInner {
                    conns: HashMap::new(),
                    port_states: HashMap::new(),
                }),
                next_cid: AtomicU32::new(3),
                event_sender: events,
            };

            if discover {
                let weak_self = weak_self.clone();
                ret.scope.spawn(async move {
                    // Make sure we're out of Arc::new_cyclic before this future gets polled.
                    let _ = rx.await;
                    if let Err(e) = listen_for_usb_devices(weak_self).await {
                        tracing::warn!(error = ?e, "USB listening encountered an unexpected error");
                    }
                });
            }

            ret
        });

        for path in paths {
            ret.add_path(path);
        }

        let _ = tx.send(());
        ret
    }

    /// Create a new USB VSOCK host for testing. Guaranteed not to try to touch
    /// the machine's actual USB devices.
    pub fn new_for_test(event_sender: mpsc::Sender<UsbVsockHostEvent>) -> Arc<Self> {
        Arc::new(UsbVsockHost {
            scope: fasync::Scope::new(),
            inner: Mutex::new(UsbVsockHostInner {
                conns: HashMap::new(),
                port_states: HashMap::new(),
            }),
            next_cid: AtomicU32::new(3),
            event_sender,
        })
    }

    /// Add a new test connection to this host.
    pub fn add_connection_for_test(
        self: &Arc<Self>,
        connection: Arc<usb_vsock::Connection<Vec<u8>>>,
        control_socket: fuchsia_async::Socket,
        incoming_requests: mpsc::Receiver<usb_vsock::ConnectionRequest>,
    ) -> u32 {
        let cid = self.next_cid.fetch_add(1, Ordering::Relaxed);
        let success = self
            .inner
            .lock()
            .unwrap()
            .conns
            .insert(cid, ConnectionState { connection, _control_socket: control_socket })
            .is_none();
        assert!(success);
        self.add_incoming_request_handler(cid, incoming_requests);
        let mut sender = self.event_sender.clone();
        self.scope.spawn(async move {
            let _ = sender.send(UsbVsockHostEvent::AddedCid(cid)).await;
        });
        cid
    }

    /// Connect to a new USB device by device path and assign it a CID. Returns
    /// `true` if the device was added successfully. `false` could just indicate
    /// the device wasn't a USB VSOCK device, so it's a normal event, not an
    /// error, hence a bool not a Result.
    pub fn add_path(self: &Arc<Self>, path: impl AsRef<Path>) -> bool {
        self.add_device(usb_rs::DeviceHandle::from_path(path))
    }

    /// Allocate a new port on the host (cid 2) to be used as the local end of a
    /// connection the host initiates.
    fn alloc_port(&self) -> u32 {
        loop {
            let random_port = rand::thread_rng().gen_range(RANDOM_PORT_RANGE);

            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.inner.lock().unwrap().port_states.entry(random_port)
            {
                entry.insert(PortState::Reserved);
                return random_port;
            }
        }
    }

    /// Connect a new socket to a target with the given CID and port.
    pub async fn connect(
        &self,
        cid: NonZero<u32>,
        port: u32,
        socket: fasync::Socket,
    ) -> Result<usb_vsock::ConnectionState, UsbVsockError> {
        if port == u32::MAX {
            return Err(UsbVsockError::PortOutOfRange);
        }

        let cid = cid.get();
        let cid = if cid == CID_LOOPBACK { CID_HOST } else { cid };

        // TODO(407622394): Handle loopback cases.
        let Some(conn) =
            self.inner.lock().unwrap().conns.get_mut(&cid).map(|x| Arc::clone(&x.connection))
        else {
            return Err(UsbVsockError::NotFound(cid));
        };

        // TODO(407622199): Arrange for this port to be released when the connection dies.
        let host_port = self.alloc_port();

        conn.connect(
            usb_vsock::Address {
                device_cid: cid,
                host_cid: CID_HOST,
                device_port: port,
                host_port,
            },
            socket,
        )
        .await
        .map_err(UsbVsockError::ConnectFailed)
    }

    /// Listen for connections to the host (cid 2) on the given port.
    pub fn listen(
        &self,
        port: u32,
        cid: Option<NonZero<u32>>,
    ) -> Result<impl Stream<Item = (fasync::Socket, usb_vsock::ConnectionState)>, UsbVsockError>
    {
        let cid = cid.map(|x| x.get()).map(|x| if x == CID_LOOPBACK { CID_HOST } else { x });
        let mut inner = self.inner.lock().unwrap();

        // Technically this might not be spec behavior but I think it's smart.
        // See comment in add_incoming_request_handler
        if let Some(cid) = cid {
            if cid > CID_HOST && !inner.conns.contains_key(&cid) {
                return Err(UsbVsockError::NotFound(cid));
            }
        }

        let (sender, receiver) = mpsc::channel(1);

        match (cid, inner.port_states.entry(port)) {
            (Some(cid), std::collections::hash_map::Entry::Vacant(port_state)) => {
                port_state.insert(PortState::Listening(PortListening::ByCid(
                    [(cid, sender)].into_iter().collect(),
                )));
            }
            (None, std::collections::hash_map::Entry::Vacant(port_state)) => {
                port_state.insert(PortState::Listening(PortListening::AnyCid(sender)));
            }
            (_, std::collections::hash_map::Entry::Occupied(mut port_state)) => {
                port_state.get_mut().add_listener(cid, port, sender)?;
            }
        }

        Ok(receiver)
    }

    /// Establish a connection with a USB device and assign it a CID. Returns
    /// true if the device was added (see `add_path`).
    fn add_device(self: &Arc<Self>, device: usb_rs::DeviceHandle) -> bool {
        let interface = match device.scan_interfaces(|device, interface| {
            let subclass_match = u32::from(device.vendor) == BIND_USB_VID_GOOGLE
                && u32::from(interface.class) == BIND_USB_CLASS_VENDOR_SPECIFIC
                && u32::from(interface.subclass) == BIND_USB_SUBCLASS_VSOCK_BRIDGE;
            let protocol_match = u32::from(interface.protocol) == BIND_USB_PROTOCOL_VSOCK_BRIDGE;
            subclass_match && protocol_match
        }) {
            Ok(dev) => dev,
            Err(usb_rs::Error::InterfaceNotFound) => {
                return false;
            }
            Err(e) => {
                tracing::warn!(device = device.debug_name().as_str(), error = ?e,
                               "Error scanning USB device");
                return false;
            }
        };

        let weak_this = Arc::downgrade(self);
        self.scope.spawn(async move {
            let mut cid = None;
            if let Err(e) =
                run_usb_link(weak_this.clone(), device.debug_name(), interface, &mut cid).await
            {
                tracing::warn!("USB link terminated with error: {:?}", e)
            } else {
                tracing::info!("Shut down USB link for {}", device.debug_name())
            }

            if let (Some(this), Some(cid)) = (weak_this.upgrade(), cid) {
                this.remove_device(cid);
            }
        });

        true
    }

    /// Remove a device by CID from the connected devices list and shut down all
    /// related connections.
    fn remove_device(&self, cid: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(got) = inner.conns.remove(&cid) else {
            return;
        };
        std::mem::drop(got);
        for port_state in inner.port_states.values_mut() {
            // If we're being strictly POSIXy I think tehcnically we
            // shouldn't do this. If you want to keep waiting and hope
            // the CID comes back you should be welcome to. In practice
            // it's probably best to notify a listener that the CID is gone.
            if let PortState::Listening(PortListening::ByCid(ports)) = port_state {
                std::mem::drop(ports.remove(&cid));
            }
        }

        let mut sender = self.event_sender.clone();
        self.scope.spawn(async move {
            let _ = sender.send(UsbVsockHostEvent::RemovedCid(cid)).await;
        });
    }

    /// Set up incoming requests for a given connection to be handled correctly.
    fn add_incoming_request_handler(
        self: &Arc<Self>,
        cid: u32,
        mut incoming_requests: mpsc::Receiver<usb_vsock::ConnectionRequest>,
    ) {
        let weak_this = Arc::downgrade(self);
        self.scope.spawn(async move {
            while let Some(incoming) = incoming_requests.next().await {
                let Some(this) = weak_this.upgrade() else {
                    tracing::debug!("Host disappeared before incoming request task terminated");
                    break;
                };

                let (accept_channel, connection) = {
                    let mut inner = this.inner.lock().unwrap();
                    let Some(state) = inner.conns.get_mut(&cid) else {
                        tracing::debug!("Connection state removed before request task terminated");
                        break;
                    };
                    let connection = Arc::clone(&state.connection);

                    let usb_vsock::Address { device_cid, host_cid, device_port: _, host_port } =
                        *incoming.address();

                    let accept_channel = if host_cid != CID_HOST {
                        tracing::warn!("USB device usb:cid:{cid} tried to connect to non-host cid {host_cid}");
                        None
                    } else if device_cid != cid {
                        tracing::warn!("USB device usb:cid:{cid} tried to relay a connection from {device_cid}");
                        None
                    } else {
                        let ret = if let Some(PortState::Listening(ch)) = inner.port_states.get(&host_port) {
                            match ch {
                                PortListening::AnyCid(sender) if !sender.is_closed() => Some(sender.clone()),
                                PortListening::ByCid(senders) => {
                                    senders.get(&device_cid).cloned().filter(|x| !x.is_closed())
                                },
                                _ => None,
                            }
                        } else {
                            None
                        };
                        if ret.is_none() {
                            tracing::debug!("USB device usb:cid:{cid} tried to connect to closed port {host_port}");
                        }
                        ret
                    };

                    (accept_channel, connection)
                };

                if let Some(mut accept_channel) = accept_channel {
                    let (socket, other_end) = fasync::emulated_handle::Socket::create_stream();
                    let socket = fasync::Socket::from_socket(socket);
                    let other_end = fasync::Socket::from_socket(other_end);

                    match connection.accept(incoming, other_end).await {
                        Ok(state) => {
                            if let Err(e) = accept_channel.send((socket, state)).await {
                                tracing::warn!(cid, error = ?e, "Listener disappeared while accepting connection");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(cid, error = ?e, "Accepting connection request failed")
                        }
                    }
                } else {
                    if let Err(e) = connection.reject(incoming).await {
                        tracing::warn!(cid, error = ?e, "Rejecting connection request failed");
                    }
                }
            }
        });
    }
}

/// Collection of values related to a test connection.
pub struct TestConnection {
    pub cid: u32,
    pub host: Arc<UsbVsockHost>,
    pub connection: Arc<usb_vsock::Connection<Vec<u8>>>,
    pub _control_socket: fasync::Socket,
    pub incoming_requests: mpsc::Receiver<usb_vsock::ConnectionRequest>,
    pub abort_transfer: (AbortHandle, AbortHandle),
    pub event_receiver: mpsc::Receiver<UsbVsockHostEvent>,
    pub scope: fasync::Scope,
}

impl TestConnection {
    /// Creates a new host with one connected CID inside of it, and also a raw
    /// usb_vsock connection which is the other end of that connection
    /// (representing the target perspective).
    pub fn new() -> TestConnection {
        let (a_incoming_requests_tx, a_incoming_requests) = mpsc::channel(1);
        let (a_control_socket_other_end, a_control_socket) =
            fasync::emulated_handle::Socket::create_stream();
        let a_control_socket_other_end = fasync::Socket::from_socket(a_control_socket_other_end);
        let a = Arc::new(usb_vsock::Connection::new(
            a_control_socket_other_end,
            a_incoming_requests_tx,
        ));

        let (b_incoming_requests_tx, b_incoming_requests) = mpsc::channel(1);
        let (b_control_socket_other_end, b_control_socket) =
            fasync::emulated_handle::Socket::create_stream();
        let b_control_socket_other_end = fasync::Socket::from_socket(b_control_socket_other_end);
        let b = Arc::new(usb_vsock::Connection::new(
            b_control_socket_other_end,
            b_incoming_requests_tx,
        ));

        let scope = fasync::Scope::new();
        let (abort_a, abort_a_reg) = AbortHandle::new_pair();
        let (abort_b, abort_b_reg) = AbortHandle::new_pair();
        for (from, to, abort_reg) in [
            (Arc::clone(&a), Arc::clone(&b), abort_a_reg),
            (Arc::clone(&b), Arc::clone(&a), abort_b_reg),
        ] {
            scope.spawn(
                Abortable::new(
                    async move {
                        let mut builder = UsbPacketBuilder::new(vec![0u8; MTU]);
                        loop {
                            builder = from.fill_usb_packet(builder).await;
                            let packets =
                                VsockPacketIterator::new(builder.take_usb_packet().unwrap());
                            for packet in packets {
                                to.handle_vsock_packet(packet.unwrap()).await.unwrap();
                            }
                        }
                    },
                    abort_reg,
                )
                .map(std::mem::drop),
            );
        }

        let (event_sender, event_receiver) = mpsc::channel(1);
        let host = UsbVsockHost::new_for_test(event_sender);
        let cid = host.add_connection_for_test(
            a,
            fasync::Socket::from_socket(a_control_socket),
            a_incoming_requests,
        );

        TestConnection {
            cid,
            host,
            connection: b,
            _control_socket: fasync::Socket::from_socket(b_control_socket),
            incoming_requests: b_incoming_requests,
            abort_transfer: (abort_a, abort_b),
            event_receiver,
            scope,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use futures::{AsyncReadExt, AsyncWriteExt};
    use std::u32;

    #[fuchsia::test]
    async fn test_connect() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            mut incoming_requests,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (a, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let connect_task = fasync::Task::spawn(async move {
            host.connect(cid.try_into().unwrap(), 1234, other_end).await
        });

        let incoming = incoming_requests.next().await.unwrap();

        let addr = incoming.address();

        assert_eq!(cid, addr.device_cid);
        assert_eq!(CID_HOST, addr.host_cid);
        assert_eq!(1234, addr.device_port);

        let (b, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let _state = connection.accept(incoming, other_end).await.unwrap();
        connect_task.await.unwrap();

        let mut a = fasync::Socket::from_socket(a);
        let mut b = fasync::Socket::from_socket(b);

        const TEST_STR_1: &[u8] = b"Y'all seem disenchanted with my whimsical diversions.";
        const TEST_STR_2: &[u8] = b"Why were we programmed to get bored anyway?";

        a.write_all(TEST_STR_1).await.unwrap();
        b.write_all(TEST_STR_2).await.unwrap();

        let mut buf = vec![0u8; TEST_STR_2.len()];
        a.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_2);

        let mut buf = vec![0u8; TEST_STR_1.len()];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_1);

        std::mem::drop(b);
        let Err(e) = a.read_exact(&mut buf).await else { panic!() };

        assert_eq!(std::io::ErrorKind::UnexpectedEof, e.kind());
    }

    #[fuchsia::test]
    async fn test_listen() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (a, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let mut listener = host.listen(1234, None).unwrap();
        let connect_task = fasync::Task::spawn(async move {
            connection
                .connect(
                    usb_vsock::Address {
                        device_cid: cid,
                        host_cid: 2,
                        device_port: 16384,
                        host_port: 1234,
                    },
                    other_end,
                )
                .await
        });

        let (mut b, _state) = listener.next().await.unwrap();
        let _remote_state = connect_task.await.unwrap();

        let mut a = fasync::Socket::from_socket(a);

        const TEST_STR_1: &[u8] = b"Y'all seem disenchanted with my whimsical diversions.";
        const TEST_STR_2: &[u8] = b"Why were we programmed to get bored anyway?";

        a.write_all(TEST_STR_1).await.unwrap();
        b.write_all(TEST_STR_2).await.unwrap();

        let mut buf = vec![0u8; TEST_STR_2.len()];
        a.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_2);

        let mut buf = vec![0u8; TEST_STR_1.len()];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_1);
    }

    #[fuchsia::test]
    async fn test_listen_one_cid() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (a, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let mut listener = host.listen(1234, Some(cid.try_into().unwrap())).unwrap();
        let connect_task = fasync::Task::spawn(async move {
            connection
                .connect(
                    usb_vsock::Address {
                        device_cid: cid,
                        host_cid: 2,
                        device_port: 16384,
                        host_port: 1234,
                    },
                    other_end,
                )
                .await
        });

        let (mut b, _state) = listener.next().await.unwrap();
        let _remote_state = connect_task.await.unwrap();

        let mut a = fasync::Socket::from_socket(a);

        const TEST_STR_1: &[u8] = b"Y'all seem disenchanted with my whimsical diversions.";
        const TEST_STR_2: &[u8] = b"Why were we programmed to get bored anyway?";

        a.write_all(TEST_STR_1).await.unwrap();
        b.write_all(TEST_STR_2).await.unwrap();

        let mut buf = vec![0u8; TEST_STR_2.len()];
        a.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_2);

        let mut buf = vec![0u8; TEST_STR_1.len()];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_1);
    }

    #[fuchsia::test]
    async fn test_connect_bad_cid() {
        let (event_sender, _unused) = mpsc::channel(1);
        let host = UsbVsockHost::new_for_test(event_sender);
        let (sock, _) = fasync::emulated_handle::Socket::create_stream();
        let sock = fasync::Socket::from_socket(sock);
        let Err(UsbVsockError::NotFound(got_cid)) =
            host.connect(3.try_into().unwrap(), 1234, sock).await
        else {
            panic!()
        };

        assert_eq!(3, got_cid);
    }

    #[fuchsia::test]
    async fn test_connect_bad_port() {
        let TestConnection {
            cid,
            host,
            connection: _c,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();
        let (sock, _) = fasync::emulated_handle::Socket::create_stream();
        let sock = fasync::Socket::from_socket(sock);
        let Err(UsbVsockError::PortOutOfRange) =
            host.connect(cid.try_into().unwrap(), u32::MAX, sock).await
        else {
            panic!()
        };
    }

    #[fuchsia::test]
    async fn test_connect_rejection() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            mut incoming_requests,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();
        let (sock, _) = fasync::emulated_handle::Socket::create_stream();
        let sock = fasync::Socket::from_socket(sock);
        let connect_task =
            fasync::Task::spawn(
                async move { host.connect(cid.try_into().unwrap(), 1234, sock).await },
            );

        let req = incoming_requests.next().await.unwrap();
        connection.reject(req).await.unwrap();

        let Err(UsbVsockError::ConnectFailed(_)) = connect_task.await else {
            panic!();
        };
    }

    #[fuchsia::test]
    async fn test_refuse_connection() {
        let TestConnection {
            cid,
            host: _host,
            connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();
        let (socket, _) = fasync::emulated_handle::Socket::create_stream();
        let socket = fasync::Socket::from_socket(socket);
        let Err(_) = connection
            .connect(
                usb_vsock::Address {
                    device_cid: cid,
                    host_cid: 2,
                    device_port: 1234,
                    host_port: 1234,
                },
                socket,
            )
            .await
        else {
            panic!();
        };
    }

    #[fuchsia::test]
    async fn test_reject_weird_cid() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (socket, _) = fasync::emulated_handle::Socket::create_stream();
        let socket = fasync::Socket::from_socket(socket);
        let _listener = host.listen(1234, None).unwrap();
        let Err(_) = connection
            .connect(
                usb_vsock::Address {
                    device_cid: cid,
                    host_cid: 60,
                    device_port: 16384,
                    host_port: 1234,
                },
                socket,
            )
            .await
        else {
            panic!();
        };
    }

    #[fuchsia::test]
    async fn test_reject_from_weird_cid() {
        let TestConnection {
            cid: _,
            host,
            connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (socket, _) = fasync::emulated_handle::Socket::create_stream();
        let socket = fasync::Socket::from_socket(socket);
        let _listener = host.listen(1234, None).unwrap();
        let Err(_) = connection
            .connect(
                usb_vsock::Address {
                    device_cid: 60,
                    host_cid: 2,
                    device_port: 16384,
                    host_port: 1234,
                },
                socket,
            )
            .await
        else {
            panic!();
        };
    }

    #[fuchsia::test]
    async fn test_double_listen() {
        let TestConnection {
            cid: _,
            host,
            connection: _connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let _listener = host.listen(1234, None).unwrap();
        let Err(UsbVsockError::PortInUse(port)) = host.listen(1234, None) else {
            panic!();
        };
        assert_eq!(1234, port);
    }

    #[fuchsia::test]
    async fn test_connect_then_listen() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            mut incoming_requests,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (socket, _) = fasync::emulated_handle::Socket::create_stream();
        let socket = fasync::Socket::from_socket(socket);
        let connect_host = Arc::clone(&host);
        let connect_task = fasync::Task::spawn(async move {
            connect_host.connect(cid.try_into().unwrap(), 1234, socket).await
        });

        let request = incoming_requests.next().await.unwrap();
        let usb_vsock::Address { device_cid: _, host_cid: _, device_port: _, host_port } =
            *request.address();

        let (socket, _) = fasync::emulated_handle::Socket::create_stream();
        let socket = fasync::Socket::from_socket(socket);
        let _remote_state = connection.accept(request, socket).await.unwrap();
        let _state = connect_task.await.unwrap();

        let Err(UsbVsockError::PortInUse(port)) = host.listen(host_port, None) else {
            panic!();
        };
        assert_eq!(host_port, port);
    }

    #[fuchsia::test]
    async fn test_double_listen_second_narrower() {
        let TestConnection {
            cid,
            host,
            connection: _connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let _listener = host.listen(1234, None).unwrap();
        let Err(UsbVsockError::PortInUse(port)) = host.listen(1234, Some(cid.try_into().unwrap()))
        else {
            panic!();
        };
        assert_eq!(1234, port);
    }

    #[fuchsia::test]
    async fn test_double_listen_second_broader() {
        let TestConnection {
            cid,
            host,
            connection: _connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let _listener = host.listen(1234, Some(cid.try_into().unwrap())).unwrap();
        let Err(UsbVsockError::PortInUse(port)) = host.listen(1234, None) else {
            panic!();
        };
        assert_eq!(1234, port);
    }

    #[fuchsia::test]
    async fn test_listen_bad_cid() {
        let TestConnection {
            cid: _,
            host,
            connection: _connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let Err(UsbVsockError::NotFound(cid)) = host.listen(1234, Some(60.try_into().unwrap()))
        else {
            panic!();
        };
        assert_eq!(60, cid);
    }

    #[fuchsia::test]
    async fn test_listen_dropped_conn() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let mut listener = host.listen(1234, Some(cid.try_into().unwrap())).unwrap();

        assert!(listener
            .poll_next_unpin(&mut std::task::Context::from_waker(futures::task::noop_waker_ref()))
            .is_pending());
        host.remove_device(cid);
        std::mem::drop(connection);
        let Err(UsbVsockError::NotFound(got_cid)) =
            host.listen(5678, Some(cid.try_into().unwrap()))
        else {
            panic!();
        };
        assert_eq!(cid, got_cid);
    }

    #[fuchsia::test]
    async fn test_connect_then_drop_cid() {
        let TestConnection {
            cid,
            host,
            connection,
            _control_socket,
            mut incoming_requests,
            abort_transfer: (abort_a, abort_b),
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let (a, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let connect_task = {
            let host = Arc::clone(&host);
            fasync::Task::spawn(async move {
                host.connect(cid.try_into().unwrap(), 1234, other_end).await
            })
        };

        let incoming = incoming_requests.next().await.unwrap();

        let addr = incoming.address();

        assert_eq!(cid, addr.device_cid);
        assert_eq!(2, addr.host_cid);
        assert_eq!(1234, addr.device_port);

        let (b, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let _state = connection.accept(incoming, other_end).await.unwrap();
        connect_task.await.unwrap();
        std::mem::drop(connection);

        let mut a = fasync::Socket::from_socket(a);
        let mut b = fasync::Socket::from_socket(b);

        const TEST_STR_1: &[u8] = b"Y'all seem disenchanted with my whimsical diversions.";

        a.write_all(TEST_STR_1).await.unwrap();

        let mut buf = vec![0u8; TEST_STR_1.len()];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, TEST_STR_1);

        host.remove_device(cid);
        abort_a.abort();
        abort_b.abort();

        let Err(e) = b.read_exact(&mut buf).await else {
            panic!();
        };

        assert_eq!(std::io::ErrorKind::UnexpectedEof, e.kind());

        let Err(e) = a.read_exact(&mut buf).await else {
            panic!();
        };

        assert_eq!(std::io::ErrorKind::UnexpectedEof, e.kind());

        let (_unused, other_end) = fasync::emulated_handle::Socket::create_stream();
        let other_end = fasync::Socket::from_socket(other_end);
        let Err(UsbVsockError::NotFound(got_cid)) =
            host.connect(cid.try_into().unwrap(), 1234, other_end).await
        else {
            panic!();
        };

        assert_eq!(cid, got_cid);
    }

    #[fuchsia::test]
    async fn test_listen_drop_listen() {
        let TestConnection {
            cid: _,
            host,
            connection: _connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            event_receiver: _,
            scope: _scope,
        } = TestConnection::new();

        let listener = host.listen(1234, None).unwrap();
        std::mem::drop(listener);
        let _listener = host.listen(1234, None).unwrap();
    }

    #[fuchsia::test]
    async fn test_events() {
        let TestConnection {
            cid,
            host,
            connection: _connection,
            _control_socket,
            incoming_requests: _,
            abort_transfer: _,
            mut event_receiver,
            scope: _scope,
        } = TestConnection::new();

        let Some(UsbVsockHostEvent::AddedCid(got_cid)) = event_receiver.next().await else {
            panic!();
        };

        assert_eq!(cid, got_cid);

        host.remove_device(cid);

        let Some(UsbVsockHostEvent::RemovedCid(got_cid)) = event_receiver.next().await else {
            panic!();
        };

        assert_eq!(cid, got_cid);
    }
}
