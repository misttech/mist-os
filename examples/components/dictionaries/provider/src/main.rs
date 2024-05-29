// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints;
use fidl_fidl_examples_routing_echo::{EchoMarker, EchoRequest, EchoRequestStream};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_async as fasync;
use fuchsia_component::{client, server::ServiceFs};
use futures::{StreamExt, TryStreamExt};
use tracing::*;

enum IncomingRequest {
    Router(fsandbox::RouterRequestStream),
}

#[fuchsia::main]
async fn main() {
    info!("Started");
    let factory = client::connect_to_protocol::<fsandbox::FactoryMarker>().unwrap();

    // Create a dictionary
    let dict = factory.create_dictionary().await.unwrap();
    let dict = dict.into_proxy().unwrap();

    // Add 3 Echo servers to the dictionary
    let mut receiver_tasks = fasync::TaskGroup::new();
    for i in 1..=3 {
        let (receiver, receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
        let sender = factory.create_connector(receiver).await.unwrap();
        dict.insert(
            &format!("fidl.examples.routing.echo.Echo-{i}"),
            fsandbox::Capability::Connector(sender),
        )
        .await
        .unwrap()
        .unwrap();
        receiver_tasks.spawn(async move { handle_echo_receiver(i, receiver_stream).await });
    }

    info!("Populated the dictionary");

    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingRequest::Router);
    fs.take_and_serve_directory_handle().unwrap();
    fs.for_each_concurrent(None, move |request: IncomingRequest| {
        let dict = Clone::clone(&dict);
        async move {
            match request {
                IncomingRequest::Router(mut stream) => {
                    while let Ok(Some(request)) = stream.try_next().await {
                        match request {
                            fsandbox::RouterRequest::Route { payload: _, responder } => {
                                let client_end = dict.clone().await.unwrap();
                                let capability = fsandbox::Capability::Dictionary(client_end);
                                let _ = responder.send(Ok(capability));
                            }
                            fsandbox::RouterRequest::_UnknownMethod { ordinal, .. } => {
                                warn!(%ordinal, "Unknown Router request");
                            }
                        }
                    }
                }
            }
        }
    })
    .await;
}

async fn handle_echo_receiver(index: u32, mut receiver_stream: fsandbox::ReceiverRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    while let Some(request) = receiver_stream.try_next().await.unwrap() {
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                task_group.spawn(async move {
                    let server_end = endpoints::ServerEnd::<EchoMarker>::new(channel.into());
                    run_echo_server(index, server_end.into_stream().unwrap()).await;
                });
            }
            fsandbox::ReceiverRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown Receiver request");
            }
        }
    }
}

async fn run_echo_server(index: u32, mut stream: EchoRequestStream) {
    while let Ok(Some(event)) = stream.try_next().await {
        let EchoRequest::EchoString { value, responder } = event;
        let res = match value {
            Some(s) => responder.send(Some(&format!("{s} {index}"))),
            None => responder.send(None),
        };
        if let Err(err) = res {
            warn!(%err, "Failed to send echo response");
        }
    }
}
