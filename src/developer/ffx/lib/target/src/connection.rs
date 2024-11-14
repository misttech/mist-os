// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl_pipe::FidlPipe;
use crate::target_connector::TargetConnector;
use anyhow::Result;
use async_lock::Mutex;
use compat_info::CompatibilityInfo;
use fdomain_client::fidl::DiscoverableProtocolMarker;
use fdomain_fuchsia_developer_remotecontrol::{
    RemoteControlMarker as FDRemoteControlMarker, RemoteControlProxy as FDRemoteControlProxy,
};
use ffx_ssh::parse::HostAddr;
use fidl::prelude::*;
use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};
use std::net::SocketAddr;
use std::sync::Arc;
use {fdomain_fuchsia_io as fio, fidl_fuchsia_io as fio_f};

#[derive(Debug)]
struct RcsInfo {
    node_id: overnet_core::NodeId,
    // Need access to the main RCS to prevent the underlying overnet loop from closing on itself.
    _rcs_proxy: RemoteControlProxy,
}

/// Represents a direct (no daemon) connection to a Fuchsia target.
#[derive(Debug)]
pub struct Connection {
    overnet: Option<OvernetClient>,
    fdomain: Mutex<Option<Arc<fdomain_client::Client>>>,
    fidl_pipe: FidlPipe,
    rcs_info: Mutex<Option<RcsInfo>>,
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
    #[error("Overnet isn't supported for this target")]
    OvernetUnsupported,
}

impl Connection {
    /// Attempts to create a direct connection to a Fuchsia device using the passed connector. This
    /// constructor will not attempt to wait for a timeout, so it is best to use one against this
    /// function if you don't intend to wait an inordinate amount of time for the connection to
    /// complete.
    ///
    /// When the `Connection` object is returned, this means that a successful direct connection to
    /// a Fuchsia device has occurred.
    ///
    /// # Errors
    ///
    /// This function will only ever return a `ConnectionStartError` or `InternalError`, both of
    /// which are considered fatal (e.g. there is no means to reattempt connecting to the device).
    #[tracing::instrument(level = "debug")]
    pub async fn new(connector: impl TargetConnector + 'static) -> Result<Self, ConnectionError> {
        let connector_debug_string = format!("{connector:?}");
        let (fidl_pipe, node, client) = FidlPipe::start_internal(connector).await.map_err(|e| {
            ConnectionError::ConnectionStartError(connector_debug_string, e.to_string())
        })?;
        let overnet = node.map(|node| OvernetClient { node });
        Ok(Self { overnet, fdomain: Mutex::new(client), fidl_pipe, rcs_info: Default::default() })
    }

    /// Attempts to retrieve an instance of the remote control proxy. When invoked for the first
    /// time, this function will run indefinitely until it finds a remote control proxy, so it is
    /// the caller's responsibility to time out.
    pub async fn rcs_proxy(&self) -> Result<RemoteControlProxy, ConnectionError> {
        let mut rcs_info = self.rcs_info.lock().await;
        let overnet = self.overnet.as_ref().ok_or(ConnectionError::OvernetUnsupported)?;
        if rcs_info.is_none() {
            let (proxy, node_id) = overnet.connect_remote_control().await.map_err(|e| {
                ConnectionError::KnockError(
                    self.wrap_connection_errors(e).context("getting RCS proxy"),
                )
            })?;
            *rcs_info = Some(RcsInfo { _rcs_proxy: proxy, node_id });
        }
        let (remote_proxy, remote_server_end) =
            fidl::endpoints::create_proxy::<RemoteControlMarker>()
                .map_err(|e| ConnectionError::InternalError(e.into()))?;
        let node_id = rcs_info.as_ref().unwrap().node_id;
        overnet
            .node
            .connect_to_service(
                node_id,
                RemoteControlMarker::PROTOCOL_NAME,
                remote_server_end.into_channel(),
            )
            .await
            .map_err(|e| {
                ConnectionError::KnockError(
                    self.wrap_connection_errors(e).context("connecting to new RCS proxy"),
                )
            })?;
        Ok(remote_proxy)
    }

    /// Attempts to retrieve an instance of the remote control proxy. When invoked for the first
    /// time, this function will run indefinitely until it finds a remote control proxy, so it is
    /// the caller's responsibility to time out.
    pub async fn rcs_proxy_fdomain(&self) -> Result<FDRemoteControlProxy, ConnectionError> {
        let fdomain = {
            let mut fdomain = self.fdomain.lock().await;
            if let Some(fdomain) = fdomain.clone() {
                fdomain
            } else {
                let client = self.pass_thru_client().await?;
                *fdomain = Some(client);
                fdomain.clone().unwrap()
            }
        };

        let (proxy, server_end) = fdomain
            .create_proxy::<FDRemoteControlMarker>()
            .await
            .map_err(|e| ConnectionError::InternalError(e.into()))?;
        let ns = fdomain.namespace().await.map_err(|e| ConnectionError::InternalError(e.into()))?;
        let ns = fio::DirectoryProxy::new(ns);
        ns.open3(
            FDRemoteControlMarker::PROTOCOL_NAME,
            fio::Flags::PROTOCOL_SERVICE,
            &fio::Options::default(),
            server_end.into_channel(),
        )
        .map_err(|e| ConnectionError::InternalError(e.into()))?;

        Ok(proxy)
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

    pub fn compatibility_info(&self) -> Option<CompatibilityInfo> {
        self.fidl_pipe.compatibility_info()
    }

    /// The device to which we are connected.
    pub fn device_address(&self) -> Option<SocketAddr> {
        self.fidl_pipe.device_address()
    }

    /// The ssh host address from the perspective of the device.
    pub fn host_ssh_address(&self) -> Option<HostAddr> {
        self.fidl_pipe.host_ssh_address()
    }

    /// Get an FDomain client that just forwards to Overnet.
    async fn pass_thru_client(&self) -> Result<Arc<fdomain_client::Client>, ConnectionError> {
        let rcs = self.rcs_proxy().await?;
        let toolbox =
            rcs::toolbox::open_toolbox(&rcs).await.map_err(ConnectionError::InternalError)?;

        Ok(fdomain_local::local_client(move || {
            let (client, server) = fidl::endpoints::create_endpoints();
            if let Err(error) = toolbox.open3(
                ".",
                fio_f::Flags::PROTOCOL_DIRECTORY,
                &fio_f::Options::default(),
                server.into(),
            ) {
                tracing::debug!(?error, "Could not open svc folder in toolbox namespace");
            };
            Ok(client)
        }))
    }
}

#[derive(Debug)]
struct OvernetClient {
    node: Arc<overnet_core::Router>,
}

impl OvernetClient {
    /// Attempts to locate a node exposing `RemoteControlMarker::PROTOCOL_NAME` as a service. Will
    /// wait indefinitely until this is found.
    async fn locate_remote_control_node(&self) -> Result<overnet_core::NodeId> {
        let lpc = self.node.new_list_peers_context().await;
        loop {
            let new_peers = lpc.list_peers().await?;
            if let Some(id) = new_peers
                .iter()
                .find(|p| p.services.contains(&RemoteControlMarker::PROTOCOL_NAME.to_string()))
                .map(|p| p.node_id)
            {
                return Ok(id);
            }
        }
    }

    /// This is the remote control proxy that should be used for everything.
    ///
    /// If this is dropped, it will close the FidlPipe connection.
    ///
    /// This function will not return if the remote control marker cannot be found on overnet, so
    /// the caller should ensure a proper timeout is handled for potentially indefinite waiting.
    pub(crate) async fn connect_remote_control(
        &self,
    ) -> Result<(RemoteControlProxy, overnet_core::NodeId)> {
        let (server, client) = fidl::Channel::create();
        let node_id = self.locate_remote_control_node().await?;
        let _ = self
            .node
            .connect_to_service(node_id, RemoteControlMarker::PROTOCOL_NAME, server)
            .await?;
        let proxy = RemoteControlProxy::new(fidl::AsyncChannel::from_channel(client));
        Ok((proxy, node_id))
    }
}

pub mod testing {
    use super::*;
    use crate::target_connector::{OvernetConnection, TargetConnection, TargetConnectionError};
    use async_channel::Receiver;
    use fidl_fuchsia_developer_remotecontrol as rcs_fidl;
    use fuchsia_async::{Task, Timer};
    use futures::{FutureExt, StreamExt, TryStreamExt};
    use std::time::Duration;

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
    pub enum FakeOvernetBehavior {
        CloseRcsImmediately,
        KeepRcsOpen,
        FailNonFatalOnce,
    }

    #[derive(Debug)]
    pub struct FakeOvernet {
        circuit_node: Arc<overnet_core::Router>,
        error_receiver: Receiver<anyhow::Error>,
        behavior: FakeOvernetBehavior,
        already_failed: bool,
    }

    impl FakeOvernet {
        pub fn new(
            circuit_node: Arc<overnet_core::Router>,
            error_receiver: Receiver<anyhow::Error>,
            behavior: FakeOvernetBehavior,
        ) -> Self {
            Self { circuit_node, error_receiver, behavior, already_failed: false }
        }

        async fn handle_transaction(req: rcs_fidl::RemoteControlRequest) {
            match req {
                rcs_fidl::RemoteControlRequest::EchoString { value, responder } => {
                    responder.send(&value).unwrap()
                }
                rcs_fidl::RemoteControlRequest::OpenCapability {
                    capability_name,
                    server_channel,
                    responder,
                    ..
                } => match capability_name.as_str() {
                    rcs_fidl::RemoteControlMarker::PROTOCOL_NAME => {
                        fuchsia_async::Task::local(async {
                            let mut stream = fidl::endpoints::ServerEnd::<
                                rcs_fidl::RemoteControlMarker,
                            >::new(server_channel)
                            .into_stream()
                            .unwrap();
                            while let Ok(Some(request)) = stream.try_next().await {
                                match request {
                                    rcs_fidl::RemoteControlRequest::EchoString {
                                        value,
                                        responder,
                                    } => responder.send(&value).unwrap(),
                                    _ => unreachable!(),
                                }
                            }
                        })
                        .detach();
                        responder.send(Ok(())).unwrap();
                    }
                    c => panic!("Received an unexpected capability name: {c}"),
                },
                _ => panic!("Received an unexpected request: {req:?}"),
            }
        }
    }

    impl TargetConnector for FakeOvernet {
        const CONNECTION_TYPE: &'static str = "fake";
        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            if let FakeOvernetBehavior::FailNonFatalOnce = self.behavior {
                if !self.already_failed {
                    self.already_failed = true;
                    Timer::new(Duration::from_secs(5)).await;
                    return Err(TargetConnectionError::NonFatal(anyhow::anyhow!(
                        "awww, we have to try again (for testing, of course)!"
                    )));
                }
            }
            let circuit_socket = create_overnet_circuit(self.circuit_node.clone());
            let (rcs_sender, rcs_receiver) = async_channel::unbounded();
            let behavior = self.behavior.clone();
            self.circuit_node
                .register_service(
                    rcs_fidl::RemoteControlMarker::PROTOCOL_NAME.to_owned(),
                    move |channel| {
                        match behavior {
                            FakeOvernetBehavior::CloseRcsImmediately => {
                                drop(channel);
                                return Ok(());
                            }
                            _ => {}
                        }
                        let _ = rcs_sender.try_send(channel).unwrap();
                        Ok(())
                    },
                )
                .await
                .unwrap();
            let rcs_task = Task::local(async move {
                while let Ok(channel) = rcs_receiver.recv().await {
                    let mut stream = rcs_fidl::RemoteControlRequestStream::from_channel(
                        fidl::AsyncChannel::from_channel(channel),
                    );
                    Task::local(async move {
                        while let Ok(Some(req)) = stream.try_next().await {
                            Self::handle_transaction(req).await;
                        }
                    })
                    .detach();
                }
            });
            let (circuit_reader, circuit_writer) = tokio::io::split(circuit_socket);
            Ok(TargetConnection::Overnet(OvernetConnection {
                output: Box::new(tokio::io::BufReader::new(circuit_reader)),
                input: Box::new(circuit_writer),
                errors: self.error_receiver.clone(),
                compat: None,
                main_task: Some(rcs_task),
                ssh_host_address: None,
            }))
        }
    }
}

#[cfg(test)]
mod test {
    use super::testing::*;
    use super::*;

    #[fuchsia::test]
    async fn test_overnet_rcs_knock() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet::new(
            circuit_node.clone(),
            error_receiver,
            FakeOvernetBehavior::KeepRcsOpen,
        );
        let conn = Connection::new(circuit).await.expect("making connection");
        assert!(conn.rcs_proxy().await.is_ok());
    }

    #[fuchsia::test]
    async fn test_overnet_rcs_knock_retry() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet::new(
            circuit_node.clone(),
            error_receiver,
            FakeOvernetBehavior::FailNonFatalOnce,
        );
        let conn = Connection::new(circuit).await.expect("making connection");
        assert!(conn.rcs_proxy().await.is_ok());
    }

    #[fuchsia::test]
    async fn test_overnet_rcs_knock_failure_disconnect() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (error_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet::new(
            circuit_node.clone(),
            error_receiver,
            FakeOvernetBehavior::CloseRcsImmediately,
        );
        let conn = Connection::new(circuit).await.expect("making connection");
        error_sender.send(anyhow::anyhow!("kaboom")).await.unwrap();
        let err = conn.rcs_proxy().await.unwrap().echo_string("").await;
        assert!(err.is_err());
        let err_string = conn.wrap_connection_errors(err.unwrap_err().into()).to_string();
        assert!(err_string.contains("kaboom"), "'kaboom' should be in '{}'", err_string);
    }

    #[fuchsia::test]
    async fn test_overnet_rcs_echo_multiple_times() {
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet::new(
            circuit_node.clone(),
            error_receiver,
            FakeOvernetBehavior::KeepRcsOpen,
        );
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
