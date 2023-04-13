// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Serves a single `fuchsia.examples.Echo` FIDL protocol connection. The server end is exposed
//! in a `cap` Dict capability, which is exposed via the `User0` processargs numbered handle.

use {
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::{Proxy, RequestStream},
    fidl::HandleBased,
    fidl_fuchsia_component_bedrock as fbedrock,
    fidl_fuchsia_examples::{EchoRequest, EchoRequestStream},
    fuchsia_async as fasync,
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    fuchsia_zircon as zx,
    futures::prelude::*,
};

/// The name of the echo capability in the dict.
const ECHO_CAP_NAME: &str = "echo";

#[fuchsia::main(logging = false)]
async fn main() -> Result<(), Error> {
    let dict_handle = take_startup_handle(HandleInfo::new(HandleType::User0, 0))
        .context("missing Dict startup handle")?;

    assert!(!dict_handle.is_invalid());

    let dict_proxy = fbedrock::DictProxy::from_channel(
        fasync::Channel::from_channel(dict_handle.into_handle_based::<zx::Channel>())
            .context("failed to create channel")?,
    );

    // Take the Echo protocol server end from the Dict, via the Dict FIDL interface.
    let echo_server_end_handle = dict_proxy
        .remove(ECHO_CAP_NAME)
        .await
        .context("failed to call Dict.Remove")?
        .map_err(|err| anyhow!("failed to get echo entry from dict: {:?}", err))?;

    let echo_request_stream = EchoRequestStream::from_channel(
        fasync::Channel::from_channel(echo_server_end_handle.into_handle_based::<zx::Channel>())
            .context("failed to create channel")?,
    );

    handle_echo_requests(echo_request_stream).await.context("failed to serve Echo")?;

    Ok(())
}

async fn handle_echo_requests(stream: EchoRequestStream) -> Result<(), Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                EchoRequest::EchoString { value, responder } => {
                    responder.send(&value).context("error sending EchoString response")?;
                }
                EchoRequest::SendString { value, control_handle } => {
                    control_handle
                        .send_on_string(&value)
                        .context("error sending SendString event")?;
                }
            }
            Ok(())
        })
        .await
}
