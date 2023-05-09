// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A wrapping of Overnet that is particularly useful for overnet integration tests that don't
//! depend on the surrounding environment (and can thus be run like self contained unit tests)

#![cfg(test)]

mod drop;
mod echo;
mod triangle;

use {
    anyhow::Error,
    circuit::multi_stream::multi_stream_node_connection_to_async,
    fidl::endpoints::ClientEnd,
    fidl_fuchsia_overnet::{Peer, ServiceProviderMarker},
    fuchsia_async::Task,
    futures::channel::mpsc::unbounded,
    futures::prelude::*,
    overnet_core::{log_errors, ListPeersContext, NodeId, NodeIdGenerator, Router},
    parking_lot::Mutex,
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

pub use fidl_fuchsia_overnet::ServiceConsumerProxyInterface;
pub use fidl_fuchsia_overnet::ServicePublisherProxyInterface;

///////////////////////////////////////////////////////////////////////////////////////////////////
// Overnet <-> API bindings

#[derive(Debug)]
enum OvernetCommand {
    ListPeers(futures::channel::oneshot::Sender<Vec<Peer>>),
    RegisterService(String, ClientEnd<ServiceProviderMarker>),
    ConnectToService(NodeId, String, fidl::Channel),
    AttachCircuitSocketLink(fidl::Socket, bool),
}

/// Overnet implementation for integration tests
pub struct Overnet {
    tx: Mutex<futures::channel::mpsc::UnboundedSender<OvernetCommand>>,
    node_id: NodeId,
    // Main loop for the Overnet instance - once the object is dropped, the loop can stop.
    _task: Task<()>,
}

impl Overnet {
    /// Create a new instance
    pub fn new(node_id_gen: &mut NodeIdGenerator) -> Result<Arc<Overnet>, Error> {
        Self::from_router(node_id_gen.new_router()?)
    }

    /// Create a new circuit instance
    pub fn new_circuit_router(node_id_gen: &mut NodeIdGenerator) -> Result<Arc<Overnet>, Error> {
        Self::from_router(node_id_gen.new_router_circuit_router()?)
    }

    fn from_router(node: Arc<Router>) -> Result<Arc<Overnet>, Error> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        tracing::info!(node_id = node.node_id().0, "SPAWN OVERNET");
        let tx = Mutex::new(tx);
        Ok(Arc::new(Overnet {
            tx,
            node_id: node.node_id(),
            _task: Task::spawn(log_errors(run_overnet(node, rx), "Main loop failed")),
        }))
    }

    fn send(&self, cmd: OvernetCommand) -> Result<(), fidl::Error> {
        Ok(self.tx.lock().unbounded_send(cmd).map_err(|_| fidl::Error::Invalid)?)
    }

    /// Produce a proxy that acts as a connection to Overnet under the ServiceConsumer role
    pub fn connect_as_service_consumer(
        self: &Arc<Self>,
    ) -> Result<impl ServiceConsumerProxyInterface, Error> {
        Ok(ServiceConsumer(self.clone()))
    }

    /// Produce a proxy that acts as a connection to Overnet under the ServicePublisher role
    pub fn connect_as_service_publisher(
        self: &Arc<Self>,
    ) -> Result<impl ServicePublisherProxyInterface, Error> {
        Ok(ServicePublisher(self.clone()))
    }

    fn attach_circuit_socket_link(
        &self,
        socket: fidl::Socket,
        is_server: bool,
    ) -> Result<(), fidl::Error> {
        self.send(OvernetCommand::AttachCircuitSocketLink(socket, is_server))
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
}

async fn run_overnet_command(
    node: Arc<Router>,
    lpc: Arc<ListPeersContext>,
    cmd: OvernetCommand,
) -> Result<(), Error> {
    match cmd {
        OvernetCommand::ListPeers(sender) => {
            let peers = lpc.list_peers().await?;
            let _ = sender.send(peers);
            Ok(())
        }
        OvernetCommand::RegisterService(service_name, provider) => {
            node.register_service(service_name, provider).await
        }
        OvernetCommand::ConnectToService(node_id, service_name, channel) => {
            node.connect_to_service(node_id, &service_name, channel).await
        }
        OvernetCommand::AttachCircuitSocketLink(socket, is_server) => {
            let (mut rx, mut tx) = fidl::AsyncSocket::from_socket(socket)?.split();
            let (errors_sender, _black_hole) = unbounded();
            multi_stream_node_connection_to_async(
                node.circuit_node(),
                &mut rx,
                &mut tx,
                is_server,
                circuit::Quality::IN_PROCESS,
                errors_sender,
                "test node".to_owned(),
            )
            .await?;
            Ok(())
        }
    }
}

static NEXT_CMD_ID: AtomicU64 = AtomicU64::new(0);

async fn run_overnet(
    node: Arc<Router>,
    rx: futures::channel::mpsc::UnboundedReceiver<OvernetCommand>,
) -> Result<(), Error> {
    let node_id = node.node_id();
    tracing::info!(?node_id, "RUN OVERNET");
    let lpc = Arc::new(node.new_list_peers_context().await);
    // Run application loop
    rx.for_each_concurrent(None, move |cmd| {
        let node = node.clone();
        let lpc = lpc.clone();
        async move {
            let cmd_text = format!("{:?}", cmd);
            let cmd_id = NEXT_CMD_ID.fetch_add(1, Ordering::Relaxed);
            tracing::info!(node_id = node_id.0, cmd = cmd_id, "START: {}", cmd_text);
            if let Err(e) = run_overnet_command(node, lpc, cmd).await {
                tracing::info!(
                    node_id = node_id.0,
                    cmd = cmd_id,
                    "{} with error: {:?}",
                    cmd_text,
                    e
                );
            } else {
                tracing::info!(node_id = node_id.0, cmd = cmd_id, "SUCCEEDED: {}", cmd_text);
            }
        }
    })
    .await;
    tracing::info!(node_id = node_id.0, "DONE OVERNET");
    Ok(())
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// ProxyInterface implementations

struct ServicePublisher(Arc<Overnet>);

impl ServicePublisherProxyInterface for ServicePublisher {
    fn publish_service(
        &self,
        service_name: &str,
        provider: ClientEnd<ServiceProviderMarker>,
    ) -> Result<(), fidl::Error> {
        self.0.send(OvernetCommand::RegisterService(service_name.to_string(), provider))
    }
}

struct ServiceConsumer(Arc<Overnet>);

use futures::{
    channel::oneshot,
    future::{err, Either, MapErr, Ready},
};

fn bad_recv(_: oneshot::Canceled) -> fidl::Error {
    fidl::Error::PollAfterCompletion
}

impl ServiceConsumerProxyInterface for ServiceConsumer {
    type ListPeersResponseFut = Either<
        MapErr<oneshot::Receiver<Vec<Peer>>, fn(oneshot::Canceled) -> fidl::Error>,
        Ready<Result<Vec<Peer>, fidl::Error>>,
    >;

    fn list_peers(&self) -> Self::ListPeersResponseFut {
        let (sender, receiver) = futures::channel::oneshot::channel();
        if let Err(e) = self.0.send(OvernetCommand::ListPeers(sender)) {
            Either::Right(err(e))
        } else {
            // Returning an error from the receiver means that the sender disappeared without
            // sending a response, a condition we explicitly disallow.
            Either::Left(receiver.map_err(bad_recv))
        }
    }

    fn connect_to_service(
        &self,
        node: &fidl_fuchsia_overnet_protocol::NodeId,
        service_name: &str,
        chan: fidl::Channel,
    ) -> Result<(), fidl::Error> {
        self.0.send(OvernetCommand::ConnectToService(
            node.id.into(),
            service_name.to_string(),
            chan,
        ))
    }
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Connect Overnet nodes to each other

/// Connect two test overnet instances with a stream socket.
pub fn connect(a: &Arc<Overnet>, b: &Arc<Overnet>) -> Result<(), Error> {
    tracing::info!(a = a.node_id().0, b = b.node_id().0, "Connect nodes");
    let (sa, sb) = fidl::Socket::create_stream();
    a.attach_circuit_socket_link(sa, true)?;
    b.attach_circuit_socket_link(sb, false)?;
    Ok(())
}
