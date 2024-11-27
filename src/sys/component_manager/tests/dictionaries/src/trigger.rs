// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This program serves two Trigger protocols: one from the outgoing directory in the standard
//! manner (Trigger-c), and one from a dynamic dictionary that is exposed using the Router
//! protocol. This lets us test a dictionary that is a composite of dynamic and static routes.

use fidl::endpoints;
use fuchsia_component::client;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use tracing::info;
use {
    fidl_fidl_examples_routing_echo as fecho, fidl_fidl_test_components as ftest,
    fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync,
};

enum IncomingRequest {
    Router(fsandbox::DictionaryRouterRequestStream),
    Trigger(ftest::TriggerRequestStream),
}

#[fasync::run_singlethreaded]
async fn main() {
    info!("trigger.cm started");
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let dict_id = 1;
    store.dictionary_create(dict_id).await.unwrap().unwrap();

    // Dynamically add trigger-d to the dictionary
    let (trigger_receiver_client, trigger_receiver_stream) =
        endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
    let connector_id = 100;
    store.connector_create(connector_id, trigger_receiver_client).await.unwrap().unwrap();
    store
        .dictionary_insert(
            dict_id,
            &fsandbox::DictionaryItem {
                key: "fidl.test.components.Trigger-d".into(),
                value: connector_id,
            },
        )
        .await
        .unwrap()
        .unwrap();
    info!("trigger.cm populated the dictionary");

    let _receiver_task =
        fasync::Task::local(async move { handle_receiver(trigger_receiver_stream).await });

    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingRequest::Trigger);
    fs.dir("svc").add_fidl_service(IncomingRequest::Router);
    fs.take_and_serve_directory_handle().expect("failed to serve outgoing directory");
    fs.for_each_concurrent(None, move |request: IncomingRequest| {
        let store = store.clone();
        async move {
            match request {
                IncomingRequest::Trigger(stream) => {
                    run_trigger_service("Triggered c", stream).await
                }
                IncomingRequest::Router(mut stream) => {
                    while let Ok(Some(request)) = stream.try_next().await {
                        match request {
                            fsandbox::DictionaryRouterRequest::Route { payload: _, responder } => {
                                let dup_dict_id = dict_id + 1;
                                store.duplicate(dict_id, dup_dict_id).await.unwrap().unwrap();
                                let capability = store.export(dup_dict_id).await.unwrap().unwrap();
                                let fsandbox::Capability::Dictionary(dict) = capability else {
                                    panic!("capability was not a dictionary? {capability:?}");
                                };
                                let _ = responder.send(Ok(
                                    fsandbox::DictionaryRouterRouteResponse::Dictionary(dict),
                                ));
                            }
                            fsandbox::DictionaryRouterRequest::_UnknownMethod { .. } => {
                                unimplemented!()
                            }
                        }
                    }
                }
            }
        }
    })
    .await;
}

async fn run_trigger_service(echo_str: &str, mut stream: ftest::TriggerRequestStream) {
    let echo =
        client::connect_to_protocol::<fecho::EchoMarker>().expect("error connecting to echo");
    while let Some(event) = stream.try_next().await.expect("failed to serve trigger service") {
        let ftest::TriggerRequest::Run { responder } = event;
        let out = echo.echo_string(Some(echo_str)).await.expect("echo_string failed");
        let out = out.expect("empty echo result");
        responder.send(&out).expect("failed to send trigger response");
    }
}

async fn handle_receiver(mut receiver_stream: fsandbox::ReceiverRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    while let Some(request) = receiver_stream.try_next().await.unwrap() {
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                task_group.spawn(async move {
                    let server_end =
                        endpoints::ServerEnd::<ftest::TriggerMarker>::new(channel.into());
                    run_trigger_service("Triggered d", server_end.into_stream()).await;
                });
            }
            fsandbox::ReceiverRequest::_UnknownMethod { .. } => {
                unimplemented!()
            }
        }
    }
}
