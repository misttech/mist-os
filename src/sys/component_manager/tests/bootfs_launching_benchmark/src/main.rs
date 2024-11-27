// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{self, DiscoverableProtocolMarker};
use fsandbox::DictionaryRef;
use fuchsia_component::client;
use fuchsia_component_test::ScopedInstance;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::stream::FuturesUnordered;
use futures::{StreamExt, TryStreamExt};
use std::fs::File;
use std::io::Read;
use std::time::Duration;
use zx::{self as zx, HandleBased};
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

static TEST_CASES: [usize; 6] = [1, 5, 10, 15, 20, 25];

/// This test starts different numbers of ELF components specified in
/// [`TEST_CASES`], each of which links some dynamic libraries, then waits for
/// all of them to call back. It is intended to measure the latency from
/// component_manager to hitting `main()` in a number of ELF components.
#[fuchsia::main]
fn main() {
    // Initialize benchmark.
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut criterion::Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(Duration::from_millis(1000))
        .measurement_time(Duration::from_millis(20000))
        .sample_size(20);

    // Run benchmark.
    for test_case in TEST_CASES {
        let bench = criterion::Benchmark::new(format!("ElfComponent/{test_case:0>2}"), move |b| {
            b.iter_with_setup(|| ElfComponentLaunchTest::new(test_case), |test| test.run());
        });
        c.bench("fuchsia.bootfs.launching", bench);
    }
}

struct ElfComponentLaunchTest {
    numbered_handles: Vec<fprocess::HandleInfo>,
    receiver_task: fasync::Task<PendingResponses>,
    dict_ref: DictionaryRef,
    instance: ScopedInstance,
    executor: fasync::LocalExecutor,
}

struct FinishedElfComponentLaunchTest {
    pending_responses: Option<PendingResponses>,
    instance: Option<ScopedInstance>,
    executor: fasync::LocalExecutor,
}

impl ElfComponentLaunchTest {
    fn new(number_of_echo_connections: usize) -> Self {
        let mut executor = fasync::LocalExecutor::new();

        let vmo = read_file_to_vmo(ZBI_PATH);
        let numbered_handles = vec![fprocess::HandleInfo {
            handle: vmo.into_handle(),
            id: HandleInfo::from(HandleType::BootfsVmo).as_raw(),
        }];
        let cm_url = format!("#meta/component_manager_{number_of_echo_connections}.cm");
        let instance = executor.run_singlethreaded(async move {
            ScopedInstance::new("coll".into(), cm_url).await.unwrap()
        });

        let (dict_ref, echo_receiver_stream) = executor.run_singlethreaded(build_echo_dictionary());

        let receiver_task = fasync::Task::local(async move {
            handle_receiver(echo_receiver_stream, number_of_echo_connections).await
        });

        Self { numbered_handles, receiver_task, dict_ref, instance, executor }
    }

    fn run(mut self) -> FinishedElfComponentLaunchTest {
        // Start component_manager.
        let args = fcomponent::StartChildArgs {
            numbered_handles: Some(self.numbered_handles),
            dictionary: Some(self.dict_ref),
            ..Default::default()
        };

        let (instance, pending_responses) = self.executor.run_singlethreaded(async move {
            let _cm_controller = self.instance.start_with_args(args).await.unwrap();

            // Wait for test ELF component run by component_manager to report back.
            let pending_responses = self.receiver_task.await;

            (self.instance, pending_responses)
        });

        FinishedElfComponentLaunchTest {
            pending_responses: Some(pending_responses),
            instance: Some(instance),
            executor: self.executor,
        }
    }
}

/// Time taking during the drop won't be counted towards the benchmark metrics.
/// We can wait until the realm is cleaned up here.
impl Drop for FinishedElfComponentLaunchTest {
    fn drop(&mut self) {
        let FinishedElfComponentLaunchTest { pending_responses, instance, executor } = self;
        executor.run_singlethreaded(async move {
            drop(pending_responses.take());
            let destruction_waiter = instance.as_mut().unwrap().take_destroy_waiter();
            drop(instance.take());
            destruction_waiter.await.unwrap();
        });
    }
}

/// Build a dictionary with an echo protocol capability and return the receiver stream
/// to handle connection requests.
async fn build_echo_dictionary() -> (fsandbox::DictionaryRef, fsandbox::ReceiverRequestStream) {
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let svc_dict_id = 1;
    store.dictionary_create(svc_dict_id).await.unwrap().unwrap();

    let (echo_receiver_client, echo_receiver_stream) =
        endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
    let connector_id = 100;
    store.connector_create(connector_id, echo_receiver_client).await.unwrap().unwrap();
    store
        .dictionary_insert(
            svc_dict_id,
            &fsandbox::DictionaryItem {
                key: fecho::EchoMarker::PROTOCOL_NAME.into(),
                value: connector_id,
            },
        )
        .await
        .unwrap()
        .unwrap();

    let dict_id = 2;
    store.dictionary_create(dict_id).await.unwrap().unwrap();
    store
        .dictionary_insert(
            dict_id,
            &fsandbox::DictionaryItem { key: "svc".into(), value: svc_dict_id },
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

struct PendingResponses {
    values: Vec<(fecho::EchoEchoStringResponder, String)>,
}

impl Drop for PendingResponses {
    fn drop(&mut self) {
        // Respond to them all, causing the test ELF components to exit.
        let values = std::mem::replace(&mut self.values, Default::default());
        for (responder, response) in values {
            responder.send(Some(&response)).unwrap();
        }
    }
}

/// Serve echo connection requests.
///
/// number_of_echo_connections: How many Echo protocol connection do we expect to wait for.
///
async fn handle_receiver(
    receiver_stream: fsandbox::ReceiverRequestStream,
    number_of_echo_connections: usize,
) -> PendingResponses {
    // Read `number_of_echo_connections` echo connection requests while handling them concurrently.
    let mut receiver_stream = receiver_stream.take(number_of_echo_connections);
    let futures = FuturesUnordered::new();
    while let Some(request) = receiver_stream.try_next().await.unwrap() {
        match request {
            fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                let server_end = endpoints::ServerEnd::<fecho::EchoMarker>::new(channel.into());
                let stream = server_end.into_stream();
                futures.push(fasync::Task::spawn(run_echo_service(stream)));
            }
            fsandbox::ReceiverRequest::_UnknownMethod { .. } => unimplemented!(),
        }
    }

    // Wait to receive the first request in those connections.
    let values: Vec<_> = futures.take(number_of_echo_connections).collect().await;
    PendingResponses { values }
}

async fn run_echo_service(
    mut stream: fecho::EchoRequestStream,
) -> (fecho::EchoEchoStringResponder, String) {
    let message = stream.try_next().await.unwrap().unwrap();
    let fecho::EchoRequest::EchoString { value, responder } = message;
    (responder, value.unwrap())
}
