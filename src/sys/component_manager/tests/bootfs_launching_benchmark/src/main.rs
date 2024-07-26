// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{self, DiscoverableProtocolMarker};
use fuchsia_component::client;
use fuchsia_component_test::ScopedInstance;
use fuchsia_runtime::{HandleInfo, HandleType};
use fuchsia_zircon::{self as zx, HandleBased};
use futures::stream::FuturesUnordered;
use futures::{StreamExt, TryStreamExt};
use std::fs::File;
use std::io::Read;
use {
    fidl_fidl_examples_routing_echo as fecho, fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_process as fprocess,
    fuchsia_async as fasync,
};

const ZBI_PATH: &str = "/pkg/data/tests/uncompressed_bootfs";

fn read_file_to_vmo(path: &str) -> zx::Vmo {
    let mut file_buffer = Vec::new();
    File::open(path).and_then(|mut f| f.read_to_end(&mut file_buffer)).unwrap();
    let vmo = zx::Vmo::create(file_buffer.len() as u64).unwrap();
    vmo.write(&file_buffer, 0).unwrap();
    vmo
}

/// This test starts [`NUMBER_OF_ECHO_CONNECTIONS`] ELF components, each of which
/// links some dynamic libraries, then waits for all of them to call back. It is
/// intended to measure the latency from component_manager to hitting `main()` in
/// a number of ELF components.
#[fuchsia::test]
async fn launch_elf_component() {
    let vmo = read_file_to_vmo(ZBI_PATH);
    let numbered_handles = vec![fprocess::HandleInfo {
        handle: vmo.into_handle(),
        id: HandleInfo::from(HandleType::BootfsVmo).as_raw(),
    }];
    let instance =
        ScopedInstance::new("coll".into(), "#meta/component_manager.cm".into()).await.unwrap();

    let (dict_ref, echo_receiver_stream) = build_echo_dictionary().await;

    let receiver_task =
        fasync::Task::local(async move { handle_receiver(echo_receiver_stream).await });

    // Start component_manager.
    // TODO(https://fxbug.dev/355009003): Track start time here.
    let args = fcomponent::StartChildArgs {
        numbered_handles: Some(numbered_handles),
        dictionary: Some(dict_ref),
        ..Default::default()
    };
    let _cm_controller = instance.start_with_args(args).await.unwrap();

    // Wait for test ELF component run by component_manager to report back.
    receiver_task.await;
}

/// Build a dictionary with an echo protocol capability and return the receiver stream
/// to handle connection requests.
async fn build_echo_dictionary() -> (fsandbox::DictionaryRef, fsandbox::ReceiverRequestStream) {
    let factory = client::connect_to_protocol::<fsandbox::FactoryMarker>().unwrap();
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let dict_id = 1;
    store.dictionary_create(dict_id).await.unwrap().unwrap();

    let (echo_receiver_client, echo_receiver_stream) =
        endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
    let echo_connector = factory.create_connector(echo_receiver_client).await.unwrap();
    let value = 100;
    store.import(value, fsandbox::Capability::Connector(echo_connector)).await.unwrap().unwrap();
    store
        .dictionary_insert(
            dict_id,
            &fsandbox::DictionaryItem { key: fecho::EchoMarker::PROTOCOL_NAME.into(), value },
        )
        .await
        .unwrap()
        .unwrap();
    let dict_ref = match store.export(dict_id).await.unwrap().unwrap() {
        fsandbox::Capability::Dictionary(dict_ref) => dict_ref,
        cap @ _ => panic!("Unexpected {cap:?}"),
    };

    (dict_ref, echo_receiver_stream)
}

/// How many Echo protocol connection do we expect.
/// TODO(https://fxbug.dev/355009003): Run more than one ELF test component.
const NUMBER_OF_ECHO_CONNECTIONS: usize = 1;

async fn handle_receiver(receiver_stream: fsandbox::ReceiverRequestStream) {
    // Read `NUMBER_OF_ECHO_CONNECTIONS` echo connection requests while handling them concurrently.
    let mut receiver_stream = receiver_stream.take(NUMBER_OF_ECHO_CONNECTIONS);
    let futures = FuturesUnordered::new();
    while let Some(request) = receiver_stream.try_next().await.unwrap() {
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                let server_end = endpoints::ServerEnd::<fecho::EchoMarker>::new(channel.into());
                let stream = server_end.into_stream().unwrap();
                futures.push(fasync::Task::spawn(run_echo_service(stream)));
            }
            fsandbox::ReceiverRequest::_UnknownMethod { .. } => unimplemented!(),
        }
    }

    // Wait to receive the first request in those connections.
    let values: Vec<_> = futures.take(NUMBER_OF_ECHO_CONNECTIONS).collect().await;

    // TODO(https://fxbug.dev/355009003): Track end time here.

    // Respond to them all, causing the test ELF components to exit.
    for (responder, response) in values {
        responder.send(Some(&response)).unwrap();
    }
}

async fn run_echo_service(
    mut stream: fecho::EchoRequestStream,
) -> (fecho::EchoEchoStringResponder, String) {
    let message = stream.try_next().await.unwrap().unwrap();
    let fecho::EchoRequest::EchoString { value, responder } = message;
    (responder, value.unwrap())
}
