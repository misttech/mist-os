// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that sends the server and client ends of a FIDL channel to the server and
//! client, and verifies that the client can call FIDL methods.
//!
//! `EchoServer` and `EchoClient` act like programs. They can be started and accept a `Dict`
//! capability that contains "incoming" capabilities provided to them.
//!
//! `EchoServer` gets a `Handle` capability in its incoming `Dict` for the server end of the
//! Echo protocol, and serves the protocol.
//!
//! `EchoClient` gets a `Handle` capability in its incoming `Dict` for the client end of the
//! Echo protocol, and calls a method.
//!
//! Since the `Handle` capabilities represent a single connection and are sent once,
//! the client cannot reconnect.

use {
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    exec::{Start, Stop},
    fidl::endpoints::{create_endpoints, Proxy, RequestStream},
    fidl::HandleBased,
    fidl_fuchsia_examples as fexamples, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::{try_join, StreamExt, TryStreamExt},
    tracing::{error, info},
};

/// The name of the echo capability in incoming/outgoing dicts.
const ECHO_CAP_NAME: &str = "echo";

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
        let echo_handle =
            echo_cap.downcast::<cap::Handle>().map_err(|_| anyhow!("echo cap is not a Handle"))?;
        info!("(server) echo handle: {:?}", echo_handle);

        let echo_stream = fexamples::EchoRequestStream::from_channel(
            fasync::Channel::from_channel((*echo_handle).into_handle_based::<zx::Channel>())
                .context("failed to create channel")?,
        );

        // Serve the Echo protocol on the server end.
        echo_stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async move {
                match request {
                    fexamples::EchoRequest::EchoString { value, responder } => {
                        responder.send(&value).context("error sending EchoString response")?;
                    }
                    fexamples::EchoRequest::SendString { value, control_handle } => {
                        control_handle
                            .send_on_string(&value)
                            .context("error sending SendString event")?;
                    }
                }
                Ok(())
            })
            .await
            .unwrap_or_else(|err| error!(error=%err, "FIDL stream handler failed"));

        Ok(self)
    }
}

#[async_trait]
impl Stop for EchoServer {
    type Error = Error;

    async fn stop(&mut self) -> Result<(), Self::Error> {
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
        let echo_cap = incoming
            .entries
            .lock()
            .await
            .remove(ECHO_CAP_NAME)
            .context("no echo cap in incoming")?;
        let echo_handle =
            echo_cap.downcast::<cap::Handle>().map_err(|_| anyhow!("echo cap is not a Handle"))?;
        info!("(client) echo handle: {:?}", echo_handle);

        let echo_proxy = fexamples::EchoProxy::from_channel(
            fasync::Channel::from_channel((*echo_handle).into_handle_based::<zx::Channel>())
                .context("failed to create channel")?,
        );

        let message = "Hello, bedrock!";
        let response =
            echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
        assert_eq!(response, message.to_string());

        info!("(client) got a response: {:?}", response);

        Ok(self)
    }
}

#[async_trait]
impl Stop for EchoClient {
    type Error = Error;

    async fn stop(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[fuchsia::test]
async fn test_echo_oneshot_channel() -> Result<(), Error> {
    let (echo_server_end, echo_client_end) = create_endpoints::<fexamples::EchoMarker>();

    // Send the server and client ends as Handle capabilities.
    let echo_server_cap = cap::Handle::from(zx::Handle::from(echo_server_end));
    let echo_client_cap = cap::Handle::from(zx::Handle::from(echo_client_end));

    let server_incoming = Box::new(cap::Dict::new());
    server_incoming.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_server_cap));
    let server = EchoServer::new(server_incoming);

    let client_incoming = Box::new(cap::Dict::new());
    client_incoming.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_client_cap));
    let client = EchoClient::new(client_incoming);

    try_join!(server.start(), client.start())?;

    Ok(())
}
