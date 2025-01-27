// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_lock::RwLock;
use fuchsia_async as fasync;
use fuchsia_repo::server::ConnectionStream;
use futures::channel::mpsc::UnboundedSender;
use futures::{Stream, StreamExt as _};
use protocols::prelude::*;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const TUNNEL_CONNECT_ATTEMPTS: usize = 5;
const TUNNEL_CONNECT_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const TUNNEL_LISTEN_BACKLOG: u16 = 5;
const RCS_SOCKET_PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

/// Manage all the repository tunnels.
#[derive(Debug)]
pub(crate) struct TunnelManager {
    tunnel_addr: SocketAddr,
    server_sink: UnboundedSender<Result<ConnectionStream>>,
    tunnels: Arc<RwLock<HashMap<String, fasync::Task<()>>>>,
}

impl TunnelManager {
    /// Create a new [TunnelManager].
    pub(crate) fn new(
        tunnel_addr: SocketAddr,
        server_sink: UnboundedSender<Result<ConnectionStream>>,
    ) -> Self {
        Self { tunnel_addr, server_sink, tunnels: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// Spawn a repository tunnel to `target_nodename`.
    #[tracing::instrument(skip(self, cx))]
    pub(crate) async fn start_tunnel(&self, cx: &Context, target_nodename: String) -> Result<()> {
        // Exit early if we already have a tunnel set up for this source.
        {
            let tunnels = self.tunnels.read().await;
            if tunnels.get(&target_nodename).is_some() {
                return Ok(());
            }
        }

        tracing::info!("creating repository tunnel for target {:?}", target_nodename);

        let tunnel_stream = create_tunnel_stream(cx, &target_nodename, self.tunnel_addr).await?;

        tracing::info!("created repository tunnel for target {:?}", target_nodename);

        let target_nodename_task = target_nodename.clone();
        let tunnels_task = Arc::clone(&self.tunnels);
        let server_sink_task = self.server_sink.clone();

        let tunnel_fut = async move {
            run_tunnel_protocol(&target_nodename_task, tunnel_stream, server_sink_task).await;

            // Remove the tunnel once the protocol has stopped.
            tunnels_task.write().await.remove(&target_nodename_task);
        };

        // Spawn the tunnel.
        {
            let mut tunnels = self.tunnels.write().await;

            // Check if some other task managed to spawn a tunnel before us.
            if tunnels.get(&target_nodename).is_some() {
                return Ok(());
            }

            // Otherwise, spawn the tunnel.
            let task = fasync::Task::local(tunnel_fut);
            tunnels.insert(target_nodename, task);
        }

        Ok(())
    }
}

#[tracing::instrument(skip(cx))]
async fn create_tunnel_stream(
    cx: &Context,
    target_nodename: &str,
    tunnel_addr: SocketAddr,
) -> Result<impl Stream<Item = (SocketAddr, rcs::port_forward::ForwardedSocket)>> {
    let rc = cx.open_remote_control(Some(target_nodename.to_string())).await?;

    for attempt in 0..TUNNEL_CONNECT_ATTEMPTS {
        tracing::debug!(
            "attempt {} to create repository tunnel for target {:?}",
            attempt + 1,
            target_nodename
        );

        let result = rcs::port_forward::reverse_port(
            &rc,
            tunnel_addr,
            RCS_SOCKET_PROVIDER_TIMEOUT,
            TUNNEL_LISTEN_BACKLOG,
        )
        .await;

        match result {
            Ok(result) => return Ok(result),
            Err(e) => {
                tracing::warn!(
                    "failed to bind repository tunnel port on target {:?} ({e:?})",
                    target_nodename
                );

                // Another process is using the port. Sleep and retry.
                fasync::Timer::new(TUNNEL_CONNECT_RETRY_TIMEOUT).await;
            }
        }
    }

    Err(anyhow::anyhow!("failed to bind to tunnel port on target {:?}", target_nodename))
}

#[tracing::instrument(skip(tunnel_stream, server_sink))]
async fn run_tunnel_protocol(
    target_nodename: &str,
    tunnel_stream: impl Stream<Item = (SocketAddr, rcs::port_forward::ForwardedSocket)>,
    server_sink: UnboundedSender<Result<ConnectionStream>>,
) {
    let result = tunnel_stream
        .map(|(addr, socket)| {
            tracing::info!("tunneling connection from target {:?} to {}", target_nodename, addr);
            let (socket, keep_alive) = socket.split();

            let socket = fasync::Socket::from_socket(socket);
            Ok(Ok(ConnectionStream::Socket(socket, keep_alive)))
        })
        .forward(server_sink)
        .await;

    match result {
        Ok(()) => {
            tracing::info!("closed repository tunnel stream from target {:?}", target_nodename);
        }
        Err(err) => {
            tracing::error!("error forwarding tunnel from target {:?}: {}", target_nodename, err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_spawning_tunnel() {
        let tunnel_addr = (Ipv4Addr::LOCALHOST, 8085).into();
        let (server_tx, _server_rx) = futures::channel::mpsc::unbounded();
        let _tunnel_manager = TunnelManager::new(tunnel_addr, server_tx);
    }
}
