// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use nix::errno::Errno;
use nix::sys::socket::sockopt::SocketError;
use nix::sys::socket::{connect, getsockopt, socket, AddressFamily, SockFlag, SockType, VsockAddr};
use nix::unistd::{read, write};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use tokio::io::Ready;

const OVERNET_VSOCK_PORT: u32 = 202;
const BUFFER_SIZE: usize = 4096;

/// Host-pipe-like task for communicating via vsock.
pub async fn spawn_vsock(cid: u32, node: Arc<overnet_core::Router>) {
    tracing::debug!(cid, "Spawning VSOCK host pipe");
    let addr = VsockAddr::new(cid, OVERNET_VSOCK_PORT);
    #[cfg(not(target_os = "macos"))]
    let flags = SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK;
    #[cfg(target_os = "macos")]
    let flags = SockFlag::empty();

    let socket = match socket(AddressFamily::Vsock, SockType::Stream, flags, None) {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!(cid, ?error, "Could not create VSOCK socket");
            return;
        }
    };

    #[cfg(target_os = "macos")]
    {
        use nix::fcntl::{fcntl, FcntlArg, OFlag};

        if let Err(error) =
            fcntl(socket.as_raw_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC))
        {
            tracing::warn!(cid, ?error, "Could not set O_CLOEXEC|O_NONBLOCK on VSOCK socket");
            return;
        }
    }

    let socket = match tokio::io::unix::AsyncFd::new(socket) {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!(?error, "Could not create Tokio AsyncFD for VSOCK socket");
            return;
        }
    };

    if let Err(error) = connect(socket.as_raw_fd(), &addr) {
        if error != Errno::EINPROGRESS {
            tracing::warn!(cid, ?error, "Could not connect to VSOCK socket");
            return;
        }
    }

    // If we got EINPROGRESS above then the connection isn't actually
    // established and we need to block until it is.
    if let Err(error) = socket.writable().await {
        tracing::warn!(
            cid,
            ?error,
            "Error while waiting for VSOCK to become ready after connection"
        );
        return;
    } else {
        let error = match getsockopt(&socket, SocketError) {
            Ok(e) => e,
            Err(error) => {
                tracing::warn!(cid, ?error, "Could not verify connection to VSOCK socket");
                return;
            }
        };

        if error != 0 {
            let error = Errno::from_raw(error);
            tracing::warn!(cid, ?error, "Could not complete connection to VSOCK socket");
            return;
        }
    }

    tracing::debug!(cid, "VSOCK connection established");

    let (in_reader, in_writer) = circuit::stream::stream();
    let (out_reader, out_writer) = circuit::stream::stream();
    let (error_sender, mut error_receiver) = unbounded();

    let conn = circuit::multi_stream::multi_stream_node_connection(
        node.circuit_node(),
        in_reader,
        out_writer,
        false,
        circuit::Quality::LOCAL_SOCKET,
        error_sender,
        format!("VSOCK connection cid:{cid}"),
    );

    let conn = async move {
        if let Err(error) = conn.await {
            tracing::warn!(cid, ?error, "VSOCK Connection failed");
        }
    };

    let error_logger = {
        async move {
            while let Some(error) = error_receiver.next().await {
                tracing::debug!(vsock_cid = cid, ?error, "Stream encountered an error");
            }
        }
    };

    let in_fut = {
        let socket = &socket;
        async move {
            loop {
                let mut socket = match socket.readable().await {
                    Ok(s) => s,
                    Err(error) => {
                        tracing::warn!(cid, ?error, "Error waiting for data from VSOCK");
                        break;
                    }
                };

                if let Err(error) = in_writer.write(BUFFER_SIZE, |buf| {
                    match read(socket.get_inner().as_raw_fd(), buf) {
                        Ok(0) => Err(circuit::Error::ConnectionClosed(Some(
                            "VSOCK connection closed".to_owned(),
                        ))),
                        Ok(n) => Ok(n),
                        Err(nix::Error::EAGAIN) => {
                            socket.clear_ready_matching(Ready::READABLE);
                            Ok(0)
                        }
                        Err(e) => Err(circuit::Error::IO(e.into())),
                    }
                }) {
                    if let circuit::Error::ConnectionClosed(reason) = error {
                        tracing::debug!(cid, ?reason, "VSOCK connection closed");
                    } else {
                        tracing::warn!(cid, ?error, "VSOCK connection read error")
                    }
                    break;
                }
            }
        }
    };

    let out_fut = {
        let socket = &socket;
        async move {
            loop {
                let mut socket = match socket.writable().await {
                    Ok(s) => s,
                    Err(error) => {
                        tracing::warn!(cid, ?error, "Error waiting for data from VSOCK");
                        break;
                    }
                };

                if let Err(error) = out_reader
                    .read(1, |buf| match write(socket.get_inner(), buf) {
                        Ok(0) => Err(circuit::Error::ConnectionClosed(Some(
                            "VSOCK connection closed".to_owned(),
                        ))),
                        Ok(n) => Ok(((), n)),
                        Err(nix::Error::EAGAIN) => {
                            socket.clear_ready_matching(Ready::WRITABLE);
                            Ok(((), 0))
                        }
                        Err(e) => Err(circuit::Error::IO(e.into())),
                    })
                    .await
                {
                    if let circuit::Error::ConnectionClosed(reason) = error {
                        tracing::debug!(cid, ?reason, "VSOCK connection closed");
                    } else {
                        tracing::warn!(cid, ?error, "VSOCK connection write error")
                    }
                    break;
                }
            }
        }
    };

    futures::join!(conn, error_logger, in_fut, out_fut);
}
