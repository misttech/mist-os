// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::target_connector::{TargetConnection, TargetConnectionError, TargetConnector};
use crate::ConnectionError;
use anyhow::Result;
use async_channel::Receiver;
use compat_info::CompatibilityInfo;
use ffx_ssh::parse::HostAddr;
use fuchsia_async::Task;
use futures::FutureExt;
use futures_lite::stream::StreamExt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Represents a FIDL-piping connection to a Fuchsia target.
///
/// To connect a FIDL pipe, the user must know the IP address of the Fuchsia target being connected
/// to. This may or may not include a port number.
#[derive(Debug)]
pub struct FidlPipe {
    task: Option<Task<()>>,
    error_queue: Receiver<anyhow::Error>,
    compat: Option<CompatibilityInfo>,
    device_address: Option<SocketAddr>,
    host_ssh_address: Option<HostAddr>,
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
    pub fn compatibility_info(&self) -> Option<CompatibilityInfo> {
        self.compat.clone()
    }

    /// This is the main internal constructor for FIDL pipe. This will take a connector and attempt
    /// to connect to it. If the connector fails, this function will attempt to reconnect as long
    /// as the error is not fatal (using an exponential backoff of `t**(2n)` where `t` is the
    /// initial time and `n` is the number of iterations). Any error returned from this function,
    /// therefore, should be interpreted as a non-recoverable error.
    pub(crate) async fn start_internal<C>(
        mut connector: C,
    ) -> Result<(Self, Arc<overnet_core::Router>)>
    where
        C: TargetConnector + 'static,
    {
        let node = overnet_core::Router::new(None)?;
        let socket = create_overnet_socket(node.clone())
            .map_err(|e| ConnectionError::InternalError(e.into()))?;
        let (overnet_reader, overnet_writer) = tokio::io::split(socket);

        let mut wait_duration = Duration::from_millis(50);
        let target_connection = loop {
            break match connector.connect().await {
                Ok(c) => c,
                Err(e) => match e {
                    TargetConnectionError::Fatal(e) => return Err(e),
                    TargetConnectionError::NonFatal(e) => {
                        let error = format!("non-fatal error connecting to device. Retrying again after {wait_duration:?}: {e:?}");
                        tracing::debug!(error);
                        eprintln!("{}", error);
                        async_io::Timer::after(wait_duration).await;
                        wait_duration *= 2;
                        continue;
                    }
                },
            };
        };
        let overnet_connection = match target_connection {
            TargetConnection::Overnet(o) | TargetConnection::Both(_, o) => o,
            TargetConnection::FDomain(_) => panic!("FDomain-only pipe not yet supported"),
        };
        let device_address = connector.device_address();
        let (error_sender, error_queue) = async_channel::unbounded();
        let compat = overnet_connection.compat.clone();
        let host_ssh_address = overnet_connection.ssh_host_address.clone();
        let main_task = async move {
            overnet_connection.run(overnet_writer, overnet_reader, error_sender).await;
            // Explicit drop to force the struct into the closure.
            drop(connector);
        };
        Ok((
            Self {
                task: Some(Task::local(main_task)),
                error_queue,
                compat,
                device_address,
                host_ssh_address,
            },
            node,
        ))
    }

    pub fn error_stream(&self) -> Receiver<anyhow::Error> {
        return self.error_queue.clone();
    }

    pub fn try_drain_errors(&self) -> Option<Vec<anyhow::Error>> {
        let mut pipe_errors = Vec::new();
        while let Ok(err) = self.error_queue.try_recv() {
            pipe_errors.push(err)
        }
        if pipe_errors.is_empty() {
            return None;
        }
        Some(pipe_errors)
    }

    pub fn device_address(&self) -> Option<SocketAddr> {
        self.device_address.clone()
    }

    pub fn host_ssh_address(&self) -> Option<HostAddr> {
        self.host_ssh_address.clone()
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

    use crate::target_connector::{OvernetConnection, TargetConnection};
    use std::fmt::Debug;
    use tokio::io::BufReader;

    #[derive(Debug)]
    struct FailOnceThenSucceedConnector {
        should_succeed: bool,
    }

    impl TargetConnector for FailOnceThenSucceedConnector {
        const CONNECTION_TYPE: &'static str = "fake";

        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            if !self.should_succeed {
                self.should_succeed = true;
                return Err(TargetConnectionError::NonFatal(anyhow::anyhow!("test error")));
            }
            let (sock1, sock2) = fidl::Socket::create_stream();
            let sock1 = fidl::AsyncSocket::from_socket(sock1);
            let sock2 = fidl::AsyncSocket::from_socket(sock2);
            let (_error_tx, error_rx) = async_channel::unbounded();
            let error_task = Task::local(async move {});
            Ok(TargetConnection::Overnet(OvernetConnection {
                output: Box::new(BufReader::new(sock1)),
                input: Box::new(sock2),
                errors: error_rx,
                compat: None,
                main_task: Some(error_task),
                ssh_host_address: None,
            }))
        }
    }

    #[derive(Debug)]
    struct AutoFailConnector;

    impl TargetConnector for AutoFailConnector {
        const CONNECTION_TYPE: &'static str = "fake";
        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            let (sock1, sock2) = fidl::Socket::create_stream();
            let sock1 = fidl::AsyncSocket::from_socket(sock1);
            let sock2 = fidl::AsyncSocket::from_socket(sock2);
            let (error_tx, error_rx) = async_channel::unbounded();
            let error_task = Task::local(async move {
                let _ = error_tx.send(anyhow::anyhow!("boom")).await;
            });
            Ok(TargetConnection::Overnet(OvernetConnection {
                output: Box::new(BufReader::new(sock1)),
                input: Box::new(sock2),
                errors: error_rx,
                compat: None,
                main_task: Some(error_task),
                ssh_host_address: None,
            }))
        }
    }

    #[derive(Debug)]
    struct DoNothingConnector;

    impl TargetConnector for DoNothingConnector {
        const CONNECTION_TYPE: &'static str = "fake";
        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            let (sock1, sock2) = fidl::Socket::create_stream();
            let sock1 = fidl::AsyncSocket::from_socket(sock1);
            let sock2 = fidl::AsyncSocket::from_socket(sock2);
            let (_error_tx, error_rx) = async_channel::unbounded();
            let error_task = Task::local(async move {});
            Ok(TargetConnection::Overnet(OvernetConnection {
                output: Box::new(BufReader::new(sock1)),
                input: Box::new(sock2),
                errors: error_rx,
                compat: None,
                main_task: Some(error_task),
                ssh_host_address: None,
            }))
        }
    }

    #[fuchsia::test]
    async fn test_error_queue() {
        // These sockets will do nothing of import.
        let (fidl_pipe, _node) = FidlPipe::start_internal(AutoFailConnector).await.unwrap();
        let mut errors = fidl_pipe.error_stream();
        let err = errors.next().await.unwrap();
        assert_eq!(anyhow::anyhow!("boom").to_string(), err.to_string());
    }

    #[fuchsia::test]
    async fn test_retry() {
        let (fidl_pipe, _node) =
            FidlPipe::start_internal(FailOnceThenSucceedConnector { should_succeed: false })
                .await
                .unwrap();
        let errs = fidl_pipe.try_drain_errors();
        assert!(errs.is_none());
    }

    #[fuchsia::test]
    async fn test_empty_error_queue() {
        let (fidl_pipe, _node) = FidlPipe::start_internal(DoNothingConnector).await.unwrap();
        // So, this DoNothingConnector is going to ensure no errors are placed onto the queue,
        // however, even if AutoFailConnector was used here it still wouldn't work, since there is
        // no polling happening between the creation of FidlPipe and the attempt to drain errors
        // off of the queue.
        let errs = fidl_pipe.try_drain_errors();
        assert!(errs.is_none());
    }
}
