// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use circuit::multi_stream::multi_stream_node_connection_to_async;
use futures::{AsyncReadExt, StreamExt};
use overnet_core::Router;
use std::sync::Weak;
use {fidl_fuchsia_vsock as vsock, fuchsia_async as fasync};

const OVERNET_VSOCK_PORT: u32 = 202;

pub async fn run_vsocks(router: Weak<Router>) -> Result<()> {
    let connector = fuchsia_component::client::connect_to_protocol::<vsock::ConnectorMarker>()?;
    let (client, mut requests) = fidl::endpoints::create_request_stream();
    connector.listen(OVERNET_VSOCK_PORT, client).await?.map_err(fidl::Status::from_raw)?;

    while let Some(request) = requests.next().await {
        let vsock::AcceptorRequest::Accept { addr, responder } = request?;

        log::info!(addr:? = addr; "Accepted VSOCK connection");

        let (client, con) = fidl::endpoints::create_endpoints();
        let (data, socket) = fidl::Socket::create_stream();
        let socket = fuchsia_async::Socket::from_socket(socket);
        let (mut reader, mut writer) = socket.split();
        let (err_sender, mut err_receiver) = futures::channel::mpsc::unbounded();

        let scope = fasync::Scope::new();
        scope.spawn(async move {
            while let Some(error) = err_receiver.next().await {
                log::debug!(
                    error:? = error;
                    "Stream error for VSOCK link"
                )
            }
        });

        let Some(router) = router.upgrade() else { return Ok(()) };

        scope.spawn(async move {
            let _client = client;

            if let Err(error) = multi_stream_node_connection_to_async(
                router.circuit_node(),
                &mut reader,
                &mut writer,
                true,
                circuit::Quality::LOCAL_SOCKET,
                err_sender,
                format!("VSOCK {addr:?}"),
            )
            .await
            {
                log::info!(
                    addr:? = addr,
                    error:? = error;
                    "VSOCK link terminated",
                );
            }
        });

        scope.detach();

        responder.send(Some(vsock::ConnectionTransport { data, con }))?;
    }
    Ok(())
}
