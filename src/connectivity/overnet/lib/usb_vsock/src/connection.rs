// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::channel::{mpsc, oneshot};
use futures::lock::Mutex;
use log::{debug, trace, warn};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::ops::DerefMut;
use std::sync::Arc;

use fuchsia_async::{Scope, Socket};
use futures::io::{ReadHalf, WriteHalf};
use futures::{AsyncReadExt, AsyncWriteExt, SinkExt, StreamExt};

use crate::{Address, Header, Packet, PacketType, UsbPacketBuilder, UsbPacketFiller};

/// A marker trait for types that are capable of being used as buffers for a [`Connection`].
pub trait PacketBuffer: DerefMut<Target = [u8]> + Send + Unpin + 'static {}
impl<T> PacketBuffer for T where T: DerefMut<Target = [u8]> + Send + Unpin + 'static {}

/// Manages the state of a vsock-over-usb connection and the sockets over which data is being
/// transmitted for them.
///
/// This implementation aims to be agnostic to both the underlying transport and the buffers used
/// to read and write from it. The buffer type must conform to [`PacketBuffer`], which is essentially
/// a type that holds a mutable slice of bytes and is [`Send`] and [`Unpin`]-able.
///
/// The client of this library will:
/// - Use methods on this struct to initiate actions like connecting and accepting
/// connections to the other end.
/// - Provide buffers to be filled and sent to the other end with [`Connection::fill_usb_packet`].
/// - Pump usb packets received into it using [`Connection::handle_vsock_packet`].
pub struct Connection<B> {
    control_socket_writer: Mutex<WriteHalf<Socket>>,
    packet_filler: Arc<UsbPacketFiller<B>>,
    connections: std::sync::Mutex<HashMap<Address, VsockConnection>>,
    incoming_requests_tx: mpsc::Sender<ConnectionRequest>,
    _task_scope: Scope,
}

impl<B: PacketBuffer> Connection<B> {
    /// Creates a new connection with:
    /// - a `control_socket`, over which data addressed to and from cid 0, port 0 (a control channel
    /// between host and device) can be read and written from.
    /// - An `incoming_requests_tx` that is the sender half of a request queue for incoming
    /// connection requests from the other side.
    pub fn new(
        control_socket: Socket,
        incoming_requests_tx: mpsc::Sender<ConnectionRequest>,
    ) -> Self {
        let (control_socket_reader, control_socket_writer) = control_socket.split();
        let control_socket_writer = Mutex::new(control_socket_writer);
        let packet_filler = Arc::new(UsbPacketFiller::default());
        let connections = Default::default();
        let task_scope = Scope::new_with_name("vsock_usb");
        task_scope.spawn(Self::run_socket(
            control_socket_reader,
            Address::default(),
            packet_filler.clone(),
        ));
        Self {
            control_socket_writer,
            packet_filler,
            connections,
            incoming_requests_tx,
            _task_scope: task_scope,
        }
    }

    async fn send_close_packet(address: &Address, usb_packet_filler: &Arc<UsbPacketFiller<B>>) {
        let header = &mut Header::new(PacketType::Finish);
        header.set_address(address);
        usb_packet_filler
            .write_vsock_packet(&Packet { header, payload: &[] })
            .await
            .expect("Finish packet should never be too big");
    }

    async fn run_socket(
        mut reader: ReadHalf<Socket>,
        address: Address,
        usb_packet_filler: Arc<UsbPacketFiller<B>>,
    ) {
        let mut buf = [0; 4096];
        loop {
            log::trace!("reading from control socket");
            let read = match reader.read(&mut buf).await {
                Ok(0) => {
                    if !address.is_zeros() {
                        Self::send_close_packet(&address, &usb_packet_filler).await;
                    }
                    return;
                }
                Ok(read) => read,
                Err(err) => {
                    if address.is_zeros() {
                        log::error!("Error reading usb socket: {err:?}");
                    } else {
                        Self::send_close_packet(&address, &usb_packet_filler).await;
                    }
                    return;
                }
            };
            log::trace!("writing {read} bytes to vsock packet");
            usb_packet_filler.write_vsock_data_all(&address, &buf[..read]).await;
            log::trace!("wrote {read} bytes to vsock packet");
        }
    }

    fn set_connection(&self, address: Address, state: VsockConnectionState) -> Result<(), Error> {
        let mut connections = self.connections.lock().unwrap();
        if !connections.contains_key(&address) {
            connections.insert(address.clone(), VsockConnection { _address: address, state });
            Ok(())
        } else {
            Err(Error::other(format!("connection on address {address:?} already set")))
        }
    }

    /// Starts a connection attempt to the other end of the USB connection, and provides a socket
    /// to read and write from. The function will complete when the other end has accepted or
    /// rejected the connection, and the returned [`ConnectionState`] handle can be used to wait
    /// for the connection to be closed.
    pub async fn connect(&self, addr: Address, socket: Socket) -> Result<ConnectionState, Error> {
        let (read_socket, write_socket) = socket.split();
        let write_socket = Arc::new(Mutex::new(write_socket));
        let (connected_tx, connected_rx) = oneshot::channel();

        self.set_connection(
            addr.clone(),
            VsockConnectionState::ConnectingOutgoing(write_socket, read_socket, connected_tx),
        )?;

        let header = &mut Header::new(PacketType::Connect);
        header.set_address(&addr);
        self.packet_filler.write_vsock_packet(&Packet { header, payload: &[] }).await.unwrap();
        connected_rx.await.map_err(|_| Error::other("Accept was never received for {addr:?}"))?
    }

    /// Sends a request for the other end to close the connection.
    pub async fn close(&self, address: &Address) {
        Self::send_close_packet(address, &self.packet_filler).await
    }

    /// Resets the named connection without going through a close request.
    pub async fn reset(&self, address: &Address) -> Result<(), Error> {
        let mut notify = None;
        if let Some(conn) = self.connections.lock().unwrap().remove(&address) {
            if let VsockConnectionState::Connected { notify_closed, .. } = conn.state {
                notify = Some(notify_closed);
            }
        } else {
            return Err(Error::other(
                "Client asked to reset connection {address:?} that did not exist",
            ));
        }

        if let Some(mut notify) = notify {
            notify.send(Err(ErrorKind::ConnectionReset.into())).await.ok();
        }

        let header = &mut Header::new(PacketType::Reset);
        header.set_address(address);
        self.packet_filler
            .write_vsock_packet(&Packet { header, payload: &[] })
            .await
            .expect("Reset packet should never be too big");
        Ok(())
    }

    /// Accepts a connection for which an outstanding connection request has been made, and
    /// provides a socket to read and write data packets to and from. The returned [`ConnectionState`]
    /// can be used to wait for the connection to be closed.
    pub async fn accept(
        &self,
        request: ConnectionRequest,
        socket: Socket,
    ) -> Result<ConnectionState, Error> {
        let address = request.address;
        let notify_closed_rx;
        if let Some(conn) = self.connections.lock().unwrap().get_mut(&address) {
            let VsockConnectionState::ConnectingIncoming = &conn.state else {
                return Err(Error::other(format!(
                    "Attempted to accept connection that was not waiting at {address:?}"
                )));
            };

            let (read_socket, write_socket) = socket.split();
            let writer = Arc::new(Mutex::new(write_socket));
            let notify_closed = mpsc::channel(2);
            notify_closed_rx = notify_closed.1;
            let notify_closed = notify_closed.0;

            let reader_task = Scope::new_with_name("connection-reader");
            reader_task.spawn(Self::run_socket(read_socket, address, self.packet_filler.clone()));

            conn.state = VsockConnectionState::Connected {
                writer,
                _reader_scope: reader_task,
                notify_closed,
            };
        } else {
            return Err(Error::other(format!(
                "Attempting to accept connection that did not exist at {address:?}"
            )));
        }
        let header = &mut Header::new(PacketType::Accept);
        header.set_address(&address);
        self.packet_filler.write_vsock_packet(&Packet { header, payload: &[] }).await.unwrap();
        Ok(ConnectionState(notify_closed_rx))
    }

    /// Rejects a pending connection request from the other side.
    pub async fn reject(&self, request: ConnectionRequest) -> Result<(), Error> {
        let address = request.address;
        match self.connections.lock().unwrap().entry(address.clone()) {
            Entry::Occupied(entry) => {
                let VsockConnectionState::ConnectingIncoming = &entry.get().state else {
                    return Err(Error::other(format!(
                        "Attempted to reject connection that was not waiting at {address:?}"
                    )));
                };
                entry.remove();
            }
            Entry::Vacant(_) => {
                return Err(Error::other(format!(
                    "Attempted to reject connection that was not waiting at {address:?}"
                )));
            }
        }

        let header = &mut Header::new(PacketType::Reset);
        header.set_address(&address);
        self.packet_filler
            .write_vsock_packet(&Packet { header, payload: &[] })
            .await
            .expect("accept packet should never be too large for packet buffer");
        Ok(())
    }

    async fn handle_data_packet(&self, address: Address, payload: &[u8]) -> Result<(), Error> {
        // all zero data packets go to the control channel
        if address.is_zeros() {
            let written = self.control_socket_writer.lock().await.write(payload).await?;
            assert_eq!(written, payload.len());
            Ok(())
        } else {
            let payload_socket;
            if let Some(conn) = self.connections.lock().unwrap().get_mut(&address) {
                let VsockConnectionState::Connected { writer, .. } = &conn.state else {
                    warn!(
                        "Received data packet for connection in unexpected state for {address:?}"
                    );
                    return Ok(());
                };
                payload_socket = writer.clone();
            } else {
                warn!("Received data packet for connection that didn't exist at {address:?}");
                return Ok(());
            }
            payload_socket.lock().await.write_all(payload).await.expect("BOOM do not submit");
            Ok(())
        }
    }

    async fn handle_accept_packet(&self, address: Address) -> Result<(), Error> {
        if let Some(conn) = self.connections.lock().unwrap().get_mut(&address) {
            let state = std::mem::replace(&mut conn.state, VsockConnectionState::Invalid);
            let VsockConnectionState::ConnectingOutgoing(writer, read_socket, connected_tx) = state
            else {
                warn!("Received accept packet for connection in unexpected state for {address:?}");
                return Ok(());
            };
            let (notify_closed, notify_closed_rx) = mpsc::channel(2);
            if connected_tx.send(Ok(ConnectionState(notify_closed_rx))).is_err() {
                warn!("Accept packet received for {address:?} but connect caller stopped waiting for it");
            }

            let reader_task = Scope::new_with_name("connection-reader");
            reader_task.spawn(Self::run_socket(read_socket, address, self.packet_filler.clone()));
            conn.state = VsockConnectionState::Connected {
                writer,
                _reader_scope: reader_task,
                notify_closed,
            };
        } else {
            warn!("Got accept packet for connection that was not being made at {address:?}");
            return Ok(());
        }
        Ok(())
    }

    async fn handle_connect_packet(&self, address: Address) -> Result<(), Error> {
        trace!("received connect packet for {address:?}");
        match self.connections.lock().unwrap().entry(address.clone()) {
            Entry::Vacant(entry) => {
                debug!("valid connect request for {address:?}");
                entry.insert(VsockConnection {
                    _address: address,
                    state: VsockConnectionState::ConnectingIncoming,
                });
            }
            Entry::Occupied(_) => {
                warn!("Received connect packet for already existing connection for address {address:?}. Ignoring");
                return Ok(());
            }
        }

        trace!("sending incoming connection request to client for {address:?}");
        let connection_request = ConnectionRequest { address };
        self.incoming_requests_tx
            .clone()
            .send(connection_request)
            .await
            .inspect(|_| trace!("sent incoming request for {address:?}"))
            .map_err(|_| Error::other("Failed to send connection request"))
    }

    async fn handle_finish_packet(&self, address: Address) -> Result<(), Error> {
        trace!("received finish packet for {address:?}");
        let mut notify;
        if let Some(conn) = self.connections.lock().unwrap().remove(&address) {
            let VsockConnectionState::Connected { notify_closed, .. } = conn.state else {
                warn!("Received finish (close) packet for {address:?} which was not in a connected state. Ignoring and dropping connection state.");
                return Ok(());
            };
            notify = notify_closed;
        } else {
            warn!("Received finish (close) packet for connection that didn't exist on address {address:?}. Ignoring");
            return Ok(());
        }

        notify.send(Ok(())).await.ok();

        let header = &mut Header::new(PacketType::Reset);
        header.set_address(&address);
        self.packet_filler
            .write_vsock_packet(&Packet { header, payload: &[] })
            .await
            .expect("accept packet should never be too large for packet buffer");
        Ok(())
    }

    async fn handle_reset_packet(&self, address: Address) -> Result<(), Error> {
        trace!("received reset packet for {address:?}");
        let mut notify = None;
        if let Some(conn) = self.connections.lock().unwrap().remove(&address) {
            if let VsockConnectionState::Connected { notify_closed, .. } = conn.state {
                notify = Some(notify_closed);
            } else {
                debug!("Received reset packet for connection that wasn't in a connecting or disconnected state on address {address:?}.");
            }
        } else {
            warn!("Received reset packet for connection that didn't exist on address {address:?}. Ignoring");
        }

        if let Some(mut notify) = notify {
            notify.send(Ok(())).await.ok();
        }
        Ok(())
    }

    /// Dispatches the given vsock packet type and handles its effect on any outstanding connections
    /// or the overall state of the connection.
    pub async fn handle_vsock_packet(&self, packet: Packet<'_>) -> Result<(), Error> {
        trace!("received vsock packet {header:?}", header = packet.header);
        let payload_len = packet.header.payload_len.get() as usize;
        let payload = &packet.payload[..payload_len];
        let address = Address::from(packet.header);
        match packet.header.packet_type {
            PacketType::Sync => Err(Error::other("Received sync packet mid-stream")),
            PacketType::Data => self.handle_data_packet(address, payload).await,
            PacketType::Accept => self.handle_accept_packet(address).await,
            PacketType::Connect => self.handle_connect_packet(address).await,
            PacketType::Finish => self.handle_finish_packet(address).await,
            PacketType::Reset => self.handle_reset_packet(address).await,
        }
    }

    /// Provides a packet builder for the state machine to write packets to. Returns a future that
    /// will be fulfilled when there is data available to send on the packet.
    ///
    /// # Panics
    ///
    /// Panics if called while another [`Self::fill_usb_packet`] future is pending.
    pub async fn fill_usb_packet(&self, builder: UsbPacketBuilder<B>) -> UsbPacketBuilder<B> {
        self.packet_filler.fill_usb_packet(builder).await
    }
}

enum VsockConnectionState {
    ConnectingOutgoing(
        Arc<Mutex<WriteHalf<Socket>>>,
        ReadHalf<Socket>,
        oneshot::Sender<Result<ConnectionState, Error>>,
    ),
    ConnectingIncoming,
    Connected {
        writer: Arc<Mutex<WriteHalf<Socket>>>,
        notify_closed: mpsc::Sender<Result<(), Error>>,
        _reader_scope: Scope,
    },
    Invalid,
}

struct VsockConnection {
    _address: Address,
    state: VsockConnectionState,
}

/// A handle for the state of a connection established with either [`Connection::connect`] or
/// [`Connection::accept`]. Use this to get notified when the connection has been closed without
/// needing to hold on to the Socket end.
pub struct ConnectionState(mpsc::Receiver<Result<(), Error>>);

impl ConnectionState {
    /// Wait for this connection to close. Returns Ok(()) if the connection was closed without error,
    /// and an error if it closed because of an error.
    pub async fn wait_for_close(mut self) -> Result<(), Error> {
        self.0
            .next()
            .await
            .ok_or_else(|| Error::other("Connection state's other end was dropped"))?
    }
}

/// An outstanding connection request that needs to be either [`Connection::accept`]ed or
/// [`Connection::reject`]ed.
pub struct ConnectionRequest {
    address: Address,
}

impl ConnectionRequest {
    /// Creates a new connection request for the given address.
    pub fn new(address: Address) -> Self {
        Self { address }
    }

    /// The address this connection request is being made for.
    pub fn address(&self) -> &Address {
        &self.address
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use crate::VsockPacketIterator;

    use super::*;

    #[cfg(not(target_os = "fuchsia"))]
    use fuchsia_async::emulated_handle::Socket as SyncSocket;
    use fuchsia_async::Task;
    use futures::StreamExt;
    #[cfg(target_os = "fuchsia")]
    use zx::Socket as SyncSocket;

    async fn usb_echo_server(echo_connection: Arc<Connection<Vec<u8>>>) {
        let mut builder = UsbPacketBuilder::new(vec![0; 128]);
        loop {
            println!("waiting for usb packet");
            builder = echo_connection.fill_usb_packet(builder).await;
            let packets = VsockPacketIterator::new(builder.take_usb_packet().unwrap());
            println!("got usb packet, echoing it back to the other side");
            let mut packet_count = 0;
            for packet in packets {
                let packet = packet.unwrap();
                match packet.header.packet_type {
                    PacketType::Connect => {
                        // respond with an accept packet
                        let mut reply_header = packet.header.clone();
                        reply_header.packet_type = PacketType::Accept;
                        echo_connection
                            .handle_vsock_packet(Packet { header: &reply_header, payload: &[] })
                            .await
                            .unwrap();
                    }
                    PacketType::Accept => {
                        // just ignore it
                    }
                    _ => echo_connection.handle_vsock_packet(packet).await.unwrap(),
                }
                packet_count += 1;
            }
            println!("handled {packet_count} packets");
        }
    }

    #[fuchsia::test]
    async fn data_over_control_socket() {
        let (socket, other_socket) = SyncSocket::create_stream();
        let (incoming_requests_tx, _incoming_requests) = mpsc::channel(5);
        let mut socket = Socket::from_socket(socket);
        let connection =
            Arc::new(Connection::new(Socket::from_socket(other_socket), incoming_requests_tx));

        let echo_task = Task::spawn(usb_echo_server(connection.clone()));

        for size in [1u8, 2, 8, 16, 32, 64, 128, 255] {
            println!("round tripping packet of size {size}");
            socket.write_all(&vec![size; size as usize]).await.unwrap();
            let mut buf = vec![0u8; size as usize];
            socket.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, vec![size; size as usize]);
        }
        echo_task.cancel().await;
    }

    #[fuchsia::test]
    async fn data_over_normal_outgoing_socket() {
        let (_control_socket, other_socket) = SyncSocket::create_stream();
        let (incoming_requests_tx, _incoming_requests) = mpsc::channel(5);
        let connection =
            Arc::new(Connection::new(Socket::from_socket(other_socket), incoming_requests_tx));

        let echo_task = Task::spawn(usb_echo_server(connection.clone()));

        let (socket, other_socket) = SyncSocket::create_stream();
        let mut socket = Socket::from_socket(socket);
        connection
            .connect(
                Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 },
                Socket::from_socket(other_socket),
            )
            .await
            .unwrap();

        for size in [1u8, 2, 8, 16, 32, 64, 128, 255] {
            println!("round tripping packet of size {size}");
            socket.write_all(&vec![size; size as usize]).await.unwrap();
            let mut buf = vec![0u8; size as usize];
            socket.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, vec![size; size as usize]);
        }
        echo_task.cancel().await;
    }

    #[fuchsia::test]
    async fn data_over_normal_incoming_socket() {
        let (_control_socket, other_socket) = SyncSocket::create_stream();
        let (incoming_requests_tx, mut incoming_requests) = mpsc::channel(5);
        let connection =
            Arc::new(Connection::new(Socket::from_socket(other_socket), incoming_requests_tx));

        let echo_task = Task::spawn(usb_echo_server(connection.clone()));

        let header = &mut Header::new(PacketType::Connect);
        header.set_address(&Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 });
        connection.handle_vsock_packet(Packet { header, payload: &[] }).await.unwrap();

        let request = incoming_requests.next().await.unwrap();
        assert_eq!(
            request.address,
            Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 }
        );

        let (socket, other_socket) = SyncSocket::create_stream();
        let mut socket = Socket::from_socket(socket);
        connection.accept(request, Socket::from_socket(other_socket)).await.unwrap();

        for size in [1u8, 2, 8, 16, 32, 64, 128, 255] {
            println!("round tripping packet of size {size}");
            socket.write_all(&vec![size; size as usize]).await.unwrap();
            let mut buf = vec![0u8; size as usize];
            socket.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, vec![size; size as usize]);
        }
        echo_task.cancel().await;
    }

    async fn copy_connection(from: &Connection<Vec<u8>>, to: &Connection<Vec<u8>>) {
        let mut builder = UsbPacketBuilder::new(vec![0; 1024]);
        loop {
            builder = from.fill_usb_packet(builder).await;
            let packets = VsockPacketIterator::new(builder.take_usb_packet().unwrap());
            for packet in packets {
                println!("forwarding vsock packet");
                to.handle_vsock_packet(packet.unwrap()).await.unwrap();
            }
        }
    }

    pub(crate) trait EndToEndTestFn<R>:
        AsyncFnOnce(Arc<Connection<Vec<u8>>>, mpsc::Receiver<ConnectionRequest>) -> R
    {
    }
    impl<T, R> EndToEndTestFn<R> for T where
        T: AsyncFnOnce(Arc<Connection<Vec<u8>>>, mpsc::Receiver<ConnectionRequest>) -> R
    {
    }

    pub(crate) async fn end_to_end_test<R1, R2>(
        left_side: impl EndToEndTestFn<R1>,
        right_side: impl EndToEndTestFn<R2>,
    ) -> (R1, R2) {
        type Connection = crate::Connection<Vec<u8>>;
        let (_control_socket1, other_socket1) = SyncSocket::create_stream();
        let (_control_socket2, other_socket2) = SyncSocket::create_stream();
        let (incoming_requests_tx1, incoming_requests1) = mpsc::channel(5);
        let (incoming_requests_tx2, incoming_requests2) = mpsc::channel(5);

        let connection1 =
            Arc::new(Connection::new(Socket::from_socket(other_socket1), incoming_requests_tx1));
        let connection2 =
            Arc::new(Connection::new(Socket::from_socket(other_socket2), incoming_requests_tx2));

        let conn1 = connection1.clone();
        let conn2 = connection2.clone();
        let passthrough_task = Task::spawn(async move {
            futures::join!(copy_connection(&conn1, &conn2), copy_connection(&conn2, &conn1),);
            println!("passthrough task loop ended");
        });

        let res = futures::join!(
            left_side(connection1, incoming_requests1),
            right_side(connection2, incoming_requests2)
        );
        passthrough_task.cancel().await;
        res
    }

    #[fuchsia::test]
    async fn data_over_end_to_end() {
        end_to_end_test(
            async |conn, _incoming| {
                println!("sending request on connection 1");
                let (socket, other_socket) = SyncSocket::create_stream();
                let mut socket = Socket::from_socket(socket);
                let state = conn
                    .connect(
                        Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 },
                        Socket::from_socket(other_socket),
                    )
                    .await
                    .unwrap();

                for size in [1u8, 2, 8, 16, 32, 64, 128, 255] {
                    println!("round tripping packet of size {size}");
                    socket.write_all(&vec![size; size as usize]).await.unwrap();
                }
                drop(socket);
                state.wait_for_close().await.unwrap();
            },
            async |conn, mut incoming| {
                println!("accepting request on connection 2");
                let request = incoming.next().await.unwrap();
                assert_eq!(
                    request.address,
                    Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 }
                );

                let (socket, other_socket) = SyncSocket::create_stream();
                let mut socket = Socket::from_socket(socket);
                let state = conn.accept(request, Socket::from_socket(other_socket)).await.unwrap();

                println!("accepted request on connection 2");
                for size in [1u8, 2, 8, 16, 32, 64, 128, 255] {
                    let mut buf = vec![0u8; size as usize];
                    socket.read_exact(&mut buf).await.unwrap();
                    assert_eq!(buf, vec![size; size as usize]);
                }
                assert_eq!(socket.read(&mut [0u8; 1]).await.unwrap(), 0);
                state.wait_for_close().await.unwrap();
            },
        )
        .await;
    }

    #[fuchsia::test]
    async fn normal_close_end_to_end() {
        let addr = Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 };
        end_to_end_test(
            async |conn, _incoming| {
                let (socket, other_socket) = SyncSocket::create_stream();
                let mut socket = Socket::from_socket(socket);
                let state =
                    conn.connect(addr.clone(), Socket::from_socket(other_socket)).await.unwrap();
                conn.close(&addr).await;
                assert_eq!(socket.read(&mut [0u8; 1]).await.unwrap(), 0);
                state.wait_for_close().await.unwrap();
            },
            async |conn, mut incoming| {
                println!("accepting request on connection 2");
                let request = incoming.next().await.unwrap();
                assert_eq!(request.address, addr.clone(),);

                let (socket, other_socket) = SyncSocket::create_stream();
                let mut socket = Socket::from_socket(socket);
                let state = conn.accept(request, Socket::from_socket(other_socket)).await.unwrap();
                assert_eq!(socket.read(&mut [0u8; 1]).await.unwrap(), 0);
                state.wait_for_close().await.unwrap();
            },
        )
        .await;
    }

    #[fuchsia::test]
    async fn reset_end_to_end() {
        let addr = Address { device_cid: 1, host_cid: 2, device_port: 3, host_port: 4 };
        end_to_end_test(
            async |conn, _incoming| {
                let (socket, other_socket) = SyncSocket::create_stream();
                let mut socket = Socket::from_socket(socket);
                let state =
                    conn.connect(addr.clone(), Socket::from_socket(other_socket)).await.unwrap();
                conn.reset(&addr).await.unwrap();
                assert_eq!(socket.read(&mut [0u8; 1]).await.unwrap(), 0);
                state.wait_for_close().await.expect_err("expected reset");
            },
            async |conn, mut incoming| {
                println!("accepting request on connection 2");
                let request = incoming.next().await.unwrap();
                assert_eq!(request.address, addr.clone(),);

                let (socket, other_socket) = SyncSocket::create_stream();
                let mut socket = Socket::from_socket(socket);
                let state = conn.accept(request, Socket::from_socket(other_socket)).await.unwrap();
                assert_eq!(socket.read(&mut [0u8; 1]).await.unwrap(), 0);
                state.wait_for_close().await.unwrap();
            },
        )
        .await;
    }
}
