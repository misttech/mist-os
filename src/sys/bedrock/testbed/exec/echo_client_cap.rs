// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Connects to the `fuchsia.examples.Echo` FIDL protocol and calls a method. This client receives
//! a `cap::Dict` capability via the `User0` processargs numbered handle - its "incoming" Dict -
//! that contains a handle to a `cap::Sender`. The sender handle is a client end to the
//! `fuchsia.component.bedrock.Sender` protocol. The client connects to the protocol by sending
//! a server end through the Sender to the server.

use {
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::Proxy,
    fidl::{endpoints::create_proxy, HandleBased},
    fidl_fuchsia_component_bedrock as fbedrock, fidl_fuchsia_examples as fexamples,
    fuchsia_async as fasync,
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    fuchsia_zircon as zx,
};

/// The name of the echo Sender capability in the dict.
const ECHO_SENDER_CAP_NAME: &str = "echo_server_end_sender";

#[fuchsia::main(logging = false)]
async fn main() -> Result<(), Error> {
    let dict_handle = take_startup_handle(HandleInfo::new(HandleType::User0, 0))
        .context("missing Dict startup handle")?;

    assert!(!dict_handle.is_invalid());

    let dict_proxy = fbedrock::DictProxy::from_channel(
        fasync::Channel::from_channel(dict_handle.into_handle_based::<zx::Channel>())
            .context("failed to create channel")?,
    );

    // Take the Sender from the Dict, via the Dict FIDL interface.
    let sender_handle = dict_proxy
        .remove(ECHO_SENDER_CAP_NAME)
        .await
        .context("failed to call Dict.Remove")?
        .map_err(|err| anyhow!("failed to get sender entry from dict: {:?}", err))?;

    let sender_proxy = fbedrock::SenderProxy::from_channel(
        fasync::Channel::from_channel(sender_handle.into_handle_based::<zx::Channel>())
            .context("failed to create channel")?,
    );

    let (echo_proxy, echo_server_end) =
        create_proxy::<fexamples::EchoMarker>().context("failed to create Echo proxy")?;

    // Send the server end over the Sender.
    sender_proxy
        .send_(echo_server_end.into())
        .await
        .context("failed to call Send")?
        .map_err(|err| anyhow!("failed to send Echo server end: {:?}", err))?;

    // Call the EchoString method
    let message = "Hello, bedrock!";
    let response = echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
    assert_eq!(response, message.to_string());

    Ok(())
}
