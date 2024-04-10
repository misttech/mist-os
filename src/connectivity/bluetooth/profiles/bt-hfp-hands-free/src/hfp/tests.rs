// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

/// Test the interactions between Hfp and Peer structs, in particular the delay
/// timer for handling search result profile events.
use anyhow::Result;
use async_test_helpers::run_while;
use async_utils::PollExt;
use fidl_fuchsia_bluetooth_bredr as bredr;
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_async as fasync;
use fuchsia_bluetooth::types::{Channel, PeerId};
use futures::channel::mpsc;
use futures::StreamExt;
use profile_client::ProfileClient;
use std::future::Future;
use std::pin::{pin, Pin};
use std::task::Poll;
use test_profile_server::{ConnectChannel, TestProfileServer, TestProfileServerEndpoints};

use crate::config::HandsFreeFeatureSupport;
use crate::hfp::{Hfp, SEARCH_RESULT_CONNECT_DELAY_SECONDS};
use crate::service_definition;

const PEER_ID: PeerId = PeerId(1);

const ZERO_TIME: fasync::Time = fasync::Time::from_nanos(0);

const BEFORE_SEARCH_RESULT_CONNECT_DELAY_TIME: fasync::Time =
    fasync::Time::from_nanos(SEARCH_RESULT_CONNECT_DELAY_SECONDS / 2 * 1_000_000_000);

const AFTER_SEARCH_RESULT_CONNECT_DELAY_TIME: fasync::Time =
    fasync::Time::from_nanos(SEARCH_RESULT_CONNECT_DELAY_SECONDS * 2 * 1_000_000_000);

type HfpRunFuture = Pin<Box<dyn Future<Output = Result<()>>>>;

// Helpers to set up tests ////////////////////////////////////////////////////

fn protocol_list() -> Vec<bredr::ProtocolDescriptor> {
    vec![bredr::ProtocolDescriptor {
        protocol: bredr::ProtocolIdentifier::Rfcomm,
        params: vec![/* Server Channel */ bredr::DataElement::Uint8(1)],
    }]
}

fn hfp_future(
    profile_client: ProfileClient,
    profile_proxy: bredr::ProfileProxy,
) -> (HfpRunFuture, mpsc::Sender<fidl_hfp::HandsFreeRequestStream>) {
    let (fidl_connection_sender, fidl_connection_receiver) = mpsc::channel(0);

    let hfp = Hfp::new(
        HandsFreeFeatureSupport::default(),
        profile_client,
        profile_proxy,
        fidl_connection_receiver,
    );

    (Box::pin(hfp.run()), fidl_connection_sender)
}

fn set_up_server(exec: &mut fasync::TestExecutor, profile_server: &mut TestProfileServer) {
    {
        let expect_advertise_fut = profile_server.expect_advertise();
        let mut expect_advertise_fut = pin!(expect_advertise_fut);
        exec.run_until_stalled(&mut expect_advertise_fut)
            .expect("Pending while expecting advertise.");
    }

    {
        let expect_search_fut = profile_server.expect_search();
        let mut expect_search_fut = pin!(expect_search_fut);
        exec.run_until_stalled(&mut expect_search_fut).expect("Pending while expecting search.");
    }
}

// Helpers to send profile events /////////////////////////////////////////

fn send_peer_connected(profile_server: &mut TestProfileServer) -> Channel {
    let near = profile_server.send_connected(PEER_ID, protocol_list());

    near
}

fn send_search_result(
    exec: &mut fasync::TestExecutor,
    hfp_fut: &mut HfpRunFuture,
    profile_server: &mut TestProfileServer,
) {
    let send_fut = pin!(profile_server.send_service_found(PEER_ID, Some(protocol_list()), vec![]));

    let (result, _hfp_fut) = run_while(exec, hfp_fut, send_fut);
    result.expect("Error sending search result");
}

fn run_hfp(exec: &mut fasync::TestExecutor, hfp_fut: &mut HfpRunFuture) {
    let mut hfp_fut = pin!(hfp_fut);

    let _timers_were_expired = exec.wake_expired_timers();
    exec.run_until_stalled(&mut hfp_fut).expect_pending("Done while running search result");
}

// Helpers to assert that postconditions are true /////////////////////////

fn expect_no_requests(
    exec: &mut fasync::TestExecutor,
    test_profile_server: &mut TestProfileServer,
) {
    let no_requests_fut = test_profile_server.next();
    let mut no_requests_fut = pin!(no_requests_fut);

    let poll = exec.run_until_stalled(&mut no_requests_fut);
    if let Poll::Ready(request) = poll {
        panic!("Request stream had request: {:?}", request)
    };
}

fn expect_connect(
    exec: &mut fasync::TestExecutor,
    test_profile_server: &mut TestProfileServer,
) -> Channel {
    let expect_connect_fut =
        test_profile_server.expect_connect(Some(ConnectChannel::RfcommChannel(1)));
    let mut expect_connect_fut = pin!(expect_connect_fut);

    let near =
        exec.run_until_stalled(&mut expect_connect_fut).expect("Pending while expecting connect");

    near
}

fn expect_channel_closed(exec: &mut fasync::TestExecutor, channel: &Channel) {
    let closed_fut = channel.closed();
    let mut closed_fut = pin!(closed_fut);

    exec.run_until_stalled(&mut closed_fut)
        .expect("Channel not closed")
        .expect("Error checking if channel is open");
}

fn expect_channel_still_open(exec: &mut fasync::TestExecutor, channel: &Channel) {
    let closed_fut = channel.closed();
    let mut closed_fut = pin!(closed_fut);

    exec.run_until_stalled(&mut closed_fut).expect_pending("Channel closed");
}

// Tests //////////////////////////////////////////////////////////////////

// TODO(https://fxbug.deb/127364) Make sure to hold on to the PeerHandlerProxy once the peer is sending it to
// prevent the stream from closing and the test failing.
#[fuchsia::test]
fn hfp_connected_starts_task() {
    let mut exec = fasync::TestExecutor::new();

    let service_definition = service_definition::hands_free(HandsFreeFeatureSupport::default());
    let profile_id = bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway;
    let TestProfileServerEndpoints {
        proxy: profile_proxy,
        client: profile_client,
        test_server: mut profile_server,
    } = TestProfileServer::new(Some(service_definition), Some(profile_id));

    let (mut hfp_fut, _fidl_connection_sender) = hfp_future(profile_client, profile_proxy);
    set_up_server(&mut exec, &mut profile_server);

    // Send PeerConnectedEvent to hfp.
    let near = send_peer_connected(&mut profile_server);

    // Let HFP handle the connection.
    run_hfp(&mut exec, &mut hfp_fut);

    // The peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near);
}

#[fuchsia::test]
fn search_result_connects_channel_and_starts_task_after_delay() {
    let mut exec = fasync::TestExecutor::new_with_fake_time();
    exec.set_fake_time(ZERO_TIME);

    let service_definition = service_definition::hands_free(HandsFreeFeatureSupport::default());
    let profile_id = bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway;
    let TestProfileServerEndpoints {
        proxy: profile_proxy,
        client: profile_client,
        test_server: mut profile_server,
    } = TestProfileServer::new(Some(service_definition), Some(profile_id));

    let (mut hfp_fut, _fidl_connection_sender) = hfp_future(profile_client, profile_proxy);
    set_up_server(&mut exec, &mut profile_server);

    // Send a search result event to the peer.
    send_search_result(&mut exec, &mut hfp_fut, &mut profile_server);

    // Wait a little bit of time, but don't let the delay timer expire.
    exec.set_fake_time(BEFORE_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // Before the delay, the peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // Expire delay in handling search results.
    exec.set_fake_time(AFTER_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // After the delay, the peer should connect a channel.
    let near = expect_connect(&mut exec, &mut profile_server);
    run_hfp(&mut exec, &mut hfp_fut);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near);
}

#[fuchsia::test]
fn peer_connected_then_search_result_starts_task_and_does_not_connect() {
    let mut exec = fasync::TestExecutor::new_with_fake_time();
    exec.set_fake_time(ZERO_TIME);

    let service_definition = service_definition::hands_free(HandsFreeFeatureSupport::default());
    let profile_id = bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway;
    let TestProfileServerEndpoints {
        proxy: profile_proxy,
        client: profile_client,
        test_server: mut profile_server,
    } = TestProfileServer::new(Some(service_definition), Some(profile_id));

    let (mut hfp_fut, _fidl_connection_sender) = hfp_future(profile_client, profile_proxy);
    set_up_server(&mut exec, &mut profile_server);

    // Send PeerConnectedEvent to peer.
    let near = send_peer_connected(&mut profile_server);

    // The peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near);

    // Send a search result event to the peer.
    send_search_result(&mut exec, &mut hfp_fut, &mut profile_server);

    // Expire delay in handling search results.
    exec.set_fake_time(AFTER_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // Even after the delay, the peer should not connect a channel.
    expect_no_requests(&mut exec, &mut profile_server);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near);
}

#[fuchsia::test]
fn search_result_then_peer_connected_before_delay_starts_task_and_does_not_connect() {
    let mut exec = fasync::TestExecutor::new_with_fake_time();
    exec.set_fake_time(ZERO_TIME);

    let service_definition = service_definition::hands_free(HandsFreeFeatureSupport::default());
    let profile_id = bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway;
    let TestProfileServerEndpoints {
        proxy: profile_proxy,
        client: profile_client,
        test_server: mut profile_server,
    } = TestProfileServer::new(Some(service_definition), Some(profile_id));

    let (mut hfp_fut, _fidl_connection_sender) = hfp_future(profile_client, profile_proxy);
    set_up_server(&mut exec, &mut profile_server);

    // Send a search result event to the peer.
    send_search_result(&mut exec, &mut hfp_fut, &mut profile_server);

    // Wait a little bit of time, but don't let the delay timer expire.
    exec.set_fake_time(BEFORE_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // Before the delay, the peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // Send PeerConnectedEvent to peer.
    let near = send_peer_connected(&mut profile_server);

    // Handle the peer connected event.
    run_hfp(&mut exec, &mut hfp_fut);

    // The peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near);

    // Expire delay in handling search results.
    exec.set_fake_time(AFTER_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // Even after the delay, the peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near);
}

#[fuchsia::test]
fn search_result_then_peer_connected_after_delay_starts_task_and_connects() {
    let mut exec = fasync::TestExecutor::new_with_fake_time();
    exec.set_fake_time(ZERO_TIME);

    let service_definition = service_definition::hands_free(HandsFreeFeatureSupport::default());
    let profile_id = bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway;
    let TestProfileServerEndpoints {
        proxy: profile_proxy,
        client: profile_client,
        test_server: mut profile_server,
    } = TestProfileServer::new(Some(service_definition), Some(profile_id));

    let (mut hfp_fut, _fidl_connection_sender) = hfp_future(profile_client, profile_proxy);
    set_up_server(&mut exec, &mut profile_server);

    // Send a search result event to the peer.
    send_search_result(&mut exec, &mut hfp_fut, &mut profile_server);

    // Wait a little bit of time, but don't let the delay timer expire.
    exec.set_fake_time(BEFORE_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // Before the delay, the peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // Expire delay in handling search results.
    exec.set_fake_time(AFTER_SEARCH_RESULT_CONNECT_DELAY_TIME);
    run_hfp(&mut exec, &mut hfp_fut);

    // After the delay, the peer should connect a channel.
    let near_received = expect_connect(&mut exec, &mut profile_server);
    run_hfp(&mut exec, &mut hfp_fut);

    // Send PeerConnectedEvent to peer.
    let near_sent = send_peer_connected(&mut profile_server);

    // Handle the peer connected event.
    run_hfp(&mut exec, &mut hfp_fut);

    // The peer should send no requests.
    expect_no_requests(&mut exec, &mut profile_server);

    // The task should have dropped the channel it sent us.
    expect_channel_closed(&mut exec, &near_received);

    // And the task should not have exited,
    expect_channel_still_open(&mut exec, &near_sent);
}
