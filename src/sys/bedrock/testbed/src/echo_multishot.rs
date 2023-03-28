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
    crate::task::{create_task, AlreadyStopped, ExitReason, RunningTask},
    anyhow::{anyhow, Error},
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
    type Stop = RunningTask;

    async fn start(mut self) -> Result<Self::Stop, Self::Error> {
        create_task(async move {
            info!("started EchoServer");

            let incoming = self.incoming.downcast_mut::<cap::Dict>().expect("should get a Dict");
            let echo_cap = incoming
                .entries
                .lock()
                .await
                .remove(ECHO_CAP_NAME)
                .expect("incoming should have echo cap");

            let echo_receiver = echo_cap
                .downcast::<cap::multishot::Receiver>()
                .expect("echo cap should be a Receiver");

            echo_receiver
                .for_each_concurrent(None, |item| async {
                    run_server(item).await.expect("server should not error")
                })
                .await;
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
    type Stop = RunningTask;

    async fn start(mut self) -> Result<Self::Stop, Self::Error> {
        create_task(async move {
            info!("started EchoClient");

            let incoming = self.incoming.downcast_mut::<cap::Dict>().expect("should get a Dict");
            let mut entries = incoming.entries.lock().await;
            let echo_cap = entries.get_mut(ECHO_CAP_NAME).expect("incoming should have echo cap");

            let echo_sender = echo_cap
                .downcast_mut::<cap::multishot::Sender>()
                .expect("echo cap should be a Sender");
            info!("(client) echo sender: {:?}", echo_sender);

            // Reconnect a few times.
            for _ in 0..3 {
                // Pipeline an echo server end using the sender.
                let (echo_proxy, echo_server_end) =
                    fidl::endpoints::create_proxy::<fexamples::EchoMarker>().unwrap();
                echo_sender
                    .send(Box::new(cap::Handle::from(echo_server_end.into_handle())))
                    .await
                    .unwrap();

                let message = "Hello, bedrock!";
                let response =
                    echo_proxy.echo_string(message).await.expect("EchoString should succeed");
                assert_eq!(response, message.to_string());
                info!("(client) got a response: {:?}", response);
            }
        })
        .start()
        .await
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
    let mut server_task = server.start().await.expect("should be able to start server");
    let mut client_task = client.start().await.expect("should be able to start client");

    assert_eq!(client_task.on_exit().await?, ExitReason::RanToCompletion);

    client_task.stop().await.expect("should be able to stop client");
    server_task.stop().await.expect("should be able to stop server");

    Ok(())
}
