// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;
use futures::channel::mpsc::unbounded;
use futures::{AsyncReadExt, StreamExt};
use std::sync::Arc;
use usb_vsock_host::UsbVsockHost;

use super::vsock::OVERNET_VSOCK_PORT;

/// Host-pipe-like task for communicating via vsock.
pub async fn spawn_usb(host: Arc<UsbVsockHost>, cid: u32, node: Arc<overnet_core::Router>) {
    tracing::debug!(cid, "Spawning USB VSOCK host pipe");

    let Ok(cid) = cid.try_into() else {
        tracing::warn!("Tried to connect to USB CID 0");
        return;
    };

    let (socket, other_end) = fasync::emulated_handle::Socket::create_stream();
    let other_end = fasync::Socket::from_socket(other_end);
    if let Err(error) = host.connect(cid, OVERNET_VSOCK_PORT, other_end).await {
        tracing::warn!(cid, ?error, "Could not connect to USB VSOCK");
        return;
    }

    tracing::debug!(cid, "USB VSOCK connection established");

    let (error_sender, mut error_receiver) = unbounded();

    let (mut socket_reader, mut socket_writer) = fasync::Socket::from_socket(socket).split();

    let conn = circuit::multi_stream::multi_stream_node_connection_to_async(
        node.circuit_node(),
        &mut socket_reader,
        &mut socket_writer,
        false,
        circuit::Quality::LOCAL_SOCKET,
        error_sender,
        format!("USB VSOCK connection cid:{cid}"),
    );

    let conn = async move {
        if let Err(error) = conn.await {
            tracing::warn!(cid, ?error, "USB VSOCK Connection failed");
        }
    };

    let error_logger = {
        async move {
            while let Some(error) = error_receiver.next().await {
                tracing::debug!(vsock_cid = cid, ?error, "Stream encountered an error");
            }
        }
    };

    futures::join!(conn, error_logger);
}
