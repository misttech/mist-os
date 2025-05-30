// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt, TryStreamExt};
use netext::{TcpListenerStream, TokioAsyncReadExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;

use crate::{Error, SocketProvider, TargetTcpListener, TargetTcpStream};

#[derive(Clone)]
pub struct PortForwarder {
    socket_provider: SocketProvider,
}

impl PortForwarder {
    /// Creates a new [`PortForwarder`] with target connections via
    /// [`SocketProvider`].
    pub fn new(socket_provider: SocketProvider) -> Self {
        Self { socket_provider }
    }

    /// Creates a new [`PortForwarder`] fetching the FIDL proxies with
    /// `rcs_proxy`.
    pub async fn new_with_rcs(
        connect_timeout: Duration,
        rcs_proxy: &RemoteControlProxy,
    ) -> Result<Self, Error> {
        SocketProvider::new_with_rcs(connect_timeout, rcs_proxy).await.map(Self::new)
    }

    /// Returns the socket provider backing this `PortForwarder`.
    pub fn socket_provider(&self) -> &SocketProvider {
        &self.socket_provider
    }

    /// Reverse forwarding from target to host.
    ///
    /// Returns a future that proxies connections accepted from
    /// `target_listener` to new connections to `host_address`.
    pub fn reverse(
        &self,
        target_listener: TargetTcpListener,
        host_address: SocketAddr,
    ) -> impl Future<Output = Result<(), Error>> + 'static {
        target_listener.into_stream().try_for_each_concurrent(None, move |socket| async move {
            let addr = socket.peer_addr();
            log::info!("Connection from {addr} reverse forwarding to {host_address}");
            let tcp_stream = match tokio::net::TcpStream::connect(&host_address).await {
                Ok(stream) => stream,
                Err(e) => {
                    log::error!("Could not connect to {:?}: {:?}", host_address, e);
                    return Ok(());
                }
            };
            shuttle_bytes(tcp_stream, socket).await;
            log::info!("Reverse-forwarded connection from {addr} closed");
            Result::Ok(())
        })
    }

    /// Direct forwarding from host to target.
    ///
    /// Returns a future that proxies connections accepted from `host_listener`
    /// to new connections to `target_address` on the target.
    pub fn forward(
        &self,
        host_listener: TcpListener,
        target_address: SocketAddr,
    ) -> impl Future<Output = Result<(), Error>> + 'static {
        let socket_provider = self.socket_provider.clone();
        TcpListenerStream(host_listener).map(Ok).try_for_each_concurrent(None, move |conn| {
            let socket_provider = socket_provider.clone();
            async move {
                let conn = conn.and_then(|c| {
                    let addr = c.peer_addr()?;
                    Ok((c, addr))
                });
                let (conn, addr) = match conn {
                    Ok(conn) => conn,
                    Err(e) => {
                        log::error!("Error accepting connection for TCP forwarding: {:?}", e);
                        return Ok(());
                    }
                };
                log::info!("Connection from {addr} forwarding to {target_address}");
                let socket = match socket_provider.connect(target_address).await {
                    Ok(socket) => socket,
                    Err(e) => {
                        log::error!("Error requesting port forward from RCS: {:?}", e);
                        return Ok(());
                    }
                };
                shuttle_bytes(conn, socket).await;
                log::info!("Forwarded connection from {addr} closed");
                Ok(())
            }
        })
    }
}

async fn shuttle_bytes(host: tokio::net::TcpStream, target: TargetTcpStream) {
    let (mut host_read, mut host_write) = host.into_futures_stream().split();
    let (mut target_read, mut target_write) = target.split();

    let host_to_target = async move {
        let mut buf = vec![0; 4096];
        loop {
            let bytes = host_read.read(&mut buf).await?;
            if bytes == 0 {
                break Ok(());
            }
            target_write.write_all(&buf[..bytes]).await?;
            target_write.flush().await?;
        }
    };
    let target_to_host = async move {
        let mut buf = vec![0; 4096];
        loop {
            let bytes = target_read.read(&mut buf).await?;
            if bytes == 0 {
                break Ok(()) as Result<(), std::io::Error>;
            }
            host_write.write_all(&buf[..bytes]).await?;
            host_write.flush().await?;
        }
    };
    match futures::future::join(host_to_target, target_to_host).await {
        (Err(a), Err(b)) => {
            log::warn!("Port forward closed with errors:\n  {:?}\n  {:?}", a, b)
        }
        (Err(e), _) | (_, Err(e)) => {
            log::warn!("Port forward closed with error: {:?}", e)
        }
        (Ok(()), Ok(())) => (),
    }
}
