// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context, Error},
    fidl_fuchsia_examples::{EchoRequest, EchoRequestStream},
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::{component, health::Reporter},
    futures::prelude::*,
};

// Wrap protocol requests being served.
enum IncomingRequest {
    Echo(EchoRequestStream),
}

#[fuchsia::main(logging = false)]
async fn main() -> Result<(), anyhow::Error> {
    let mut service_fs = ServiceFs::new_local();

    // Initialize inspect
    component::health().set_starting_up();
    inspect_runtime::serve(component::inspector(), &mut service_fs)?;

    // Serve the Echo protocol
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Echo);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    // Component is serving and ready to handle incoming requests
    component::health().set_ok();

    // Attach request handler for incoming requests
    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async move {
            match request {
                IncomingRequest::Echo(stream) => {
                    handle_echo_request(stream).await.expect("failed to serve Echo")
                }
            }
        })
        .await;

    Ok(())
}

async fn handle_echo_request(stream: EchoRequestStream) -> Result<(), Error> {
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
