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
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    exec::{Start, Stop},
    fidl::{endpoints::ServerEnd, HandleBased},
    fidl_fuchsia_examples as fexamples, fuchsia_async as fasync, fuchsia_zircon as zx,
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
                responder.send(&value).expect("error sending EchoString response");
            }
            fexamples::EchoRequest::SendString { value, control_handle } => {
                control_handle.send_on_string(&value).expect("error sending SendString event");
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
    type Error = Error;
    type Stop = Self;

    async fn start(mut self) -> Result<Self::Stop, Self::Error> {
        info!("started EchoServer");

        let incoming = self.incoming.downcast_mut::<cap::Dict>().context("not a Dict")?;
        let echo_cap = incoming
            .entries
            .lock()
            .await
            .remove(ECHO_CAP_NAME)
            .context("no echo cap in incoming")?;

        let echo_receiver = echo_cap
            .downcast::<cap::multishot::Receiver>()
            .map_err(|_| anyhow!("echo cap is not a Receiver"))?;

        echo_receiver
            .for_each_concurrent(None, |item| async {
                run_server(item).await.expect("server error")
            })
            .await;

        Ok(self)
    }
}

#[async_trait]
impl Stop for EchoServer {
    type Error = Error;

    async fn stop(&self) -> Result<(), Self::Error> {
        Ok(())
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
    type Error = Error;
    type Stop = Self;

    async fn start(mut self) -> Result<Self::Stop, Self::Error> {
        info!("started EchoClient");

        let incoming = self.incoming.downcast_mut::<cap::Dict>().context("not a Dict")?;
        let mut entries = incoming.entries.lock().await;
        let echo_cap = entries.get_mut(ECHO_CAP_NAME).context("no echo cap in incoming")?;

        let echo_sender = echo_cap
            .downcast_mut::<cap::multishot::Sender>()
            .context("echo cap is not a Sender")?;
        info!("(client) echo sender: {:?}", echo_sender);

        // Reconnect a few times.
        for _ in 0..3 {
            // Pipeline an echo server end using the sender.
            let (echo_proxy, echo_server_end) =
                fidl::endpoints::create_proxy::<fexamples::EchoMarker>()?;
            echo_sender.send(Box::new(cap::Handle::from(echo_server_end.into_handle()))).await?;

            let message = "Hello, bedrock!";
            let response =
                echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
            assert_eq!(response, message.to_string());
            info!("(client) got a response: {:?}", response);
        }

        drop(entries); // Release the lock.

        Ok(self)
    }
}

#[async_trait]
impl Stop for EchoClient {
    type Error = Error;

    async fn stop(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[fuchsia::test]
async fn test_echo_multishot() -> Result<(), Error> {
    let (echo_sender, echo_receiver) = cap::multishot();

    let server_incoming = Box::new(cap::Dict::new());
    server_incoming.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_receiver));
    let server = EchoServer::new(server_incoming);

    let client_incoming = Box::new(cap::Dict::new());
    client_incoming.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_sender));
    let client = EchoClient::new(client_incoming);

    // Start the server in the background. Run the client to completion.
    let server_task = fasync::Task::spawn(server.start());
    client.start().await.expect("client failed").stop().await?;
    server_task.await.expect("server failed").stop().await?;

    Ok(())
}
