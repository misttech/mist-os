// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;

use crate::protocol::server::Request;
use crate::protocol::{make_client, make_server, Transport};
use crate::WireString;

pub async fn test_send_receive<T: Transport>(client_end: T, server_end: T) {
    let (mut dispatcher, client, _) = make_client(client_end);
    let (_, mut requests) = make_server(server_end);

    let dispatcher_task = fasync::Task::spawn(async move {
        dispatcher.run().await;
    });

    client
        .send_request(42, &mut "Hello world".to_string())
        .expect("client failed to encode request")
        .await
        .expect("client failed to send request");

    let request = requests
        .next()
        .await
        .expect("server encountered an error while receiving")
        .expect("server did not receive a request");
    let Request::OneWay { mut buffer } = request else {
        panic!("expected a one-way request from the client");
    };

    assert_eq!(buffer.ordinal(), 42);
    let message = buffer.decode::<WireString<'_>>().expect("failed to decode request");
    assert_eq!(&**message, "Hello world");

    dispatcher_task.cancel().await;
}

pub async fn test_transaction<T: Transport>(client_end: T, server_end: T) {
    let (mut dispatcher, client, _) = make_client(client_end);
    let (server, mut requests) = make_server(server_end);

    let dispatcher_task = fasync::Task::spawn(async move {
        dispatcher.run().await;
    });

    let server_task = fasync::Task::spawn(async move {
        let request = requests
            .next()
            .await
            .expect("server encountered an error while receiving")
            .expect("server did not receive a request");
        let Request::Transaction { responder, mut buffer } = request else {
            panic!("expected a transaction request from the client");
        };

        assert_eq!(buffer.ordinal(), 42);
        let message = buffer.decode::<WireString<'_>>().expect("failed to decode message");
        assert_eq!(&**message, "Ping");

        server
            .send_response(responder, 42, &mut "Pong".to_string())
            .expect("server failed to encode response")
            .await
            .expect("server failed to send response");
    });

    let mut buffer = client
        .send_transaction(42, &mut "Ping".to_string())
        .expect("client failed to encode request")
        .await
        .expect("client failed to send request and receive response");
    assert_eq!(buffer.ordinal(), 42);
    let message = buffer.decode::<WireString<'_>>().expect("failed to decode response");
    assert_eq!(&**message, "Pong");

    server_task.cancel().await;
    dispatcher_task.cancel().await;
}

pub async fn test_multiple_transactions<T: Transport>(client_end: T, server_end: T) {
    let (mut dispatcher, client, _) = make_client(client_end);
    let (server, mut requests) = make_server(server_end);

    let dispatcher_task = fasync::Task::spawn(async move {
        dispatcher.run().await;
    });

    let server_task = fasync::Task::spawn(async move {
        while let Some(request) =
            requests.next().await.expect("server encountered an error while receiving")
        {
            let Request::Transaction { responder, mut buffer } = request else {
                panic!("expected only transaction requests from the client");
            };

            let ordinal = buffer.ordinal();
            let message = buffer.decode::<WireString<'_>>().expect("failed to decode message");

            let response = match ordinal {
                1 => "One",
                2 => "Two",
                3 => "Three",
                x => panic!("unexpected request ordinal {x} from client"),
            };

            assert_eq!(&**message, response);
            server
                .send_response(responder, ordinal, &mut response.to_string())
                .expect("server failed to encode response")
                .await
                .expect("server failed to send response");
        }
    });

    let send_one = client
        .send_transaction(1, &mut "One".to_string())
        .expect("client failed to encode request");
    let send_two = client
        .send_transaction(2, &mut "Two".to_string())
        .expect("client failed to encode request");
    let send_three = client
        .send_transaction(3, &mut "Three".to_string())
        .expect("client failed to encode request");
    let (response_one, response_two, response_three) =
        futures::join!(send_one, send_two, send_three);

    let mut buffer_one = response_one.expect("client failed to send request and receive response");
    let message_one = buffer_one.decode::<WireString<'_>>().expect("failed to decode response");
    assert_eq!(&**message_one, "One");

    let mut buffer_two = response_two.expect("client failed to send request and receive response");
    let message_two = buffer_two.decode::<WireString<'_>>().expect("failed to decode response");
    assert_eq!(&**message_two, "Two");

    let mut buffer_three =
        response_three.expect("client failed to send request and receive response");
    let message_three = buffer_three.decode::<WireString<'_>>().expect("failed to decode response");
    assert_eq!(&**message_three, "Three");

    server_task.cancel().await;
    dispatcher_task.cancel().await;
}

pub async fn test_event<T: Transport>(client_end: T, server_end: T) {
    let (mut dispatcher, _, mut events) = make_client(client_end);
    let (server, _) = make_server(server_end);

    let dispatcher_task = fasync::Task::spawn(async move {
        dispatcher.run().await;
    });

    server
        .send_event(10, &mut "Surprise!".to_string())
        .expect("server failed to encode response")
        .await
        .expect("server failed to send response");

    let mut buffer = events
        .next()
        .await
        .expect("client failed to receive event")
        .expect("client did not receive an event");
    assert_eq!(buffer.ordinal(), 10);
    let message = buffer.decode::<WireString<'_>>().expect("failed to decode event");
    assert_eq!(&**message, "Surprise!");

    dispatcher_task.cancel().await;
}
