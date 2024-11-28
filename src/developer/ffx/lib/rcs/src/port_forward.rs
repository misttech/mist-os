// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module provides port forwarding over RCS. It interacts directly with
//! fuchsia.posix.socket over RCS's OpenCapability functionality to keep RCS's
//! API surface small.

use anyhow::{anyhow, Result};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_net_ext::SocketAddress as SocketAddressExt;
use fidl_fuchsia_sys2::OpenDirType;
use futures::Stream;
use std::net::SocketAddr;
use std::ops::Deref;
use std::time::{Duration, Instant};
use {fidl_fuchsia_posix_socket as fsock, fuchsia_async as fasync};

/// Connect to `fuchsia.posix.socket.Provider` from the perspective of RCS.
async fn socket_provider(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<fsock::ProviderProxy> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<fsock::ProviderMarker>();
    let start_time = Instant::now();
    let res = crate::open_with_timeout_at(
        timeout,
        crate::toolbox::MONIKER,
        OpenDirType::NamespaceDir,
        &format!("svc/{}", fsock::ProviderMarker::PROTOCOL_NAME),
        rcs_proxy,
        server_end.into_channel(),
    )
    .await;
    let proxy = if res.is_ok() {
        proxy
    } else {
        // Fallback to the legacy remote control moniker if toolbox doesn't contain the capability.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fsock::ProviderMarker>();
        let timeout = timeout.saturating_sub(Instant::now() - start_time);
        crate::open_with_timeout_at(
            timeout,
            crate::REMOTE_CONTROL_MONIKER,
            OpenDirType::NamespaceDir,
            &format!("svc/{}", fsock::ProviderMarker::PROTOCOL_NAME),
            rcs_proxy,
            server_end.into_channel(),
        )
        .await?;
        proxy
    };

    Ok(proxy)
}

/// Container for a zx::Socket that came from the port forwarding API. This is
/// an RAII wrapper that handles some cleanup we need to do when this socket
/// goes away.
pub struct ForwardedSocket {
    socket: fidl::Socket,
    fidl: fsock::StreamSocketProxy,
}

impl ForwardedSocket {
    /// Get the raw socket, as well as a keep alive token that must not be
    /// dropped while the socket is in use.
    pub fn split(self) -> (fidl::Socket, SocketKeepAliveToken) {
        (self.socket, SocketKeepAliveToken { _socket: self.fidl })
    }
}

impl Deref for ForwardedSocket {
    type Target = fidl::Socket;

    fn deref(&self) -> &fidl::Socket {
        &self.socket
    }
}

/// Container for a StreamSocketProxy that lets the user keep it around, thus
/// keeping the represented socket alive, but doesn't make its internals usable,
/// thus defending the API from unintended customers.
#[derive(Debug)]
pub struct SocketKeepAliveToken {
    _socket: fsock::StreamSocketProxy,
}

/// Set up a port forward from the target. Returns the connected socket, and the
/// FIDL from the socket provider that controls it. Dropping the latter will
/// cause the socket to close.
pub async fn forward_port(
    rcs_proxy: &RemoteControlProxy,
    target_addr: SocketAddr,
    rcs_timeout: Duration,
) -> Result<ForwardedSocket> {
    let socket_provider = socket_provider(&rcs_proxy, rcs_timeout).await?;
    let domain = match &target_addr {
        SocketAddr::V4(_) => fsock::Domain::Ipv4,
        SocketAddr::V6(_) => fsock::Domain::Ipv6,
    };

    let socket_fidl = socket_provider
        .stream_socket(domain, fsock::StreamSocketProtocol::Tcp)
        .await?
        .map_err(|e| anyhow!("Error creating stream socket: {e:?}"))?;
    let socket_fidl = socket_fidl.into_proxy();
    socket_fidl
        .connect(&SocketAddressExt(target_addr).into())
        .await?
        .map_err(|e| anyhow!("Error connecting socket: {e:?}"))?;

    let socket = socket_fidl
        .describe()
        .await
        .map_err(|e| anyhow!("Socket didn't respond to describe: {e:?}"))?;

    let socket = socket.socket.ok_or_else(|| anyhow!("Socket not provided from describe"))?;

    Ok(ForwardedSocket { socket, fidl: socket_fidl })
}

/// Set up a reverse forward from the target. Returns a stream of connecting sockets.
pub async fn reverse_port(
    rcs_proxy: &RemoteControlProxy,
    listen_addr: SocketAddr,
    rcs_timeout: Duration,
    conn_backlog: u16,
) -> Result<impl Stream<Item = (SocketAddr, ForwardedSocket)>> {
    let socket_provider = socket_provider(&rcs_proxy, rcs_timeout).await?;
    let domain = match &listen_addr {
        SocketAddr::V4(_) => fsock::Domain::Ipv4,
        SocketAddr::V6(_) => fsock::Domain::Ipv6,
    };

    let listen_socket = socket_provider
        .stream_socket(domain, fsock::StreamSocketProtocol::Tcp)
        .await?
        .map_err(|e| anyhow!("Error creating stream socket: {e:?}"))?;
    let listen_socket = listen_socket.into_proxy();
    listen_socket
        .bind(&SocketAddressExt(listen_addr).into())
        .await?
        .map_err(|e| anyhow!("Error binding stream socket: {e:?}"))?;

    listen_socket
        .listen(conn_backlog.try_into().unwrap_or(i16::MAX))
        .await?
        .map_err(|e| anyhow!("Error listening on stream socket: {e:?}"))?;

    let listen_socket_fidl_socket = listen_socket
        .describe()
        .await
        .map_err(|e| anyhow!("Could not call describe for listen socket: {e:?}"))?;
    let listen_socket_fidl_socket = listen_socket_fidl_socket
        .socket
        .ok_or_else(|| anyhow!("Listen socket not provided from describe"))?;

    Ok(futures::stream::unfold(
        (listen_socket, listen_socket_fidl_socket),
        move |(listen_socket, listen_socket_fidl_socket)| async move {
            loop {
                match fasync::OnSignalsRef::new(
                    &listen_socket_fidl_socket,
                    fidl::Signals::from_bits(fsock::SIGNAL_STREAM_INCOMING).unwrap()
                        | fidl::Signals::OBJECT_PEER_CLOSED,
                )
                .await
                {
                    Err(e) => {
                        tracing::warn!("Listen socket failed: {e:?}");
                        return None;
                    }
                    Ok(s) if s.contains(fidl::Signals::OBJECT_PEER_CLOSED) => {
                        tracing::debug!("Socket hung up");
                        return None;
                    }
                    Ok(_) => (),
                }
                let got_socket = listen_socket.accept(true).await;

                match got_socket {
                    Ok(Ok((addr, got_socket))) => {
                        let Some(addr) = addr else {
                            tracing::warn!("Reverse forward accepted socket with no address from");
                            continue;
                        };

                        let socket_fidl = got_socket.into_proxy();

                        let socket = match socket_fidl.describe().await {
                            Ok(socket) => socket,
                            Err(error) => {
                                tracing::warn!(
                                "Reverse forward incoming socket didn't respond to describe: {error:?}");
                                continue;
                            }
                        };

                        let Some(socket) = socket.socket else {
                            tracing::warn!("Reverse forward socket not provided in describe");
                            continue;
                        };

                        return Some((
                            (
                                SocketAddressExt::from(*addr).0,
                                ForwardedSocket { socket, fidl: socket_fidl },
                            ),
                            (listen_socket, listen_socket_fidl_socket),
                        ));
                    }

                    Ok(Err(error)) => {
                        tracing::warn!("Reverse forward error accepting connection: {error:?}");
                        return None;
                    }
                    Err(error) => {
                        tracing::warn!("Reverse forward listening socket failed: {error:?}");
                        return None;
                    }
                }
            }
        },
    ))
}
