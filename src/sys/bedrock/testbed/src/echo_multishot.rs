// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that sends server ends of a FIDL protocol through a pair of multishot
//! `Sender` and `Receiver`, from an Echo client to a server.
//!
//! `EchoServer` and `EchoClient` act like programs. They can be started and accept a `Dict`
//! capability that contains "incoming" capabilities provided to them.
//!
//! `EchoServer` gets the `Sender` capability in its incoming `Dict`, and sends the client end
//! of the Echo protocol through the Sender.
//!
//! `EchoClient` gets the `Receiver` capability in its incoming `Dict`, receives the client
//! end of the Echo protocol, and calls a method.
//!
//! The `Sender`/`Receiver` is multishot, so the client can reconnect by sending more server ends.

use {
    crate::task::{create_task, AlreadyStopped, ArcError, RunningTask},
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    exec::{Lifecycle, Start, Stop},
    fidl::{endpoints::ServerEnd, HandleBased},
    fidl_fuchsia_examples as fexamples, fuchsia_zircon as zx,
    futures::{SinkExt, StreamExt},
    tracing::info,
};

/// The name of the echo capability in incoming/outgoing dicts.
const ECHO_CAP_NAME: &str = "echo";

struct EchoServer {
    incoming: cap::AnyCapability,
}

async fn run_server(item: cap::AnyCapability) -> anyhow::Result<()> {
    let echo_server_end =
        item.downcast::<cap::Handle>().map_err(|_| anyhow!("item is not a Handle"))?;
    let mut stream = ServerEnd::<fexamples::EchoMarker>::new(fidl::Channel::from(
        (*echo_server_end).into_handle_based::<zx::Channel>(),
    ))
    .into_stream()?;
    while let Some(Ok(echo_request)) = stream.next().await {
        match echo_request {
            fexamples::EchoRequest::EchoString { value, responder } => {
                responder.send(&value).expect("sending EchoString response should succeed");
            }
            fexamples::EchoRequest::SendString { value, control_handle } => {
                control_handle
                    .send_on_string(&value)
                    .expect("sending SendString event should succeed");
            }
        }
    }

    Ok(())
}

impl EchoServer {
    fn new(incoming: cap::AnyCapability) -> Self {
        Self { incoming }
    }
}

#[async_trait]
impl Start for EchoServer {
    type Error = AlreadyStopped;
    type Stop = RunningTask<Result<(), ArcError>>;

    async fn start(mut self) -> Result<Self::Stop, Self::Error> {
        create_task(async move {
            info!("started EchoServer");

            let incoming = self.incoming.downcast_mut::<cap::Dict>().context("not a Dict")?;
            let echo_cap =
                incoming.entries.remove(ECHO_CAP_NAME).context("no echo cap in incoming")?;

            let echo_receiver = echo_cap
                .downcast::<cap::multishot::Receiver>()
                .map_err(|_| anyhow!("echo cap is not a Handle"))?;

            echo_receiver
                .for_each_concurrent(None, |item| async {
                    run_server(item).await.expect("FIDL server should not fail")
                })
                .await;

            Ok(())
        })
        .start()
        .await
    }
}

struct EchoClient {
    incoming: cap::AnyCapability,
}

impl EchoClient {
    fn new(incoming: cap::AnyCapability) -> Self {
        Self { incoming }
    }
}

#[async_trait]
impl Start for EchoClient {
    type Error = AlreadyStopped;
    type Stop = RunningTask<Result<(), ArcError>>;

    async fn start(mut self) -> Result<Self::Stop, Self::Error> {
        create_task(async move {
            info!("started EchoClient");

            let incoming = self.incoming.downcast_mut::<cap::Dict>().context("not a Dict")?;
            let echo_cap =
                incoming.entries.get_mut(ECHO_CAP_NAME).context("no echo cap in incoming")?;

            let echo_sender = echo_cap
                .downcast_mut::<cap::multishot::Sender>()
                .ok_or_else(|| anyhow!("echo cap is not a Sender"))?;
            info!("(client) echo sender: {:?}", echo_sender);

            // Reconnect a few times.
            for _ in 0..3 {
                // Pipeline an echo server end using the sender.
                let (echo_proxy, echo_server_end) =
                    fidl::endpoints::create_proxy::<fexamples::EchoMarker>()
                        .context("failed to create proxy")?;
                echo_sender
                    .send(Box::new(cap::Handle::from(echo_server_end.into_handle())))
                    .await
                    .map_err(|e| anyhow::Error::from(e))?;

                let message = "Hello, bedrock!";
                let response =
                    echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
                assert_eq!(response, message.to_string());
                info!("(client) got a response: {:?}", response);
            }

            Ok(())
        })
        .start()
        .await
    }
}

#[fuchsia::test]
async fn test_echo_multishot() -> Result<(), Error> {
    let (echo_sender, echo_receiver) = cap::multishot();

    let mut server_incoming = Box::new(cap::Dict::new());
    server_incoming.entries.insert(ECHO_CAP_NAME.into(), Box::new(echo_receiver));
    let server = EchoServer::new(server_incoming);

    let mut client_incoming = Box::new(cap::Dict::new());
    client_incoming.entries.insert(ECHO_CAP_NAME.into(), Box::new(echo_sender));
    let client = EchoClient::new(client_incoming);

    // Start the server in the background. Run the client to completion.
    let mut server_task = server.start().await.expect("should be able to start server");
    let mut client_task = client.start().await.expect("should be able to start client");

    client_task.on_exit().await?.expect("should be able to observe client task exit")?;

    client_task.stop().await.expect("should be able to stop client");
    server_task.stop().await.expect("should be able to stop server");

    server_task.on_exit().await?;

    Ok(())
}
