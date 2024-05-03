// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{ClientEnd, Proxy, RequestStream, ServerEnd};
use fidl_fidl_test_components::{TriggerMarker, TriggerRequestStream};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_process_lifecycle as flifecycle;
use fuchsia_async as fasync;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_runtime::{HandleInfo, HandleType};
use fuchsia_zircon as zx;
use futures::{StreamExt, TryStreamExt};

/// See the `stop_with_escrowed_dictionary` test case.
///
/// This program stores some state in the escrow request, to be read back the
/// next time it is started. In particular, it stores an increasing counter of
/// the number of `TriggerRequest.Run` calls.
#[fuchsia::main]
pub async fn main() {
    struct Trigger(TriggerRequestStream);

    // If there is no `EscrowedDictionary` processargs, initialize the counter to 0.
    let counter = match fuchsia_runtime::take_startup_handle(HandleInfo::new(
        HandleType::EscrowedDictionary,
        0,
    )) {
        Some(dictionary) => {
            read_counter_from_dictionary(zx::Channel::from(dictionary).into()).await
        }
        None => 0,
    };

    // Handle exactly one connection request, which is what the test sends.
    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs.dir("svc").add_fidl_service(Trigger);
    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle().unwrap();
    let request = fs.next().await.unwrap();
    let counter = handle_trigger(counter, request.0).await;
    escrow_counter_then_stop(counter).await;
}

async fn read_counter_from_dictionary(dictionary: ClientEnd<fsandbox::DictionaryMarker>) -> u64 {
    let dictionary = dictionary.into_proxy().unwrap();
    let capability = dictionary.get("counter").await.unwrap().unwrap();
    match capability {
        fsandbox::Capability::Data(data) => match data {
            fsandbox::DataCapability::Uint64(counter) => counter,
            data @ _ => panic!("unexpected {data:?}"),
        },
        capability @ _ => panic!("unexpected {capability:?}"),
    }
}

async fn handle_trigger(mut counter: u64, stream: TriggerRequestStream) -> u64 {
    let (mut stream, stalled) =
        detect_stall::until_stalled(stream, fasync::Duration::from_micros(1));
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fidl_fidl_test_components::TriggerRequest::Run { responder } => {
                counter += 1;
                responder.send(&format!("{counter}")).unwrap();
            }
        }
    }
    if let Ok(Some(server_end)) = stalled.await {
        // Send the server endpoint back to the framework.
        fuchsia_component::client::connect_channel_to_protocol_at::<TriggerMarker>(
            server_end.into(),
            "/escrow",
        )
        .unwrap();
    }
    counter
}

async fn escrow_counter_then_stop(counter: u64) {
    // Create a new dictionary.
    let factory =
        fuchsia_component::client::connect_to_protocol::<fsandbox::FactoryMarker>().unwrap();
    let dictionary = factory.create_dictionary().await.unwrap();
    let dictionary = dictionary.into_proxy().unwrap();

    // Add the counter into the dictionary.
    dictionary
        .insert("counter", fsandbox::Capability::Data(fsandbox::DataCapability::Uint64(counter)))
        .await
        .unwrap()
        .unwrap();

    // Send the dictionary away.
    let lifecycle =
        fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::Lifecycle, 0)).unwrap();
    let lifecycle = zx::Channel::from(lifecycle);
    let lifecycle = ServerEnd::<flifecycle::LifecycleMarker>::from(lifecycle);
    lifecycle
        .into_stream()
        .unwrap()
        .control_handle()
        .send_on_escrow(flifecycle::LifecycleOnEscrowRequest {
            escrowed_dictionary: Some(dictionary.into_channel().unwrap().into_zx_channel().into()),
            ..Default::default()
        })
        .unwrap();
}
