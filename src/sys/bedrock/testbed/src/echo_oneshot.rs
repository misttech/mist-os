// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that sends a client end of a FIDL channel through a pair of capabilities,
//! `Sender` and `Receiver`, from an Echo server to a client.
//!
//! `Sender` and `Receiver` form a one-shot channel used to send a single capability.
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
//! Since the `Sender`/`Receiver` channel is one-shot, the client cannot reconnect.

use {
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    exec::{Start, Stop},
    fidl::endpoints::{spawn_stream_handler, Proxy},
    fidl::HandleBased,
    fidl_fuchsia_examples as fexamples, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::future::BoxFuture,
    futures::try_join,
    tracing::info,
};

/// The name of the echo capability in incoming/outgoing dicts.
const ECHO_CAP_NAME: &str = "echo";

/// A capability that serves a single Echo protocol connection.
#[derive(Debug)]
struct Echo {
    proxy: fexamples::EchoProxy,
}
impl cap::Capability for Echo {}
impl cap::Remote for Echo {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        unimplemented!()
    }
}

impl Echo {
    pub fn new() -> Self {
        let proxy = spawn_stream_handler(move |echo_request| async move {
            match echo_request {
                fexamples::EchoRequest::EchoString { value, responder } => {
                    responder.send(&value).expect("error sending EchoString response");
                }
                fexamples::EchoRequest::SendString { value, control_handle } => {
                    control_handle.send_on_string(&value).expect("error sending SendString event");
                }
            }
        })
        .unwrap();

        Echo { proxy }
    }

    pub fn into_handle_cap(self) -> cap::Handle {
        self.proxy.into_channel().unwrap().into_zx_channel().into_handle().into()
    }
}

struct EchoServer {
    incoming: cap::AnyCapability,
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
        let echo_sender = echo_cap
            .downcast::<cap::oneshot::Sender>()
            .map_err(|_| anyhow!("echo cap is not a Sender"))?;
        info!("(server) echo sender: {:?}", echo_sender);

        // Start the echo server and send the client end.
        let echo_handle = Box::new(Echo::new().into_handle_cap());
        echo_sender.send(echo_handle).expect("failed to send Echo");

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
        let echo_receiver = echo_cap
            .downcast_mut::<cap::oneshot::Receiver>()
            .context("echo cap is not a Receiver")?;
        info!("(client) echo receiver: {:?}", echo_receiver);
        // Get the echo client end from the receiver.
        let echo_handle = echo_receiver
            .await?
            .downcast::<cap::Handle>()
            .map_err(|_| anyhow!("echo is not a Handle"))?;
        info!("(client) echo_handle: {:?}", echo_handle);

        let echo_proxy = fexamples::EchoProxy::from_channel(
            fasync::Channel::from_channel(echo_handle.into_handle_based::<zx::Channel>())
                .context("failed to create channel")?,
        );

        let message = "Hello, bedrock!";
        let response =
            echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
        assert_eq!(response, message.to_string());

        info!("(client) got a response: {:?}", response);

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
async fn test_echo_oneshot() -> Result<(), Error> {
    let (echo_sender, echo_receiver) = cap::oneshot();

    let server_incoming = Box::new(cap::Dict::new());
    server_incoming.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_sender));
    let server = EchoServer::new(server_incoming);

    let client_incoming = Box::new(cap::Dict::new());
    client_incoming.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_receiver));
    let client = EchoClient::new(client_incoming);

    try_join!(server.start(), client.start())?;

    Ok(())
}
