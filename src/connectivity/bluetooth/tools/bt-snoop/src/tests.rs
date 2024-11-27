// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use async_utils::PollExt;
use diagnostics_assertions::assert_data_tree;
use fidl::endpoints::RequestStream;
use fidl::Error as FidlError;
use fidl_fuchsia_bluetooth_snoop::{
    PacketFormat, PacketObserverMarker, PacketObserverRequestStream, SnoopMarker, SnoopProxy,
    SnoopRequestStream, SnoopStartRequest,
};
use fuchsia_async::{Channel, TestExecutor};
use fuchsia_inspect::Inspector;

use futures::StreamExt;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use crate::packet_logs::{append_pcap, write_pcap_header};
use crate::{
    handle_client_request, register_new_client, Args, ClientId, ClientRequest,
    ConcurrentClientRequestFutures, ConcurrentSnooperPacketFutures, IdGenerator, PacketLogs,
    SnoopConfig, SnoopPacket, SubscriptionManager, HCI_DEVICE_CLASS_PATH,
};

fn setup() -> (
    TestExecutor,
    ConcurrentSnooperPacketFutures,
    PacketLogs,
    SubscriptionManager,
    ConcurrentClientRequestFutures,
    Inspector,
) {
    let inspect = Inspector::default();
    (
        TestExecutor::new(),
        ConcurrentSnooperPacketFutures::new(),
        PacketLogs::new(
            10,
            10,
            10,
            Duration::new(10, 0),
            inspect.root().create_child("packet_log"),
        ),
        SubscriptionManager::new(),
        ConcurrentClientRequestFutures::new(),
        inspect,
    )
}

#[fuchsia::test]
fn test_id_generator() {
    let mut id_gen = IdGenerator::new();
    assert_eq!(id_gen.next(), ClientId(0));
    assert_eq!(id_gen.next(), ClientId(1));
}

#[fuchsia::test]
fn test_register_new_client() {
    let (_exec, _snoopers, _logs, _subscribers, mut requests, _inspect) = setup();
    assert_eq!(requests.len(), 0);

    let (_tx, rx) = zx::Channel::create();
    let stream = SnoopRequestStream::from_channel(Channel::from_channel(rx));
    register_new_client(stream, &mut requests, ClientId(0));
    assert_eq!(requests.len(), 1);
}

fn fidl_endpoints() -> (SnoopProxy, SnoopRequestStream) {
    fidl::endpoints::create_proxy_and_stream::<SnoopMarker>().unwrap()
}

fn client_request(host_device: Option<String>) -> (SnoopStartRequest, PacketObserverRequestStream) {
    let (client, stream) = fidl::endpoints::create_request_stream::<PacketObserverMarker>();
    (
        SnoopStartRequest {
            follow: Some(true),
            host_device,
            client: Some(client),
            ..Default::default()
        },
        stream,
    )
}

fn unwrap_request<T, E>(request: Poll<Option<Result<T, E>>>) -> T {
    if let Poll::Ready(Some(Ok(request))) = request {
        return request;
    }
    panic!("Failed to receive request");
}

#[fuchsia::test]
fn test_snoop_default_command_line_args() {
    let args = Args::from_args(&["bt-snoop-v2"], &[]).expect("Args created from empty args");
    assert_eq!(args.log_size_soft_kib, 32);
    assert_eq!(args.log_size_hard_kib, 256);
    assert_eq!(args.log_time_seconds, 60);
    assert_eq!(args.max_device_count, 8);
    assert_eq!(args.truncate_payload, None);
}

#[fuchsia::test]
fn test_snoop_command_line_args() {
    let log_size_kib = 1;
    let log_time_seconds = 2;
    let max_device_count = 3;
    let truncate_payload = 4;
    let raw_args = &[
        "--log-size-soft-kib",
        &log_size_kib.to_string(),
        "--log-size-hard-kib",
        &log_size_kib.to_string(),
        "--log-time-seconds",
        &log_time_seconds.to_string(),
        "--max-device-count",
        &max_device_count.to_string(),
        "--truncate-payload",
        &truncate_payload.to_string(),
    ];
    let args = Args::from_args(&["bt-snoop-v2"], raw_args).expect("Args created from args");
    assert_eq!(args.log_size_soft_kib, log_size_kib);
    assert_eq!(args.log_size_hard_kib, log_size_kib);
    assert_eq!(args.log_time_seconds, log_time_seconds);
    assert_eq!(args.max_device_count, max_device_count);
    assert_eq!(args.truncate_payload, Some(truncate_payload));
}

#[fuchsia::test]
async fn test_packet_logs_inspect() {
    // This is a test that basic inspect data is plumbed through from the inspect root.
    // More comprehensive testing of possible permutations of packet log inspect data
    // is found in bounded_queue.rs
    let inspect = Inspector::default();
    let runtime_metrics_node = inspect.root().create_child("runtime_metrics");
    let mut packet_logs =
        PacketLogs::new(2, 256, 256, Duration::from_secs(60), runtime_metrics_node);

    assert_data_tree!(inspect, root: {
        runtime_metrics: {
            logging_active_for_devices: "",
        }
    });

    let id_1 = String::from("001");

    assert!(packet_logs.add_device(id_1.clone()).is_none(), "shouldn't have evicted a log");

    let mut expected_data = vec![];
    write_pcap_header(&mut expected_data).expect("write to succeed");

    assert_data_tree!(inspect, root: {
        runtime_metrics: {
            logging_active_for_devices: "\"001\"",
            device_0: {
                hci_device_name: "001",
                byte_len: 0u64,
                number_of_items: 0u64,
                data: expected_data,
            },
        }
    });

    let ts = zx::MonotonicInstant::from_nanos(123 * 1_000_000_000);
    let packet = SnoopPacket::new(false, PacketFormat::AclData, ts, vec![3, 2, 1]);

    // write pcap header and packet data to expected_data buffer
    let mut expected_data = vec![];
    write_pcap_header(&mut expected_data).expect("write to succeed");
    append_pcap(&mut expected_data, &packet).expect("write to succeed");

    packet_logs.log_packet(&id_1, packet);

    assert_data_tree!(inspect, root: {
        runtime_metrics: {
            logging_active_for_devices: "\"001\"",
            device_0: {
                hci_device_name: "001",
                byte_len: 59u64,
                number_of_items: 1u64,
                data: expected_data,
            },
        }
    });

    drop(packet_logs);
}

#[fuchsia::test]
fn test_snoop_config_inspect() {
    let args = Args {
        log_size_soft_kib: 1,
        log_size_hard_kib: 1,
        log_time_seconds: 2,
        max_device_count: 3,
        truncate_payload: Some(4),
    };
    let inspect = Inspector::default();
    let snoop_config_node = inspect.root().create_child("configuration");
    let config = SnoopConfig::from_args(args, snoop_config_node);
    assert_data_tree!(inspect, root: {
        configuration: {
            log_size_soft_max_bytes: 1024u64,
            log_size_hard_max_bytes: "1024",
            log_time: 2u64,
            max_device_count: 3u64,
            truncate_payload: "4 bytes",
            hci_dir: HCI_DEVICE_CLASS_PATH,
        }
    });
    drop(config);
}

// Helper that pumps the request stream to get back a single request, panicking if the stream
// stalls before a request is returned.
fn pump_request_stream(
    exec: &mut TestExecutor,
    request_stream: &mut SnoopRequestStream,
    id: ClientId,
) -> ClientRequest {
    let request = unwrap_request(exec.run_until_stalled(&mut request_stream.next()));
    (id, Some(Ok(request)))
}

// Helper that pumps the the handle_client_request until stalled, panicking if the future
// stalls in a pending state or returns an error.
fn pump_handle_client_request(
    exec: &mut TestExecutor,
    request: ClientRequest,
    request_stream: SnoopRequestStream,
    client_requests: &mut ConcurrentClientRequestFutures,
    subscribers: &mut SubscriptionManager,
    packet_logs: &PacketLogs,
) {
    let client_id = request.0;
    let mut handler = pin!(handle_client_request(request, subscribers, packet_logs));
    if exec
        .run_until_stalled(&mut handler)
        .expect("Handler future to complete")
        .expect("Client channel to accept response")
    {
        register_new_client(request_stream, client_requests, client_id);
    }
}

#[fuchsia::test]
fn test_handle_client_request() {
    let (mut exec, mut _snoopers, mut logs, mut subscribers, mut requests, _inspect) = setup();

    // unrecognized device returns an error to the client
    let (proxy, mut request_stream) = fidl_endpoints();
    let (client_req, mut client_stream1) = client_request(Some(String::from("unrecognized")));
    proxy.start(client_req).unwrap();
    let request = pump_request_stream(&mut exec, &mut request_stream, ClientId(0));
    pump_handle_client_request(
        &mut exec,
        request,
        request_stream,
        &mut requests,
        &mut subscribers,
        &logs,
    );
    let mut item_fut = client_stream1.next();
    let expected_error =
        exec.run_until_stalled(&mut item_fut).expect("ready").expect("some").expect("ok");
    assert!(expected_error.into_error().is_some());
    assert_eq!(subscribers.number_of_subscribers(), 0);
    // client should be closed after error
    let mut item_fut = client_stream1.next();
    let expected_none = exec.run_until_stalled(&mut item_fut).expect("ready");
    assert!(expected_none.is_none());

    // valid device returns no errors to a client subscribed to that device
    let (proxy, mut request_stream) = fidl_endpoints();
    let (client_req, mut client_stream2) = client_request(Some(String::from("")));
    assert!(logs.add_device(String::new()).is_none(), "shouldn't have evicted a device log");
    proxy.start(client_req).unwrap();
    let request = pump_request_stream(&mut exec, &mut request_stream, ClientId(1));
    pump_handle_client_request(
        &mut exec,
        request,
        request_stream,
        &mut requests,
        &mut subscribers,
        &logs,
    );
    let mut item_fut = client_stream2.next();
    exec.run_until_stalled(&mut item_fut).expect_pending("held open");
    assert_eq!(subscribers.number_of_subscribers(), 1);

    // valid device returns no errors to a client subscribed globally
    let (proxy, mut request_stream) = fidl_endpoints();
    let (client_req, mut client_stream3) = client_request(None);
    proxy.start(client_req).unwrap();
    let request = pump_request_stream(&mut exec, &mut request_stream, ClientId(2));
    pump_handle_client_request(
        &mut exec,
        request,
        request_stream,
        &mut requests,
        &mut subscribers,
        &logs,
    );
    let mut item_fut = client_stream3.next();
    exec.run_until_stalled(&mut item_fut).expect_pending("held open");
    assert_eq!(subscribers.number_of_subscribers(), 2);

    // second request by the same client also works.
    let (proxy, mut request_stream) = fidl_endpoints();
    let (client_req, mut client_stream4) = client_request(None);
    proxy.start(client_req).unwrap();
    let request = pump_request_stream(&mut exec, &mut request_stream, ClientId(2));
    pump_handle_client_request(
        &mut exec,
        request,
        request_stream,
        &mut requests,
        &mut subscribers,
        &logs,
    );
    let mut item_fut = client_stream4.next();
    exec.run_until_stalled(&mut item_fut).expect_pending("held open");
    assert_eq!(subscribers.number_of_subscribers(), 3);

    // valid device returns no errors to a client requesting a dump
    let (proxy, mut request_stream) = fidl_endpoints();
    let (client, mut client_stream5) =
        fidl::endpoints::create_request_stream::<PacketObserverMarker>();
    let client_req = SnoopStartRequest {
        host_device: Some(String::from("")),
        client: Some(client),
        ..Default::default()
    };
    proxy.start(client_req).unwrap();
    let request = pump_request_stream(&mut exec, &mut request_stream, ClientId(2));
    pump_handle_client_request(
        &mut exec,
        request,
        request_stream,
        &mut requests,
        &mut subscribers,
        &logs,
    );
    let mut item_fut = client_stream5.next();
    // Should be no error, but since there's no logs, closes immediately.
    let expected_none = exec.run_until_stalled(&mut item_fut).expect("ready");
    assert!(expected_none.is_none());
    assert_eq!(subscribers.number_of_subscribers(), 3);
}

#[fuchsia::test]
fn test_handle_bad_client_request() {
    let (mut exec, mut _snoopers, logs, mut subscribers, mut requests, _inspect) = setup();

    let id = ClientId(0);
    let err = Some(Err(FidlError::Invalid));
    let (client, _client_req_stream) = fidl::endpoints::create_proxy::<PacketObserverMarker>();
    let request = (id, err);
    let (_, stream) = fidl::endpoints::create_request_stream::<SnoopMarker>();
    subscribers.register(id, client, None).unwrap();
    assert!(subscribers.is_registered(&id));
    pump_handle_client_request(&mut exec, request, stream, &mut requests, &mut subscribers, &logs);
    assert!(!subscribers.is_registered(&id));

    let id = ClientId(1);
    let err = Some(Err(FidlError::Invalid));
    let (client, _client_req_stream) = fidl::endpoints::create_proxy::<PacketObserverMarker>();
    let request = (id, err);
    subscribers.register(id, client, None).unwrap();
    assert!(subscribers.is_registered(&id));
    let (_, stream) = fidl::endpoints::create_request_stream::<SnoopMarker>();
    pump_handle_client_request(&mut exec, request, stream, &mut requests, &mut subscribers, &logs);
    assert!(!subscribers.is_registered(&id));
}
