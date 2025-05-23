// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints;
use fidl_fidl_test_components::{TriggerMarker, TriggerRequestStream};
use fuchsia_component::client;
use fuchsia_component::server::{Item, ServiceFs};
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::{select, FutureExt, StreamExt, TryStreamExt};
use log::*;
use std::pin::pin;
use {
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_process_lifecycle as flifecycle,
    fuchsia_async as fasync,
};

enum IncomingRequest {
    Router(fsandbox::DictionaryRouterRequestStream),
}

/// See the `stop_with_dynamic_dictionary` test case.
#[fuchsia::main]
pub async fn main() {
    info!("Started");

    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let id_gen = sandbox::CapabilityIdGenerator::new();

    let dict_id = id_gen.next();
    store.dictionary_create(dict_id).await.unwrap().unwrap();

    let (receiver, receiver_stream) =
        endpoints::create_request_stream::<fsandbox::ReceiverMarker>();
    let connector_id = id_gen.next();
    store.connector_create(connector_id, receiver).await.unwrap().unwrap();
    store
        .dictionary_insert(
            dict_id,
            &fsandbox::DictionaryItem {
                key: format!("fidl.test.components.Trigger-dynamic"),
                value: connector_id,
            },
        )
        .await
        .unwrap()
        .unwrap();

    info!("Spawning receiver handler");
    let mut receiver_task = handle_receiver(receiver_stream).boxed_local().fuse();

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingRequest::Router);
    fs.take_and_serve_directory_handle().unwrap();

    // TODO(https://fxbug.dev/333080598): This is quite some boilerplate to escrow the outgoing dir.
    // Design some library function to handle the lifecycle requests.
    let lifecycle =
        fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::Lifecycle, 0)).unwrap();
    let lifecycle: zx::Channel = lifecycle.into();
    let lifecycle: endpoints::ServerEnd<flifecycle::LifecycleMarker> = lifecycle.into();
    let (mut lifecycle_request_stream, lifecycle_control_handle) =
        lifecycle.into_stream_and_control_handle();
    let mut lifecycle_task = async move {
        let Some(Ok(request)) = lifecycle_request_stream.next().await else {
            return std::future::pending::<()>().await;
        };
        match request {
            flifecycle::LifecycleRequest::Stop { .. } => {
                // TODO(https://fxbug.dev/332341289): If the framework asks us to stop, we still
                // end up dropping requests. If we teach the `ServiceFs` etc. libraries to skip
                // the timeout when this happens, we can cleanly stop the component.
                return;
            }
        }
    }
    .boxed_local()
    .fuse();

    let mut outgoing_dir_task = fs
        .until_stalled(fasync::MonotonicDuration::from_micros(1))
        .for_each_concurrent(None, move |item| {
            let store = store.clone();
            let id_gen = id_gen.clone();
            let lifecycle_control_handle = lifecycle_control_handle.clone();
            async move {
                match item {
                    Item::Request(services, _active_guard) => {
                        let IncomingRequest::Router(mut stream) = services;
                        info!("Opened DictionaryRouter stream");
                        while let Ok(Some(request)) = stream.try_next().await {
                            match request {
                                fsandbox::DictionaryRouterRequest::Route {
                                    payload: _,
                                    responder,
                                } => {
                                    info!("Received DictionaryRouterRequest");
                                    let dup_dict_id = id_gen.next();
                                    store.duplicate(dict_id, dup_dict_id).await.unwrap().unwrap();
                                    let capability =
                                        store.export(dup_dict_id).await.unwrap().unwrap();
                                    let fsandbox::Capability::Dictionary(dict) = capability else {
                                        panic!("capability was not a dictionary? {capability:?}");
                                    };
                                    let _ = responder.send(Ok(
                                        fsandbox::DictionaryRouterRouteResponse::Dictionary(dict),
                                    ));
                                }
                                fsandbox::DictionaryRouterRequest::_UnknownMethod {
                                    ordinal,
                                    ..
                                } => {
                                    warn!(ordinal:%; "Unknown DictionaryRouter request");
                                }
                            }
                        }
                        info!("DictionaryRouter stream closed");
                    }
                    Item::Stalled(outgoing_directory) => lifecycle_control_handle
                        .send_on_escrow(flifecycle::LifecycleOnEscrowRequest {
                            outgoing_dir: Some(outgoing_directory.into()),
                            ..Default::default()
                        })
                        .unwrap(),
                }
            }
        })
        .fuse();

    select! {
        _ = lifecycle_task => info!("Stopping due to lifecycle request"),
        _ = outgoing_dir_task => info!("Stopping due to idle activity"),
        _ = receiver_task => {},
    }

    info!("Exiting");
}

async fn handle_receiver(mut receiver_stream: fsandbox::ReceiverRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    while let Some(request) = receiver_stream.try_next().await.unwrap() {
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                info!("Received ReceiverRequest");
                task_group.spawn(async move {
                    let server_end = endpoints::ServerEnd::<TriggerMarker>::new(channel.into());
                    handle_trigger(server_end.into_stream()).await;
                    info!("Closed Trigger request stream");
                });
            }
            fsandbox::ReceiverRequest::_UnknownMethod { ordinal, .. } => {
                warn!(ordinal:%; "Unknown Receiver request");
            }
        }
    }
}

async fn handle_trigger(stream: TriggerRequestStream) {
    let (stream, stalled) =
        detect_stall::until_stalled(stream, fasync::MonotonicDuration::from_micros(1));
    let mut stream = pin!(stream);
    while let Ok(Some(request)) = stream.try_next().await {
        info!("Received TriggerRequest");
        match request {
            fidl_fidl_test_components::TriggerRequest::Run { responder } => {
                responder.send("hello").unwrap()
            }
        }
    }
    info!("Escrowing connection to Trigger");
    if let Ok(Some(server_end)) = stalled.await {
        // Send the server endpoint back to the framework.
        fuchsia_component::client::connect_channel_to_protocol_at::<TriggerMarker>(
            server_end.into(),
            "/escrow",
        )
        .unwrap();
    }
}
