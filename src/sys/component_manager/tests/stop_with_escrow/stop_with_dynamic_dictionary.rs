// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints;
use fidl_fidl_test_components::{TriggerMarker, TriggerRequestStream};
use fidl_fuchsia_component_sandbox::DictionaryRef;
use fuchsia_component::client;
use fuchsia_component::server::{Item, ServiceFs};
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::{future, select, FutureExt, StreamExt, TryStreamExt};
use log::*;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use zx::{HandleBased, Rights};
use {
    detect_stall, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_process_lifecycle as flifecycle, fuchsia_async as fasync,
};

/// Duration to wait for FIDL requests before stalling the component. Set to an
/// impossibly low duration to simulate time gaps between requests that would
/// cause the component to stall between requests. In production, this would
/// likely be in the order of seconds or minutes.
const STALL_INTERVAL: fasync::MonotonicDuration = fasync::MonotonicDuration::from_micros(1);

/// Protocol used by clients to connect. In production, this would likely be
/// generated at runtime.
const DYNAMIC_TRIGGER_PROTOCOL: &str = "fidl.test.components.Trigger-dynamic";

/// Escrowable protocol used by Component Manager to connect to this component
/// again after the client channel to [`DYNAMIC_TRIGGER_PROTOCOL`] idles.
const STATIC_TRIGGER_PROTOCOL: &str = "fidl.test.components.Trigger-static";

/// Escrowable protocol used by Component Manager to connect the server-side
/// channel to [`DYNAMIC_TRIGGER_PROTOCOL`] to this component.
const STATIC_RECEIVER_PROTOCOL: &str = "fuchsia.component.sandbox.Receiver-static";

enum IncomingRequest {
    Router(fsandbox::DictionaryRouterRequestStream),
    Receiver(fsandbox::ReceiverRequestStream),
    Static(TriggerRequestStream),
}

/// Load a previous escrowed dictionary if available, otherwise generate a
/// dictionary. This dictionary contains the entries necessary for a dynamic
/// dictionary to map [`DYNAMIC_TRIGGER_PROTOCOL`] to
/// [`STATIC_TRIGGER_PROTOCOL`].
pub async fn get_escrowed_dict() -> (DictionaryRef, Option<impl Future<Output = ()>>) {
    if let Some(dictionary) =
        fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::EscrowedDictionary, 0))
    {
        info!("Reusing escrowed dictionary");
        return (fsandbox::DictionaryRef { token: dictionary.into() }, None);
    }

    info!("No escrowed dictionary available; generating one");
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let id_gen = sandbox::CapabilityIdGenerator::new();
    let dict_id = id_gen.next();
    store.dictionary_create(dict_id).await.unwrap().unwrap();

    let (client_end, server_end) = endpoints::create_endpoints::<fsandbox::ReceiverMarker>();
    let receiver_task = handle_receiver(server_end.into_stream());

    let connector_id = id_gen.next();
    store.connector_create(connector_id, client_end).await.unwrap().unwrap();
    store
        .dictionary_insert(
            dict_id,
            &fsandbox::DictionaryItem {
                key: DYNAMIC_TRIGGER_PROTOCOL.to_string(),
                value: connector_id,
            },
        )
        .await
        .unwrap()
        .unwrap();

    match store.export(dict_id).await.unwrap().unwrap() {
        fsandbox::Capability::Dictionary(dict) => (dict, Some(receiver_task)),
        capability @ _ => {
            panic!("unexpected {capability:?}");
        }
    }
}

/// See the `stop_with_dynamic_dictionary` test case.
#[fuchsia::main]
pub async fn main() {
    info!("Started");

    let (dict, receiver_task) = get_escrowed_dict().await;

    // Serve FIDL services for all static protocols.
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingRequest::Router);
    fs.dir("svc").add_fidl_service_at(STATIC_TRIGGER_PROTOCOL, |rs| IncomingRequest::Static(rs));
    fs.dir("svc").add_fidl_service_at(STATIC_RECEIVER_PROTOCOL, |rs| IncomingRequest::Receiver(rs));

    fs.take_and_serve_directory_handle().unwrap();

    // Ignore stop requests in favor of relying on `until_stalled` timeouts.
    //
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
                // TODO(https://fxbug.dev/332341289): Teach `ServiceFs` and
                // others to skip the `until_stalled` timeout when this happens
                // so we can cleanly stop the component.
                return;
            }
        }
    }
    .boxed_local()
    .fuse();

    // Reference-count the DictionaryRef since it's not clonable and duplicating
    // it with `duplicate_handle` for every request in `for_each_concurrent`
    // would be unnecessary for handling IncomingRequest::{Receiver, Static}.
    let dict = Rc::new(dict);

    let outgoing_dir_task =
        fs.until_stalled(STALL_INTERVAL).for_each_concurrent(None, move |item| {
            let lifecycle_control_handle = lifecycle_control_handle.clone();
            let dict = dict.clone();
            async move {
                match item {
                    Item::Request(services, _active_guard) => match services {
                        IncomingRequest::Router(stream) => handle_router(stream, dict).await,
                        IncomingRequest::Receiver(stream) => handle_receiver(stream).await,
                        IncomingRequest::Static(stream) => handle_trigger(stream).await,
                    },
                    Item::Stalled(outgoing_directory) => {
                        let dict = DictionaryRef {
                            token: dict.token.duplicate_handle(Rights::SAME_RIGHTS).unwrap(),
                        };
                        lifecycle_control_handle
                            .send_on_escrow(flifecycle::LifecycleOnEscrowRequest {
                                outgoing_dir: Some(outgoing_directory.into()),
                                escrowed_dictionary: Some(dict),
                                ..Default::default()
                            })
                            .unwrap();
                    }
                }
            }
        });

    let mut server_tasks = match receiver_task {
        Some(task) => {
            future::join(outgoing_dir_task.fuse(), task.fuse()).map(|_| ()).boxed_local().fuse()
        }
        None => outgoing_dir_task.boxed_local().fuse(),
    };

    select! {
        _ = lifecycle_task => info!("Stopping due to lifecycle request"),
        _ = server_tasks => info!("Stopping due to idle activity"),
    }
}

/// Handle fuchsia.component.sandbox.DictionaryRouter requests with support for
/// escrowing in case the client doesn't send a request right away.
async fn handle_router(stream: fsandbox::DictionaryRouterRequestStream, dict: Rc<DictionaryRef>) {
    let (stream, stalled) = detect_stall::until_stalled(stream, STALL_INTERVAL);
    let mut stream = pin!(stream);
    while let Ok(Some(request)) = stream.try_next().await {
        info!("Received fuchsia.component.sandbox.DictionaryRouterRequest");
        match request {
            fsandbox::DictionaryRouterRequest::Route { payload: _, responder } => {
                let dict = DictionaryRef {
                    token: dict.token.duplicate_handle(Rights::SAME_RIGHTS).unwrap(),
                };
                if let Err(e) =
                    responder.send(Ok(fsandbox::DictionaryRouterRouteResponse::Dictionary(dict)))
                {
                    warn!("Failed to send RouteResponse {e:?}")
                }
            }
            fsandbox::DictionaryRouterRequest::_UnknownMethod { ordinal, .. } => {
                warn!(ordinal:%; "Unknown DictionaryRouter request");
            }
        }
    }
    if let Ok(Some(server_end)) = stalled.await {
        // Send the server endpoint back to the framework.
        info!("Escrowing fuchsia.component.sandbox.DictionaryRouter");
        fuchsia_component::client::connect_channel_to_protocol_at_path(
            server_end.into(),
            "/escrow/fuchsia.component.sandbox.DictionaryRouter",
        )
        .unwrap();
    }
}

/// Handle fuchsia.component.sandbox.Receiver requests with support for
/// escrowing in case the client doesn't send a request right away.
async fn handle_receiver(stream: fsandbox::ReceiverRequestStream) {
    let (stream, stalled) = detect_stall::until_stalled(stream, STALL_INTERVAL);
    let mut stream = pin!(stream);
    while let Ok(Some(request)) = stream.try_next().await {
        info!("Received fuchsia.component.sandbox.ReceiverRequest");
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                let server_end = endpoints::ServerEnd::<TriggerMarker>::new(channel);
                handle_trigger(server_end.into_stream()).await;
            }
            fsandbox::ReceiverRequest::_UnknownMethod { ordinal, .. } => {
                warn!(ordinal:%; "Unknown Receiver request");
            }
        }
    }
    if let Ok(Some(server_end)) = stalled.await {
        // Send the server endpoint back to the framework.
        info!("Escrowing {STATIC_RECEIVER_PROTOCOL}");
        fuchsia_component::client::connect_channel_to_protocol_at_path(
            server_end.into(),
            &format!("/escrow/{STATIC_RECEIVER_PROTOCOL}"),
        )
        .unwrap();
    }
}

/// Handle fidl.test.components.Trigger requests with support for escrowing in
/// case the client doesn't send a request right away.
async fn handle_trigger(stream: TriggerRequestStream) {
    let (stream, stalled) = detect_stall::until_stalled(stream, STALL_INTERVAL);
    let mut stream = pin!(stream);
    while let Ok(Some(request)) = stream.try_next().await {
        info!("Received fidl.test.components.TriggerRequest");
        match request {
            fidl_fidl_test_components::TriggerRequest::Run { responder } => {
                responder.send(&format!("hello from {STATIC_TRIGGER_PROTOCOL}")).unwrap()
            }
        }
    }
    if let Ok(Some(server_end)) = stalled.await {
        // Send the server endpoint back to the framework.
        info!("Escrowing {STATIC_TRIGGER_PROTOCOL}");
        fuchsia_component::client::connect_channel_to_protocol_at_path(
            server_end.into(),
            &format!("/escrow/{STATIC_TRIGGER_PROTOCOL}"),
        )
        .unwrap();
    }
}
