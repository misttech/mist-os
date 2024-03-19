// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::overnet_connector::OvernetConnector;
use crate::ssh_connector::SshConnector;
use anyhow::Result;
use async_channel::Receiver;
use compat_info::CompatibilityInfo;
use ffx_config::EnvironmentContext;
use fuchsia_async::Task;
use futures::FutureExt;
use futures_lite::stream::StreamExt;
use std::fmt::Debug;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Represents a FIDL-piping connection to a Fuchsia target.
///
/// To connect a FIDL pipe, the user must know the IP address of the Fuchsia target being connected
/// to. This may or may not include a port number.
pub struct FidlPipe {
    address: SocketAddr,
    task: Option<Task<()>>,
    error_queue: Receiver<anyhow::Error>,
    compat: Option<CompatibilityInfo>,
}

#[derive(thiserror::Error, Debug)]
enum FidlPipeError {
    #[error("running target fidl pipe: {0}")]
    SpawnError(String),
    #[error("internal error running pipe cmd {1}: {0}")]
    InternalError(String, String),
}

pub fn create_overnet_socket(
    node: Arc<overnet_core::Router>,
) -> Result<fidl::AsyncSocket, io::Error> {
    let (local_socket, remote_socket) = fidl::Socket::create_stream();
    let local_socket = fidl::AsyncSocket::from_socket(local_socket);
    let (errors_sender, errors) = futures::channel::mpsc::unbounded();
    Task::spawn(
        futures::future::join(
            async move {
                if let Err(e) = async move {
                    let (mut rx, mut tx) = futures::AsyncReadExt::split(
                        fuchsia_async::Socket::from_socket(remote_socket),
                    );
                    circuit::multi_stream::multi_stream_node_connection_to_async(
                        node.circuit_node(),
                        &mut rx,
                        &mut tx,
                        false,
                        circuit::Quality::LOCAL_SOCKET,
                        errors_sender,
                        "remote_control_runner".to_owned(),
                    )
                    .await?;
                    Result::<(), anyhow::Error>::Ok(())
                }
                .await
                {
                    tracing::warn!("FIDL pipe circuit closed: {:?}", e);
                }
            },
            errors
                .map(|e| {
                    tracing::warn!("A FIDL pipe circuit stream failed: {e:?}");
                })
                .collect::<()>(),
        )
        .map(|((), ())| ()),
    )
    .detach();

    Ok(local_socket)
}

impl FidlPipe {
    /// Creates a new FIDL pipe to the Fuchsia target.
    ///
    /// Args:
    ///     env_context: the environment context for which configuration options are selected.
    ///     target: the address to the Fuchsia target.
    ///     overnet_node: The backing overnet node for this connection.
    pub async fn new(
        env_context: EnvironmentContext,
        target: SocketAddr,
        overnet_node: Arc<overnet_core::Router>,
    ) -> Result<Self> {
        let ssh_connector = SshConnector::new(target, &env_context)
            .await
            .map_err(|e| FidlPipeError::SpawnError(e.to_string()))?;
        let socket = create_overnet_socket(overnet_node).map_err(|e| {
            FidlPipeError::InternalError(
                format!("Unable to create overnet_pipe from {e:?}"),
                format!("{:?}", ssh_connector),
            )
        })?;
        let (overnet_reader, overnet_writer) = tokio::io::split(socket);
        Self::start_internal(target, overnet_reader, overnet_writer, ssh_connector).await
    }

    pub fn compatibility_info(&self) -> Option<CompatibilityInfo> {
        self.compat.clone()
    }

    // This constructor is relied upon for testing in target/src/lib.rs
    // For testing RCS knock.
    pub(crate) async fn start_internal<C, W, R>(
        target: SocketAddr,
        overnet_reader: R,
        overnet_writer: W,
        mut connector: C,
    ) -> Result<Self>
    where
        C: OvernetConnector + 'static,
        R: AsyncRead + Unpin + 'static,
        W: AsyncWrite + Unpin + 'static,
    {
        let overnet_connection = connector.connect().await?;
        let (error_sender, error_queue) = async_channel::unbounded();
        let compat = overnet_connection.compat.clone();
        let main_task = async move {
            overnet_connection.run(overnet_writer, overnet_reader, error_sender).await;
            // Explicit drop to force the struct into the closure.
            drop(connector);
        };
        Ok(Self { address: target, task: Some(Task::local(main_task)), error_queue, compat })
    }

    pub fn error_stream(&self) -> Receiver<anyhow::Error> {
        return self.error_queue.clone();
    }

    pub fn target_address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for FidlPipe {
    fn drop(&mut self) {
        self.error_queue.close();
        drop(self.task.take());
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::overnet_connector::OvernetConnection;
    use tokio::io::BufReader;

    #[derive(Debug)]
    struct AutoFailConnector;

    impl OvernetConnector for AutoFailConnector {
        async fn connect(&mut self) -> Result<OvernetConnection> {
            let (sock1, sock2) = fidl::Socket::create_stream();
            let sock1 = fidl::AsyncSocket::from_socket(sock1);
            let sock2 = fidl::AsyncSocket::from_socket(sock2);
            let (error_tx, error_rx) = async_channel::unbounded();
            let error_task = Task::local(async move {
                let _ = error_tx.send(anyhow::anyhow!("boom")).await;
            });
            Ok(OvernetConnection {
                output: Box::new(BufReader::new(sock1)),
                input: Box::new(sock2),
                errors: error_rx,
                compat: None,
                main_task: Some(error_task),
            })
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_error_queue() {
        // These sockets will do nothing of import.
        let (local_socket, _remote_socket) = fidl::Socket::create_stream();
        let local_socket = fidl::AsyncSocket::from_socket(local_socket);
        let (reader, writer) = tokio::io::split(local_socket);
        let fidl_pipe = FidlPipe::start_internal(
            "127.0.0.1:22".parse().unwrap(),
            reader,
            writer,
            AutoFailConnector,
        )
        .await
        .unwrap();
        let mut errors = fidl_pipe.error_stream();
        let err = errors.next().await.unwrap();
        assert_eq!(anyhow::anyhow!("boom").to_string(), err.to_string());
    }
}
