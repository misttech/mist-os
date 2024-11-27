// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{RequestStream, ServerEnd};
use fidl_fidl_test_components::{TriggerMarker, TriggerRequestStream};
use fuchsia_component::client;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::{StreamExt, TryStreamExt};
use std::pin::pin;
use {
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_process_lifecycle as flifecycle,
    fuchsia_async as fasync,
};

/// See the `stop_with_escrowed_dictionary` test case.
///
/// This program stores some state in the escrow request, to be read back the
/// next time it is started. In particular, it stores an increasing counter of
/// the number of `TriggerRequest.Run` calls.
#[fuchsia::main]
pub async fn main() {
    struct Trigger(TriggerRequestStream);
    let dict_store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let mut dict_id = 1;

    // If there is no `EscrowedDictionary` processargs, initialize the counter to 0.
    let counter = match fuchsia_runtime::take_startup_handle(HandleInfo::new(
        HandleType::EscrowedDictionary,
        0,
    )) {
        Some(dictionary) => {
            let dictionary = fsandbox::Capability::Dictionary(fsandbox::DictionaryRef {
                token: dictionary.into(),
            });
            dict_store.import(dict_id, dictionary).await.unwrap().unwrap();
            read_counter_from_dictionary(&dict_store, dict_id).await
        }
        None => 0,
    };

    // Handle exactly one connection request, which is what the test sends.
    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs.dir("svc").add_fidl_service(Trigger);
    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle().unwrap();
    let request = fs.next().await.unwrap();
    let counter = handle_trigger(counter, request.0).await;
    dict_id += 1;
    escrow_counter_then_stop(&dict_store, dict_id, counter).await;
}

async fn read_counter_from_dictionary(
    dict_store: &fsandbox::CapabilityStoreProxy,
    dict_id: u64,
) -> u64 {
    let dest_id = 100;
    dict_store.dictionary_get(dict_id, "counter", dest_id).await.unwrap().unwrap();
    let capability = dict_store.export(dest_id).await.unwrap().unwrap();
    match capability {
        fsandbox::Capability::Data(data) => match data {
            fsandbox::Data::Uint64(counter) => counter,
            data @ _ => panic!("unexpected {data:?}"),
        },
        capability @ _ => panic!("unexpected {capability:?}"),
    }
}

async fn handle_trigger(mut counter: u64, stream: TriggerRequestStream) -> u64 {
    let (mut stream, stalled) =
        detect_stall::until_stalled(stream, fasync::MonotonicDuration::from_micros(1));
    let mut stream = pin!(stream);
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

async fn escrow_counter_then_stop(
    store: &fsandbox::CapabilityStoreProxy,
    dict_id: u64,
    counter: u64,
) {
    // Create a new dictionary.
    store.dictionary_create(dict_id).await.unwrap().unwrap();

    // Add the counter into the dictionary.
    let value = 10;
    store
        .import(value, fsandbox::Capability::Data(fsandbox::Data::Uint64(counter)))
        .await
        .unwrap()
        .unwrap();
    store
        .dictionary_insert(dict_id, &fsandbox::DictionaryItem { key: "counter".into(), value })
        .await
        .unwrap()
        .unwrap();

    // Send the dictionary away.
    let fsandbox::Capability::Dictionary(dictionary_ref) =
        store.export(dict_id).await.unwrap().unwrap()
    else {
        unreachable!("capability is not Dictionary?");
    };
    let lifecycle =
        fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::Lifecycle, 0)).unwrap();
    let lifecycle = zx::Channel::from(lifecycle);
    let lifecycle = ServerEnd::<flifecycle::LifecycleMarker>::from(lifecycle);
    lifecycle
        .into_stream()
        .control_handle()
        .send_on_escrow(flifecycle::LifecycleOnEscrowRequest {
            escrowed_dictionary: Some(dictionary_ref),
            ..Default::default()
        })
        .unwrap();
}
