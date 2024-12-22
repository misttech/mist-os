// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints;
use fidl_fidl_examples_routing_echo::{EchoMarker, EchoRequest, EchoRequestStream};
use fuchsia_component::client;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use log::*;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

enum IncomingRequest {
    Router(fsandbox::DictionaryRouterRequestStream),
}

#[fuchsia::main]
async fn main() {
    info!("Started");

    // [START init]
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let id_gen = sandbox::CapabilityIdGenerator::new();

    // Create a dictionary
    let dict_id = id_gen.next();
    store.dictionary_create(dict_id).await.unwrap().unwrap();

    // Add 3 Echo servers to the dictionary
    let mut receiver_tasks = fasync::TaskGroup::new();
    for i in 1..=3 {
        let (receiver, receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>();
        let connector_id = id_gen.next();
        store.connector_create(connector_id, receiver).await.unwrap().unwrap();
        store
            .dictionary_insert(
                dict_id,
                &fsandbox::DictionaryItem {
                    key: format!("fidl.examples.routing.echo.Echo-{i}"),
                    value: connector_id,
                },
            )
            .await
            .unwrap()
            .unwrap();
        receiver_tasks.spawn(async move { handle_echo_receiver(i, receiver_stream).await });
    }
    // [END init]

    info!("Populated the dictionary");

    // [START serve]
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingRequest::Router);
    fs.take_and_serve_directory_handle().unwrap();
    fs.for_each_concurrent(None, move |request: IncomingRequest| {
        let store = store.clone();
        let id_gen = id_gen.clone();
        async move {
            match request {
                IncomingRequest::Router(mut stream) => {
                    while let Ok(Some(request)) = stream.try_next().await {
                        match request {
                            // [START request]
                            fsandbox::DictionaryRouterRequest::Route { payload: _, responder } => {
                                let dup_dict_id = id_gen.next();
                                store.duplicate(dict_id, dup_dict_id).await.unwrap().unwrap();
                                let capability = store.export(dup_dict_id).await.unwrap().unwrap();
                                let fsandbox::Capability::Dictionary(dict) = capability else {
                                    panic!("capability was not a dictionary? {capability:?}");
                                };
                                let _ = responder.send(Ok(
                                    fsandbox::DictionaryRouterRouteResponse::Dictionary(dict),
                                ));
                            }
                            // [END request]
                            fsandbox::DictionaryRouterRequest::_UnknownMethod {
                                ordinal, ..
                            } => {
                                warn!(ordinal:%; "Unknown DictionaryRouter request");
                            }
                        }
                    }
                }
            }
        }
    })
    .await;
    // [END serve]
}

// [START receiver]
async fn handle_echo_receiver(index: u64, mut receiver_stream: fsandbox::ReceiverRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    while let Some(request) = receiver_stream.try_next().await.unwrap() {
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                task_group.spawn(async move {
                    let server_end = endpoints::ServerEnd::<EchoMarker>::new(channel.into());
                    run_echo_server(index, server_end.into_stream()).await;
                });
            }
            fsandbox::ReceiverRequest::_UnknownMethod { ordinal, .. } => {
                warn!(ordinal:%; "Unknown Receiver request");
            }
        }
    }
}

async fn run_echo_server(index: u64, mut stream: EchoRequestStream) {
    while let Ok(Some(event)) = stream.try_next().await {
        let EchoRequest::EchoString { value, responder } = event;
        let res = match value {
            Some(s) => responder.send(Some(&format!("{s} {index}"))),
            None => responder.send(None),
        };
        if let Err(err) = res {
            warn!(err:%; "Failed to send echo response");
        }
    }
}
// [END receiver]
