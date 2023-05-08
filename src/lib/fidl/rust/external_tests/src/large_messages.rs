// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file tests the public APIs of FIDL large message support

use {
    assert_matches::assert_matches,
    fidl::encoding::MAX_HANDLES,
    fidl::Event,
    fidl::{endpoints::ServerEnd, Channel},
    fidl_test_external::{
        LargeMessageTable, LargeMessageUnion, LargeMessageUnionUnknown, OverflowingProtocolEvent,
        OverflowingProtocolMarker, OverflowingProtocolProxy, OverflowingProtocolRequest,
        OverflowingProtocolSynchronousProxy, OverflowingProtocolTwoWayRequestOnlyResponse,
        OverflowingProtocolTwoWayResponseOnlyRequest,
    },
    fuchsia_async as fasync,
    fuchsia_zircon::{Duration, Time},
    futures::StreamExt,
    std::future::Future,
};

/// A very big string that is definitely larger that 64KiB. Including it in a payload will
/// necessarily make that payload into a large message.
const LARGE_STR: &'static str = include_str!("./large_string.txt");

/// A tiny string that ensures that any layout containing only it as a member will be smaller than
/// 64KiB, thereby guaranteeing that any payload using that layout is not a large message.
const SMALL_STR: &'static str = "I'm a very small string.";

/// The time to wait for the triggered event on sync clients.
const WAIT_FOR_EVENT: Duration = Duration::from_seconds(1);

/// For tests sending `LargeMessageResourced`, this function builds the correct number of present handles.
fn build_handle_array(present: u64) -> [Option<fidl::Handle>; MAX_HANDLES] {
    std::array::from_fn(|i| {
        if i < present.try_into().unwrap() {
            Some(Event::create().into())
        } else {
            None
        }
    })
}

/// For tests reading `LargeMessageResourced`, this function validates the correct number of present handles.
fn validate_handle_array(handles: &[Option<fidl::Handle>; MAX_HANDLES], present: u64) {
    for n in 0..MAX_HANDLES {
        if n < present.try_into().unwrap() {
            assert_matches!(handles[n], Some(_));
        } else {
            assert_matches!(handles[n], None);
        }
    }
}

fn server_runner(mut expected_str: &'static str, server_end: Channel) {
    fasync::LocalExecutor::new().run_singlethreaded(async move {
        let mut stream =
            ServerEnd::<OverflowingProtocolMarker>::new(server_end).into_stream().unwrap();
        if let Some(request) = stream.next().await {
            match request {
                Ok(OverflowingProtocolRequest::TwoWayRequestOnly { payload, responder }) => {
                    match payload {
                        LargeMessageUnion::Str(str) => {
                            assert_eq!(&str, &expected_str);

                            responder
                                .send(OverflowingProtocolTwoWayRequestOnlyResponse::default())
                                .unwrap();
                        }
                        LargeMessageUnionUnknown!() => panic!("unknown data"),
                    }
                }
                Ok(OverflowingProtocolRequest::TwoWayResponseOnly { responder, .. }) => {
                    responder
                        .send(LargeMessageTable {
                            str: Some(expected_str.to_string()),
                            ..Default::default()
                        })
                        .unwrap();
                }
                Ok(OverflowingProtocolRequest::TwoWayBothRequestAndResponse { str, responder }) => {
                    assert_eq!(&str, expected_str);

                    responder.send(&mut expected_str).unwrap();
                }
                Ok(OverflowingProtocolRequest::OneWayCall { payload, control_handle }) => {
                    assert_eq!(&payload.str.unwrap().as_str(), &expected_str);

                    control_handle
                        .send_on_one_way_reply_event(LargeMessageUnion::Str(
                            expected_str.to_string(),
                        ))
                        .unwrap();
                }
                Ok(OverflowingProtocolRequest::Resourced {
                    str,
                    expect_handles,
                    handles,
                    responder,
                }) => {
                    assert_eq!(&str, &expected_str);
                    validate_handle_array(&handles, expect_handles);

                    responder.send(&str, expect_handles, handles).unwrap();
                }
                Err(_) => panic!("unexpected err"),
            }
        }
    })
}

#[track_caller]
fn run_client_sync<C, R>(expected_str: &'static str, client_runner: C)
where
    C: 'static + FnOnce(OverflowingProtocolSynchronousProxy) -> R + Send,
    R: 'static + Send,
{
    let (client_end, server_end) = Channel::create();
    let client = OverflowingProtocolSynchronousProxy::new(client_end);

    let s = std::thread::spawn(move || server_runner(expected_str, server_end));
    client_runner(client);
    s.join().unwrap();
}

async fn run_client_async<C, F>(expected_str: &'static str, client_runner: C)
where
    C: 'static + FnOnce(OverflowingProtocolProxy) -> F + Send,
    F: Future<Output = ()> + 'static + Send,
{
    let (client_end, server_end) = Channel::create();
    let client_end = fasync::Channel::from_channel(client_end).unwrap();
    let client = OverflowingProtocolProxy::new(client_end);

    let s = std::thread::spawn(move || server_runner(expected_str, server_end));
    client_runner(client).await;
    s.join().unwrap();
}

#[test]
fn overflowing_two_way_request_only_large_sync() {
    run_client_sync(LARGE_STR, |client| {
        client
            .two_way_request_only(LargeMessageUnion::Str(LARGE_STR.to_string()), Time::INFINITE)
            .unwrap();
    })
}

#[test]
fn overflowing_two_way_request_only_small_sync() {
    run_client_sync(SMALL_STR, |client| {
        client
            .two_way_request_only(LargeMessageUnion::Str(SMALL_STR.to_string()), Time::INFINITE)
            .unwrap();
    })
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_two_way_request_only_large_async() {
    run_client_async(LARGE_STR, |client| async move {
        client.two_way_request_only(LargeMessageUnion::Str(LARGE_STR.to_string())).await.unwrap();
    })
    .await
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_two_way_request_only_small_async() {
    run_client_async(SMALL_STR, |client| async move {
        client.two_way_request_only(LargeMessageUnion::Str(SMALL_STR.to_string())).await.unwrap();
    })
    .await
}

#[test]
fn overflowing_two_way_response_only_large_sync() {
    run_client_sync(LARGE_STR, |client| {
        let payload = client
            .two_way_response_only(
                OverflowingProtocolTwoWayResponseOnlyRequest::default(),
                Time::INFINITE,
            )
            .unwrap();

        assert_eq!(&payload.str.unwrap(), LARGE_STR);
    })
}

#[test]
fn overflowing_two_way_response_only_small_sync() {
    run_client_sync(SMALL_STR, |client| {
        let payload = client
            .two_way_response_only(
                OverflowingProtocolTwoWayResponseOnlyRequest::default(),
                Time::INFINITE,
            )
            .unwrap();

        assert_eq!(&payload.str.unwrap(), SMALL_STR);
    })
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_two_way_response_only_large_async() {
    run_client_async(LARGE_STR, |client| async move {
        let payload = client
            .two_way_response_only(OverflowingProtocolTwoWayResponseOnlyRequest::default())
            .await
            .unwrap();

        assert_eq!(&payload.str.unwrap(), LARGE_STR);
    })
    .await
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_two_way_response_only_small_async() {
    run_client_async(SMALL_STR, |client| async move {
        let payload = client
            .two_way_response_only(OverflowingProtocolTwoWayResponseOnlyRequest::default())
            .await
            .unwrap();

        assert_eq!(&payload.str.unwrap(), SMALL_STR);
    })
    .await
}

#[test]
fn overflowing_two_way_both_request_and_response_large_sync() {
    run_client_sync(LARGE_STR, |client| {
        let str = client.two_way_both_request_and_response(LARGE_STR, Time::INFINITE).unwrap();

        assert_eq!(&str, LARGE_STR);
    })
}

#[test]
fn overflowing_two_way_both_request_and_response_small_sync() {
    run_client_sync(SMALL_STR, |client| {
        let str = client.two_way_both_request_and_response(SMALL_STR, Time::INFINITE).unwrap();

        assert_eq!(&str, SMALL_STR);
    })
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_two_way_both_request_and_response_large_async() {
    run_client_async(LARGE_STR, |client| async move {
        let str = client.two_way_both_request_and_response(LARGE_STR).await.unwrap();

        assert_eq!(&str, LARGE_STR);
    })
    .await
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_two_way_both_request_and_response_small_async() {
    run_client_async(SMALL_STR, |client| async move {
        let str = client.two_way_both_request_and_response(SMALL_STR).await.unwrap();

        assert_eq!(&str, SMALL_STR);
    })
    .await
}

#[test]
fn overflowing_one_way_large_sync() {
    run_client_sync(LARGE_STR, |client| {
        client
            .one_way_call(LargeMessageTable {
                str: Some(LARGE_STR.to_string()),
                ..Default::default()
            })
            .unwrap();
        let event = client
            .wait_for_event(Time::after(WAIT_FOR_EVENT))
            .unwrap()
            .into_on_one_way_reply_event()
            .unwrap();

        match event {
            LargeMessageUnion::Str(str) => {
                assert_eq!(&str, LARGE_STR);
            }
            LargeMessageUnionUnknown!() => panic!("unknown event data"),
        }
    })
}

#[test]
fn overflowing_one_way_small_sync() {
    run_client_sync(SMALL_STR, |client| {
        client
            .one_way_call(LargeMessageTable {
                str: Some(SMALL_STR.to_string()),
                ..Default::default()
            })
            .unwrap();
        let event = client
            .wait_for_event(Time::after(WAIT_FOR_EVENT))
            .unwrap()
            .into_on_one_way_reply_event()
            .unwrap();

        match event {
            LargeMessageUnion::Str(str) => {
                assert_eq!(&str, SMALL_STR);
            }
            LargeMessageUnionUnknown!() => panic!("unknown event data"),
        }
    })
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_one_way_large_async() {
    run_client_async(LARGE_STR, |client| async move {
        client
            .one_way_call(LargeMessageTable {
                str: Some(LARGE_STR.to_string()),
                ..Default::default()
            })
            .unwrap();
        let OverflowingProtocolEvent::OnOneWayReplyEvent { payload } =
            client.take_event_stream().next().await.unwrap().unwrap();

        match payload {
            LargeMessageUnion::Str(str) => {
                assert_eq!(&str, LARGE_STR);
            }
            LargeMessageUnionUnknown!() => panic!("unknown event data"),
        }
    })
    .await
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_one_way_small_async() {
    run_client_async(SMALL_STR, |client| async move {
        client
            .one_way_call(LargeMessageTable {
                str: Some(SMALL_STR.to_string()),
                ..Default::default()
            })
            .unwrap();
        let OverflowingProtocolEvent::OnOneWayReplyEvent { payload } =
            client.take_event_stream().next().await.unwrap().unwrap();

        match payload {
            LargeMessageUnion::Str(str) => {
                assert_eq!(&str, SMALL_STR);
            }
            LargeMessageUnionUnknown!() => panic!("unknown event data"),
        }
    })
    .await
}

#[test]
fn overflowing_resourced_63_handles_large_sync() {
    run_client_sync(LARGE_STR, |client| {
        let present = 63;
        let (out_str, out_present_handles, out_handles) = client
            .resourced(LARGE_STR, present, build_handle_array(present), Time::INFINITE)
            .unwrap();

        assert_eq!(&out_str, LARGE_STR);
        assert_eq!(out_present_handles, present);
        validate_handle_array(&out_handles, present);
    })
}

#[test]
fn overflowing_resourced_63_handles_small_sync() {
    run_client_sync(SMALL_STR, |client| {
        let present = 63;
        let (out_str, out_present_handles, out_handles) = client
            .resourced(SMALL_STR, present, build_handle_array(present), Time::INFINITE)
            .unwrap();

        assert_eq!(&out_str, SMALL_STR);
        assert_eq!(out_present_handles, present);
        validate_handle_array(&out_handles, present);
    })
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_resourced_63_handles_large_async() {
    run_client_async(LARGE_STR, |client| async move {
        let present = 63;
        let (out_str, out_present_handles, out_handles) =
            client.resourced(LARGE_STR, present, build_handle_array(present)).await.unwrap();

        assert_eq!(&out_str, LARGE_STR);
        assert_eq!(out_present_handles, present);
        validate_handle_array(&out_handles, present);
    })
    .await
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_resourced_63_handles_small_async() {
    run_client_async(SMALL_STR, |client| async move {
        let present = 63;
        let (out_str, out_present_handles, out_handles) =
            client.resourced(SMALL_STR, present, build_handle_array(present)).await.unwrap();

        assert_eq!(&out_str, SMALL_STR);
        assert_eq!(out_present_handles, present);
        validate_handle_array(&out_handles, present);
    })
    .await
}

#[test]
fn overflowing_resourced_64_handles_large_sync() {
    run_client_sync(LARGE_STR, |client| {
        let present = 64;
        let result =
            client.resourced(LARGE_STR, present, build_handle_array(present), Time::INFINITE);
        assert_matches!(result, Err(fidl::Error::LargeMessage64Handles));
    })
}

#[test]
fn overflowing_resourced_64_handles_small_sync() {
    run_client_sync(SMALL_STR, |client| {
        let present = 64;
        let (out_str, out_present_handles, out_handles) = client
            .resourced(SMALL_STR, present, build_handle_array(present), Time::INFINITE)
            .unwrap();

        assert_eq!(&out_str, SMALL_STR);
        assert_eq!(out_present_handles, present);
        validate_handle_array(&out_handles, present);
    })
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_resourced_64_handles_large_async() {
    run_client_async(LARGE_STR, |client| async move {
        let present = 64;
        let result = client.resourced(LARGE_STR, present, build_handle_array(present)).await;
        assert_matches!(result, Err(fidl::Error::LargeMessage64Handles));
    })
    .await
}

#[fasync::run_singlethreaded(test)]
async fn overflowing_resourced_64_handles_small_async() {
    run_client_async(SMALL_STR, |client| async move {
        let present = 64;
        let (out_str, out_present_handles, out_handles) =
            client.resourced(SMALL_STR, present, build_handle_array(present)).await.unwrap();

        assert_eq!(&out_str, SMALL_STR);
        assert_eq!(out_present_handles, present);
        validate_handle_array(&out_handles, present);
    })
    .await
}
