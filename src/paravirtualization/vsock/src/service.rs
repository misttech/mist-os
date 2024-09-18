// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This module contains the bulk of the logic for connecting user applications to a
// vsock driver.
//
// Handling user requests is complicated as there are multiple communication channels
// involved. For example a request to 'connect' will result in sending a message
// to the driver over the single DeviceProxy. If this returns with success then
// eventually a message will come over the single Callbacks stream indicating
// whether the remote accepted or rejected.
//
// Fundamentally then there needs to be mutual exclusion in accessing DeviceProxy,
// and de-multiplexing of incoming messages on the Callbacks stream. There are
// a two high level options for doing this.
//  1. Force a single task event driver model. This would mean that additional
//     asynchronous executions are never spawned, and any use of await! or otherwise
//     blocking with additional futures requires collection futures in future sets
//     or having custom polling logic etc. Whilst this is probably the most resource
//     efficient it restricts the service to be single task forever by its design,
//     is harder to reason about as cannot be written very idiomatically with futures
//     and is even more complicated to avoid blocking other requests whilst waiting
//     on responses from the driver.
//  2. Allow multiple asynchronous executions and use some form of message passing
//     and mutual exclusion checking to handle DeviceProxy access and sharing access
//     to the Callbacks stream. Potentially more resource intensive with unnecessary
//     refcells etc, but allows for the potential to have actual concurrent execution
//     and is much simpler to write the logic.
// The chosen option is (2) and the access to DeviceProxy is handled with an Rc<Refcell<State>>,
// and de-multiplexing of the Callbacks is done by registering an event whilst holding
// the refcell, and having a single asynchronous task that is dedicated to converting
// incoming Callbacks to signaling registered events.

use crate::{addr, port};
use anyhow::{format_err, Context as _};
use fidl::endpoints;
use fidl_fuchsia_hardware_vsock::{
    CallbacksMarker, CallbacksRequest, CallbacksRequestStream, DeviceProxy,
};
use fidl_fuchsia_vsock::{
    AcceptorProxy, ConnectionRequest, ConnectionRequestStream, ConnectionTransport,
    ConnectorRequest, ConnectorRequestStream,
};
use futures::channel::{mpsc, oneshot};
use futures::{future, select, Future, FutureExt, Stream, StreamExt, TryFutureExt, TryStreamExt};
use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::convert::Infallible;
use std::ops::Deref;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use thiserror::Error;
use {fuchsia_async as fasync, fuchsia_zircon as zx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EventType {
    Shutdown,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Event {
    action: EventType,
    addr: addr::Vsock,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum Deregister {
    Event(Event),
    Listen(u32),
    Port(u32),
}

#[derive(Error, Debug)]
enum Error {
    #[error("Driver returned failure status {}", _0)]
    Driver(#[source] zx::Status),
    #[error("All ephemeral ports are allocated")]
    OutOfPorts,
    #[error("Addr has already been bound")]
    AlreadyBound,
    #[error("Connection refused by remote")]
    ConnectionRefused,
    #[error("Error whilst communication with client")]
    ClientCommunication(#[source] anyhow::Error),
    #[error("Error whilst communication with client")]
    DriverCommunication(#[source] anyhow::Error),
    #[error("Driver reset the connection")]
    ConnectionReset,
}

impl From<oneshot::Canceled> for Error {
    fn from(_: oneshot::Canceled) -> Error {
        Error::ConnectionReset
    }
}

impl Error {
    pub fn into_status(&self) -> zx::Status {
        match self {
            Error::Driver(status) => *status,
            Error::OutOfPorts => zx::Status::NO_RESOURCES,
            Error::AlreadyBound => zx::Status::ALREADY_BOUND,
            Error::ConnectionRefused => zx::Status::UNAVAILABLE,
            Error::ClientCommunication(err) | Error::DriverCommunication(err) => {
                *err.downcast_ref::<zx::Status>().unwrap_or(&zx::Status::INTERNAL)
            }
            Error::ConnectionReset => zx::Status::PEER_CLOSED,
        }
    }
    pub fn is_comm_failure(&self) -> bool {
        match self {
            Error::ClientCommunication(_) | Error::DriverCommunication(_) => true,
            _ => false,
        }
    }
}

fn map_driver_result(result: Result<i32, fidl::Error>) -> Result<(), Error> {
    result
        .map_err(|x| Error::DriverCommunication(x.into()))
        .and_then(|x| zx::Status::ok(x).map_err(Error::Driver))
}

struct State {
    device: DeviceProxy,
    events: HashMap<Event, oneshot::Sender<()>>,
    used_ports: port::Tracker,
    listens: HashMap<u32, mpsc::UnboundedSender<addr::Vsock>>,
    tasks: fasync::TaskGroup,
}

#[derive(Clone)]
pub struct Vsock {
    inner: Rc<RefCell<State>>,
}

impl Vsock {
    /// Creates a new vsock service connected to the given `DeviceProxy`
    ///
    /// The creation is asynchronous due to need to invoke methods on the given `DeviceProxy`. On
    /// success a pair of `Self, impl Future<Result<_, Error>>` is returned. The `impl Future` is
    /// a future that is listening for and processing messages from the `device`. This future needs
    /// to be evaluated for other methods on the returned `Self` to complete successfully. Unless
    /// a fatal error occurs the future will never yield a result and will execute infinitely.
    pub async fn new(
        device: DeviceProxy,
    ) -> Result<(Self, impl Future<Output = Result<Infallible, anyhow::Error>>), anyhow::Error>
    {
        let (callbacks_client, callbacks_server) = endpoints::create_endpoints::<CallbacksMarker>();
        let server_stream = callbacks_server.into_stream()?;

        device
            .start(callbacks_client)
            .map(|x| map_driver_result(x))
            .err_into::<anyhow::Error>()
            .await
            .context("Failed to start device")?;

        let service = State {
            device,
            events: HashMap::new(),
            used_ports: port::Tracker::new(),
            listens: HashMap::new(),
            tasks: fasync::TaskGroup::new(),
        };
        let service = Vsock { inner: Rc::new(RefCell::new(service)) };
        let callback_loop = service.clone().run_callbacks(server_stream);
        Ok((service, callback_loop))
    }
    async fn run_callbacks(
        self,
        mut callbacks: CallbacksRequestStream,
    ) -> Result<Infallible, anyhow::Error> {
        while let Some(Ok(cb)) = callbacks.next().await {
            self.borrow_mut().do_callback(cb);
        }
        // The only way to get here is if our callbacks stream ended, since our notifications
        // cannot disconnect as we are holding a reference to them in |service|.
        Err(format_err!("Driver disconnected"))
    }

    // Spawns a new asynchronous task for listening for incoming connections on a port.
    fn start_listener(
        &self,
        acceptor: fidl::endpoints::ClientEnd<fidl_fuchsia_vsock::AcceptorMarker>,
        local_port: u32,
    ) -> Result<(), Error> {
        let acceptor = acceptor.into_proxy().map_err(|x| Error::ClientCommunication(x.into()))?;
        let stream = self.listen_port(local_port)?;
        self.borrow_mut().tasks.local(
            self.clone()
                .run_connection_listener(stream, acceptor)
                .unwrap_or_else(|err| tracing::warn!("Error {} running connection listener", err)),
        );
        Ok(())
    }

    // Handles a single incoming client request.
    async fn handle_request(&self, request: ConnectorRequest) -> Result<(), Error> {
        match request {
            ConnectorRequest::Connect { remote_cid, remote_port, con, responder } => responder
                .send(
                    self.make_connection(remote_cid, remote_port, con)
                        .await
                        .map_err(|e| e.into_status().into_raw()),
                ),
            ConnectorRequest::Listen { local_port, acceptor, responder } => responder.send(
                self.start_listener(acceptor, local_port).map_err(|e| e.into_status().into_raw()),
            ),
        }
        .map_err(|e| Error::ClientCommunication(e.into()))
    }

    /// Evaluates messages on a `ConnectorRequestStream` until completion or error
    ///
    /// Takes ownership of a `RequestStream` that is most likely created from a `ServicesServer`
    /// and processes any incoming requests on it.
    pub async fn run_client_connection(self, request: ConnectorRequestStream) {
        let self_ref = &self;
        let fut = request
            .map_err(|err| Error::ClientCommunication(err.into()))
            // TODO: The parallel limit of 4 is currently invented with no basis and should
            // made something more sensible.
            .try_for_each_concurrent(4, |request| {
                self_ref
                    .handle_request(request)
                    .or_else(|e| future::ready(if e.is_comm_failure() { Err(e) } else { Ok(()) }))
            });
        if let Err(e) = fut.await {
            tracing::info!("Failed to handle request {}", e);
        }
    }
    fn alloc_ephemeral_port(self) -> Option<AllocatedPort> {
        let p = self.borrow_mut().used_ports.allocate();
        p.map(|p| AllocatedPort { port: p, service: self })
    }
    // Creates a `ListenStream` that will retrieve raw incoming connection requests.
    // These requests come from the device via the run_callbacks future.
    fn listen_port(&self, port: u32) -> Result<ListenStream, Error> {
        if port::is_ephemeral(port) {
            tracing::info!("Rejecting request to listen on ephemeral port {}", port);
            return Err(Error::ConnectionRefused);
        }
        match self.borrow_mut().listens.entry(port) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (sender, receiver) = mpsc::unbounded();
                let listen =
                    ListenStream { local_port: port, service: self.clone(), stream: receiver };
                entry.insert(sender);
                Ok(listen)
            }
            _ => {
                tracing::info!("Attempt to listen on already bound port {}", port);
                Err(Error::AlreadyBound)
            }
        }
    }

    // Helper for inserting an event into the events hashmap
    fn register_event(&self, event: Event) -> Result<OneshotEvent, Error> {
        match self.borrow_mut().events.entry(event) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (sender, receiver) = oneshot::channel();
                let event = OneshotEvent {
                    event: Some(entry.key().clone()),
                    service: self.clone(),
                    oneshot: receiver,
                };
                entry.insert(sender);
                Ok(event)
            }
            _ => Err(Error::AlreadyBound),
        }
    }

    // These helpers are wrappers around sending a message to the device, and creating events that
    // will be signaled by the run_callbacks future when it receives a message from the device.
    fn send_request(
        &self,
        addr: &addr::Vsock,
        data: zx::Socket,
    ) -> Result<impl Future<Output = Result<(OneshotEvent, OneshotEvent), Error>> + 'static, Error>
    {
        let shutdown_callback =
            self.register_event(Event { action: EventType::Shutdown, addr: addr.clone() })?;
        let response_callback =
            self.register_event(Event { action: EventType::Response, addr: addr.clone() })?;

        let send_request_fut = self.borrow_mut().device.send_request(&addr, data);

        Ok(async move {
            map_driver_result(send_request_fut.await)?;
            Ok((shutdown_callback, response_callback))
        })
    }
    fn send_response(
        &self,
        addr: &addr::Vsock,
        data: zx::Socket,
    ) -> Result<impl Future<Output = Result<OneshotEvent, Error>> + 'static, Error> {
        let shutdown_callback =
            self.register_event(Event { action: EventType::Shutdown, addr: addr.clone() })?;

        let send_request_fut = self.borrow_mut().device.send_response(&addr.clone(), data);

        Ok(async move {
            map_driver_result(send_request_fut.await)?;
            Ok(shutdown_callback)
        })
    }

    // Runs a connected socket until completion. Processes any VMO sends and shutdown events.
    async fn run_connection<ShutdownFut>(
        self,
        addr: addr::Vsock,
        shutdown_event: ShutdownFut,
        mut requests: ConnectionRequestStream,
        _port: Option<AllocatedPort>,
    ) -> Result<(), Error>
    where
        ShutdownFut:
            Future<Output = Result<(), futures::channel::oneshot::Canceled>> + std::marker::Unpin,
    {
        let mut shutdown_event = shutdown_event.fuse();
        select! {
            shutdown_event = shutdown_event => {
                let fut = future::ready(shutdown_event)
                    .err_into()
                    .and_then(|()| self.borrow_mut().send_rst(&addr));
                return fut.await;
            },
            request = requests.next() => {
                match request {
                    Some(Ok(ConnectionRequest::Shutdown{control_handle: _control_handle})) => {
                        let fut =
                            self.borrow_mut().send_shutdown(&addr)
                                // Wait to either receive the RST for the client or to be
                                // shut down for some other reason
                                .and_then(|()| shutdown_event.err_into());
                        return fut.await;
                    },
                    // Generate a RST for a non graceful client disconnect.
                    Some(Err(e)) => {
                        let fut = self.borrow_mut().send_rst(&addr);
                        fut.await?;
                        return Err(Error::ClientCommunication(e.into()));
                    },
                    None => {
                        let fut = self.borrow_mut().send_rst(&addr);
                        return fut.await;
                    },
                }
            }
        }
    }

    // Waits for incoming connections on the given `ListenStream`, checks with the
    // user via the `acceptor` if it should be accepted, and if so spawns a new
    // asynchronous task to run the connection.
    async fn run_connection_listener(
        self,
        incoming: ListenStream,
        acceptor: AcceptorProxy,
    ) -> Result<(), Error> {
        incoming
            .then(|addr| acceptor.accept(&*addr.clone()).map_ok(|maybe_con| (maybe_con, addr)))
            .map_err(|e| Error::ClientCommunication(e.into()))
            .try_for_each(|(maybe_con, addr)| async {
                match maybe_con {
                    Some(con) => {
                        let data = con.data;
                        let con = con
                            .con
                            .into_stream()
                            .map_err(|x| Error::ClientCommunication(x.into()))?;
                        let shutdown_event = self.send_response(&addr, data)?.await?;
                        self.borrow_mut().tasks.local(
                            self.clone()
                                .run_connection(addr, shutdown_event, con, None)
                                .map_err(|err| {
                                    tracing::warn!("Error {} whilst running connection", err)
                                })
                                .map(|_| ()),
                        );
                        Ok(())
                    }
                    None => {
                        let fut = self.borrow_mut().send_rst(&addr);
                        fut.await
                    }
                }
            })
            .await
    }

    // Attempts to connect to the given remote cid/port. If successful spawns a new
    // asynchronous task to run the connection until completion.
    async fn make_connection(
        &self,
        remote_cid: u32,
        remote_port: u32,
        con: ConnectionTransport,
    ) -> Result<u32, Error> {
        let data = con.data;
        let con = con.con.into_stream().map_err(|x| Error::ClientCommunication(x.into()))?;
        let port = self.clone().alloc_ephemeral_port().ok_or(Error::OutOfPorts)?;
        let port_value = *port;
        let addr = addr::Vsock::new(port_value, remote_port, remote_cid);
        let (shutdown_event, response_event) = self.send_request(&addr, data)?.await?;
        let mut shutdown_event = shutdown_event.fuse();
        select! {
            _shutdown_event = shutdown_event => {
                // Getting a RST here just indicates a rejection and
                // not any underlying issues.
                return Err(Error::ConnectionRefused);
            },
            response_event = response_event.fuse() => response_event?,
        }

        self.borrow_mut().tasks.local(
            self.clone()
                .run_connection(addr, shutdown_event, con, Some(port))
                .unwrap_or_else(|err| tracing::warn!("Error {} whilst running connection", err)),
        );
        Ok(port_value)
    }

    /// Mutably borrow the wrapped value.
    fn borrow_mut(&self) -> RefMut<'_, State> {
        self.inner.borrow_mut()
    }

    // Deregisters the specified event.
    fn deregister(&self, event: Deregister) {
        self.borrow_mut().deregister(event);
    }
}

impl State {
    // Remove the `event` from the `events` `HashMap`
    fn deregister(&mut self, event: Deregister) {
        match event {
            Deregister::Event(e) => {
                self.events.remove(&e);
            }
            Deregister::Listen(p) => {
                self.listens.remove(&p);
            }
            Deregister::Port(p) => {
                self.used_ports.free(p);
            }
        }
    }

    // Wrappers around device functions with nicer type signatures
    fn send_rst(
        &mut self,
        addr: &addr::Vsock,
    ) -> impl Future<Output = Result<(), Error>> + 'static {
        self.device.send_rst(&addr.clone()).map(|x| map_driver_result(x))
    }
    fn send_shutdown(
        &mut self,
        addr: &addr::Vsock,
    ) -> impl Future<Output = Result<(), Error>> + 'static {
        self.device.send_shutdown(&addr).map(|x| map_driver_result(x))
    }

    // Processes a single callback from the `device`. This is intended to be used by
    // `Vsock::run_callbacks`
    fn do_callback(&mut self, callback: CallbacksRequest) {
        match callback {
            CallbacksRequest::Response { addr, control_handle: _control_handle } => {
                self.events
                    .remove(&Event { action: EventType::Response, addr: addr::Vsock::from(addr) })
                    .map(|channel| channel.send(()));
            }
            CallbacksRequest::Rst { addr, control_handle: _control_handle } => {
                self.events
                    .remove(&Event { action: EventType::Shutdown, addr: addr::Vsock::from(addr) });
            }
            CallbacksRequest::Request { addr, control_handle: _control_handle } => {
                let addr = addr::Vsock::from(addr);
                match self.listens.get(&addr.local_port) {
                    Some(sender) => {
                        let _ = sender.unbounded_send(addr.clone());
                    }
                    None => {
                        tracing::warn!("Request on port {} with no listener", addr.local_port);
                        let task = self.send_rst(&addr).map(|_| ());
                        self.tasks.local(task);
                    }
                }
            }
            CallbacksRequest::Shutdown { addr, control_handle: _control_handle } => {
                self.events
                    .remove(&Event { action: EventType::Shutdown, addr: addr::Vsock::from(addr) })
                    .map(|channel| channel.send(()));
            }
            CallbacksRequest::TransportReset { new_cid: _new_cid, responder } => {
                self.events.clear();
                let _ = responder.send();
            }
        }
    }
}

struct AllocatedPort {
    service: Vsock,
    port: u32,
}

impl Deref for AllocatedPort {
    type Target = u32;

    fn deref(&self) -> &u32 {
        &self.port
    }
}

impl Drop for AllocatedPort {
    fn drop(&mut self) {
        self.service.deregister(Deregister::Port(self.port));
    }
}

struct OneshotEvent {
    event: Option<Event>,
    service: Vsock,
    oneshot: oneshot::Receiver<()>,
}

impl Drop for OneshotEvent {
    fn drop(&mut self) {
        self.event.take().map(|e| self.service.deregister(Deregister::Event(e)));
    }
}

impl Future for OneshotEvent {
    type Output = <oneshot::Receiver<()> as Future>::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.oneshot.poll_unpin(cx) {
            Poll::Ready(x) => {
                // Take the event so that we don't try to deregister it later,
                // as by having sent the message we just received the callbacks
                // task will already have removed it
                self.event.take();
                Poll::Ready(x)
            }
            p => p,
        }
    }
}

struct ListenStream {
    local_port: u32,
    service: Vsock,
    stream: mpsc::UnboundedReceiver<addr::Vsock>,
}

impl Drop for ListenStream {
    fn drop(&mut self) {
        self.service.deregister(Deregister::Listen(self.local_port));
    }
}

impl Stream for ListenStream {
    type Item = addr::Vsock;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.stream.poll_next_unpin(cx)
    }
}
