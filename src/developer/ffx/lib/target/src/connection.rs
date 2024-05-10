// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl_pipe::{create_overnet_socket, FidlPipe};
use crate::overnet_connector::OvernetConnector;
use anyhow::Result;
use async_lock::Mutex;
use compat_info::CompatibilityInfo;
use fidl::prelude::*;
use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};
use std::sync::Arc;

/// Represents a direct (no daemon) connection to a Fuchsia target.
pub struct Connection {
    overnet: OvernetClient,
    fidl_pipe: FidlPipe,
    rcs_proxy: Mutex<Option<RemoteControlProxy>>,
}

#[derive(thiserror::Error, Debug)]
pub enum ConnectionError {
    #[error("starting connection with connector {0}: {1}")]
    ConnectionStartError(String, String),
    #[error("internal error: {0}")]
    InternalError(#[from] anyhow::Error),
    // TODO(b/339266778): change knock errors to non-fidl types.
    #[error("knock error: {0:?}")]
    KnockError(#[source] anyhow::Error),
}

impl Connection {
    pub async fn new(connector: impl OvernetConnector + 'static) -> Result<Self, ConnectionError> {
        let node = overnet_core::Router::new(None)?;
        let socket = create_overnet_socket(node.clone())
            .map_err(|e| ConnectionError::InternalError(e.into()))?;
        let (overnet_reader, overnet_writer) = tokio::io::split(socket);
        let connector_debug_string = format!("{connector:?}");
        let fidl_pipe =
            FidlPipe::start_internal(overnet_reader, overnet_writer, connector).await.map_err(
                |e| ConnectionError::ConnectionStartError(connector_debug_string, e.to_string()),
            )?;
        Ok(Self { overnet: OvernetClient { node }, fidl_pipe, rcs_proxy: Default::default() })
    }

    pub async fn rcs_proxy(&self) -> Result<RemoteControlProxy, ConnectionError> {
        let mut rcs = self.rcs_proxy.lock().await;
        if rcs.is_none() {
            *rcs = Some(
                self.overnet
                    .connect_remote_control()
                    .await
                    .map_err(|e| self.wrap_connection_errors(e).context("getting RCS proxy"))?,
            );
        }
        Ok(rcs.as_ref().unwrap().clone())
    }

    pub async fn knock_rcs(&self) -> Result<Option<CompatibilityInfo>, ConnectionError> {
        let proxy = self.rcs_proxy().await.map_err(|e| ConnectionError::KnockError(e.into()))?;
        match rcs::knock_rcs(&proxy).await.map_err(|e| anyhow::anyhow!("{e:?}")) {
            Ok(()) => Ok(self.fidl_pipe.compatibility_info()),
            Err(e) => Err(ConnectionError::KnockError(self.wrap_connection_errors(e))),
        }
    }

    /// Takes a given connection error and, if there have been underlying connection errors, adds
    /// additional context to the passed error, else leaves the error the same.
    ///
    /// This function is used to overcome some of the shortcomings around FIDL errors, as on the
    /// host they are being used to simulate what is essentially a networked connection, and not an
    /// OS-backed operation (like when using FIDL on a Fuchsia device).
    pub fn wrap_connection_errors(&self, e: anyhow::Error) -> anyhow::Error {
        if let Some(pipe_errors) = self.fidl_pipe.try_drain_errors() {
            return anyhow::anyhow!("{e:?}\n{pipe_errors:?}");
        }
        e
    }
}

struct OvernetClient {
    node: Arc<overnet_core::Router>,
}

impl OvernetClient {
    async fn locate_remote_control_node(&self) -> Result<overnet_core::NodeId> {
        let lpc = self.node.new_list_peers_context().await;
        let node_id;
        'found: loop {
            let new_peers = lpc.list_peers().await?;
            for peer in &new_peers {
                let peer_has_remote_control =
                    peer.services.contains(&RemoteControlMarker::PROTOCOL_NAME.to_string());
                if peer_has_remote_control {
                    node_id = peer.node_id;
                    break 'found;
                }
            }
        }
        Ok(node_id)
    }

    /// This is the remote control proxy that should be used for everything.
    ///
    /// If this is dropped, it will close the FidlPipe connection.
    pub(crate) async fn connect_remote_control(&self) -> Result<RemoteControlProxy> {
        let (server, client) = fidl::Channel::create();
        let node_id = self.locate_remote_control_node().await?;
        let _ = self
            .node
            .connect_to_service(node_id, RemoteControlMarker::PROTOCOL_NAME, server)
            .await?;
        let proxy = RemoteControlProxy::new(fidl::AsyncChannel::from_channel(client));
        Ok(proxy)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::overnet_connector::OvernetConnection;
    use async_channel::Receiver;
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use fuchsia_async::Task;
    use futures::{FutureExt, StreamExt, TryStreamExt};

    fn create_overnet_circuit(router: Arc<overnet_core::Router>) -> fidl::AsyncSocket {
        let (local_socket, remote_socket) = fidl::Socket::create_stream();
        let local_socket = fidl::AsyncSocket::from_socket(local_socket);

        let socket = fidl::AsyncSocket::from_socket(remote_socket);
        let (mut rx, mut tx) = futures::AsyncReadExt::split(socket);
        Task::spawn(async move {
            let (errors_sender, errors) = futures::channel::mpsc::unbounded();
            if let Err(e) = futures::future::join(
                circuit::multi_stream::multi_stream_node_connection_to_async(
                    router.circuit_node(),
                    &mut rx,
                    &mut tx,
                    true,
                    circuit::Quality::NETWORK,
                    errors_sender,
                    "client".to_owned(),
                ),
                errors
                    .map(|e| {
                        eprintln!("A client circuit stream failed: {e:?}");
                    })
                    .collect::<()>(),
            )
            .map(|(result, ())| result)
            .await
            {
                if let circuit::Error::ConnectionClosed(msg) = e {
                    eprintln!("testing overnet link closed: {:?}", msg);
                } else {
                    eprintln!("error handling Overnet link: {:?}", e);
                }
            }
        })
        .detach();

        local_socket
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    enum FakeOvernetBehavior {
        CloseRcsImmediately,
        KeepRcsOpen,
    }

    #[derive(Debug)]
    struct FakeOvernet {
        circuit_node: Arc<overnet_core::Router>,
        error_receiver: Receiver<anyhow::Error>,
        behavior: FakeOvernetBehavior,
    }

    impl FakeOvernet {
        async fn handle_transaction(
            req: rcs::RemoteControlRequest,
            behavior: &FakeOvernetBehavior,
        ) {
            match req {
                rcs::RemoteControlRequest::OpenCapability { server_channel, responder, .. } => {
                    match behavior {
                        FakeOvernetBehavior::KeepRcsOpen => {
                            // We're just going to assume this capability is always going to be
                            // RCS, and avoid string matching for the sake of avoiding changes
                            // to monikers and/or capability connecting.
                            let mut stream = rcs::RemoteControlRequestStream::from_channel(
                                fidl::AsyncChannel::from_channel(server_channel),
                            );
                            // This task is here to ensure the channel stays open, but won't
                            // necessarily need do anything.
                            Task::spawn(async move {
                                while let Ok(Some(req)) = stream.try_next().await {
                                    eprintln!("Got a request: {req:?}")
                                }
                            })
                            .detach();
                        }
                        FakeOvernetBehavior::CloseRcsImmediately => {
                            drop(server_channel);
                        }
                    }
                    responder.send(Ok(())).unwrap();
                }
                rcs::RemoteControlRequest::EchoString { value, responder } => {
                    responder.send(&value).unwrap()
                }
                _ => panic!("Received an unexpected request: {req:?}"),
            }
        }
    }

    impl OvernetConnector for FakeOvernet {
        async fn connect(&mut self) -> Result<OvernetConnection> {
            let circuit_socket = create_overnet_circuit(self.circuit_node.clone());
            let (rcs_sender, rcs_receiver) = async_channel::unbounded();
            self.circuit_node
                .register_service(
                    rcs::RemoteControlMarker::PROTOCOL_NAME.to_owned(),
                    move |channel| {
                        let _ = rcs_sender.try_send(channel).unwrap();
                        Ok(())
                    },
                )
                .await
                .unwrap();
            let behavior = self.behavior.clone();
            let rcs_task = Task::local(async move {
                while let Ok(channel) = rcs_receiver.recv().await {
                    let mut stream = rcs::RemoteControlRequestStream::from_channel(
                        fidl::AsyncChannel::from_channel(channel),
                    );
                    while let Ok(Some(req)) = stream.try_next().await {
                        Self::handle_transaction(req, &behavior).await;
                    }
                }
            });
            let (circuit_reader, circuit_writer) = tokio::io::split(circuit_socket);
            Ok(OvernetConnection {
                output: Box::new(tokio::io::BufReader::new(circuit_reader)),
                input: Box::new(circuit_writer),
                errors: self.error_receiver.clone(),
                compat: None,
                main_task: Some(rcs_task),
            })
        }
    }

    #[fuchsia::test]
    async fn test_overnet_rcs_knock() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet {
            circuit_node: circuit_node.clone(),
            error_receiver,
            behavior: FakeOvernetBehavior::KeepRcsOpen,
        };
        let conn = Connection::new(circuit).await.expect("making connection");
        assert!(conn.knock_rcs().await.is_ok());
    }

    #[fuchsia::test]
    async fn test_overnet_rcs_knock_failure_disconnect() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (error_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet {
            circuit_node: circuit_node.clone(),
            error_receiver,
            behavior: FakeOvernetBehavior::CloseRcsImmediately,
        };
        let conn = Connection::new(circuit).await.expect("making connection");
        error_sender.send(anyhow::anyhow!("kaboom")).await.unwrap();
        let err = conn.knock_rcs().await;
        assert!(err.is_err());
        let err_string = err.unwrap_err().to_string();
        assert!(err_string.contains("kaboom"), "'kaboom' should be in '{}'", err_string);
    }

    #[fuchsia::test]
    async fn test_overnet_rcs_echo_multiple_times() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet {
            circuit_node: circuit_node.clone(),
            error_receiver,
            behavior: FakeOvernetBehavior::KeepRcsOpen,
        };
        let conn = Connection::new(circuit).await.expect("making connection");
        let rcs = conn.rcs_proxy().await.unwrap();
        assert_eq!(rcs.echo_string("foobart").await.unwrap(), "foobart".to_owned());
        let rcs2 = conn.rcs_proxy().await.unwrap();
        assert_eq!(rcs2.echo_string("foobarr").await.unwrap(), "foobarr".to_owned());
        drop(rcs);
        drop(rcs2);
        let rcs3 = conn.rcs_proxy().await.unwrap();
        assert_eq!(rcs3.echo_string("foobarz").await.unwrap(), "foobarz".to_owned());
    }
}
