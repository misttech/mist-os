// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next_codec::{
    Chunk, Decode, Decoded, DecoderExt as _, Encode, EncoderExt as _, WireString,
};
use fuchsia_async::{Scope, Task};

use crate::{
    Client, ClientHandler, ClientSender, Responder, Server, ServerHandler, ServerSender, Transport,
};

pub fn assert_encoded<T: Encode<Vec<Chunk>>>(value: T, chunks: &[Chunk]) {
    let mut encoded_chunks = Vec::new();
    encoded_chunks.encode_next(value).unwrap();
    assert_eq!(encoded_chunks, chunks, "encoded chunks did not match");
}

pub fn assert_decoded<T: for<'a> Decode<&'a mut [Chunk]>>(
    mut chunks: &mut [Chunk],
    f: impl FnOnce(Decoded<T, &mut [Chunk]>),
) {
    let value = (&mut chunks).decode::<T>().expect("failed to decode");
    f(value)
}

pub async fn test_close_on_drop<T: Transport + 'static>(client_end: T, server_end: T) {
    struct TestServer {
        scope: Scope,
    }

    impl<T: Transport + 'static> ServerHandler<T> for TestServer {
        fn on_one_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer) {
            panic!("unexpected event");
        }

        fn on_two_way(
            &mut self,
            server: &ServerSender<T>,
            ordinal: u64,
            buffer: T::RecvBuffer,
            responder: Responder,
        ) {
            let message = buffer.decode::<WireString>().expect("failed to decode request");
            assert_eq!(ordinal, 42);
            assert_eq!(&**message, "Ping");

            let server = server.clone();
            self.scope.spawn(async move {
                server
                    .send_response(responder, 42, "Pong")
                    .expect("failed to encode response")
                    .await
                    .expect("failed to send response");
            });
        }
    }

    let mut client = Client::new(client_end);
    let client_sender = client.sender().clone();
    let client_task = Task::spawn(async move { client.run_sender().await });
    let mut server = Server::new(server_end);
    let server_task =
        Task::spawn(async move { server.run(TestServer { scope: Scope::new() }).await });

    let message = client_sender
        .send_two_way(42, "Ping")
        .expect("client failed to encode request")
        .await
        .expect("client failed to send request and receive response")
        .decode::<WireString>()
        .expect("failed to decode response");
    assert_eq!(&**message, "Pong");

    drop(client_sender);
    drop(client_task);

    server_task.await.expect("server encountered an error");
}

pub async fn test_one_way<T: Transport + 'static>(client_end: T, server_end: T) {
    struct TestServer;

    impl<T: Transport> ServerHandler<T> for TestServer {
        fn on_one_way(&mut self, _: &ServerSender<T>, ordinal: u64, buffer: T::RecvBuffer) {
            assert_eq!(ordinal, 42);
            let message = buffer.decode::<WireString>().expect("failed to decode request");
            assert_eq!(&**message, "Hello world");
        }

        fn on_two_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer, _: Responder) {
            panic!("unexpected two-way message");
        }
    }

    let mut client = Client::new(client_end);
    let client_sender = client.sender().clone();
    let client_task = Task::spawn(async move { client.run_sender().await });
    let mut server = Server::new(server_end);
    let server_task = Task::spawn(async move { server.run(TestServer).await });

    client_sender
        .send_one_way(42, "Hello world")
        .expect("client failed to encode request")
        .await
        .expect("client failed to send request");
    client_sender.close();
    // TODO: Don't require dropping all of the senders to close
    drop(client_sender);

    client_task.await.expect("client encountered an error");
    server_task.await.expect("server encountered an error");
}

pub async fn test_two_way<T: Transport + 'static>(client_end: T, server_end: T) {
    struct TestServer {
        scope: Scope,
    }

    impl<T: Transport + 'static> ServerHandler<T> for TestServer {
        fn on_one_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer) {
            panic!("unexpected event");
        }

        fn on_two_way(
            &mut self,
            server: &ServerSender<T>,
            ordinal: u64,
            buffer: T::RecvBuffer,
            responder: Responder,
        ) {
            assert_eq!(ordinal, 42);
            let message = buffer.decode::<WireString>().expect("failed to decode request");
            assert_eq!(&**message, "Ping");

            let server = server.clone();
            self.scope.spawn(async move {
                server
                    .send_response(responder, 42, "Pong")
                    .expect("failed to encode response")
                    .await
                    .expect("failed to send response");
            });
        }
    }

    let mut client = Client::new(client_end);
    let client_sender = client.sender().clone();
    let client_task = Task::spawn(async move { client.run_sender().await });
    let mut server = Server::new(server_end);
    let server_task =
        Task::spawn(async move { server.run(TestServer { scope: Scope::new() }).await });

    let message = client_sender
        .send_two_way(42, "Ping")
        .expect("client failed to encode request")
        .await
        .expect("client failed to send request and receive response")
        .decode::<WireString>()
        .expect("failed to decode response");
    assert_eq!(&**message, "Pong");

    client_sender.close();
    // TODO: Don't require dropping all of the senders to close
    drop(client_sender);

    client_task.await.expect("client encountered an error");
    server_task.await.expect("server encountered an error");
}

pub async fn test_multiple_two_way<T: Transport + 'static>(client_end: T, server_end: T) {
    struct TestServer {
        scope: Scope,
    }

    impl<T: Transport + 'static> ServerHandler<T> for TestServer {
        fn on_one_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer) {
            panic!("unexpected event");
        }

        fn on_two_way(
            &mut self,
            server: &ServerSender<T>,
            ordinal: u64,
            buffer: T::RecvBuffer,
            responder: Responder,
        ) {
            let message = buffer.decode::<WireString>().expect("failed to decode request");

            let response = match ordinal {
                1 => "One",
                2 => "Two",
                3 => "Three",
                x => panic!("unexpected request ordinal {x} from client"),
            };

            assert_eq!(&**message, response);

            let server = server.clone();
            self.scope.spawn(async move {
                server
                    .send_response(responder, ordinal, response)
                    .expect("server failed to encode response")
                    .await
                    .expect("server failed to send response");
            });
        }
    }

    let mut client = Client::new(client_end);
    let client_sender = client.sender().clone();
    let client_task = Task::spawn(async move { client.run_sender().await });
    let mut server = Server::new(server_end);
    let server_task =
        Task::spawn(async move { server.run(TestServer { scope: Scope::new() }).await });

    let send_one = client_sender.send_two_way(1, "One").expect("client failed to encode request");
    let send_two = client_sender.send_two_way(2, "Two").expect("client failed to encode request");
    let send_three =
        client_sender.send_two_way(3, "Three").expect("client failed to encode request");
    let (response_one, response_two, response_three) =
        futures::join!(send_one, send_two, send_three);

    let message_one = response_one
        .expect("client failed to send request and receive response")
        .decode::<WireString>()
        .expect("failed to decode response");
    assert_eq!(&**message_one, "One");

    let message_two = response_two
        .expect("client failed to send request and receive response")
        .decode::<WireString>()
        .expect("failed to decode response");
    assert_eq!(&**message_two, "Two");

    let message_three = response_three
        .expect("client failed to send request and receive response")
        .decode::<WireString>()
        .expect("failed to decode response");
    assert_eq!(&**message_three, "Three");

    client_sender.close();
    // TODO: Don't require dropping all of the senders to close
    drop(client_sender);

    client_task.await.expect("client encountered an error");
    server_task.await.expect("server encountered an error");
}

pub async fn test_event<T: Transport + 'static>(client_end: T, server_end: T) {
    struct TestClient;

    impl<T: Transport> ClientHandler<T> for TestClient {
        fn on_event(&mut self, client: &ClientSender<T>, ordinal: u64, buffer: T::RecvBuffer) {
            assert_eq!(ordinal, 10);
            let message = buffer.decode::<WireString>().expect("failed to decode request");
            assert_eq!(&**message, "Surprise!");

            client.close();
        }
    }

    pub struct TestServer;

    impl<T: Transport> ServerHandler<T> for TestServer {
        fn on_one_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer) {}
        fn on_two_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer, _: Responder) {}
    }

    let mut client = Client::new(client_end);
    let client_task = Task::spawn(async move { client.run(TestClient).await });
    let mut server = Server::new(server_end);
    let server_sender = server.sender().clone();
    let server_task = Task::spawn(async move { server.run(TestServer).await });

    server_sender
        .send_event(10, "Surprise!")
        .expect("server failed to encode response")
        .await
        .expect("server failed to send response");

    client_task.await.expect("client encountered an error");
    server_task.await.expect("server encountered an error");
}
