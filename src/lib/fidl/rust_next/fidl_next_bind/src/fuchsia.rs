// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Fuchsia-specific FIDL bindings.

use fuchsia_async::Task;
use zx::Channel;

use fidl_next_protocol::Transport;

use crate::{
    Client, ClientEnd, ClientProtocol, ClientSender, Server, ServerEnd, ServerProtocol,
    ServerSender,
};

/// Creates a `ClientEnd` and `ServerEnd` for the given protocol over Zircon channels.
pub fn create_channel<P>() -> (ClientEnd<P, zx::Channel>, ServerEnd<P, zx::Channel>) {
    let (client_end, server_end) = Channel::create();
    (ClientEnd::from_untyped(client_end), ServerEnd::from_untyped(server_end))
}

/// Creates a `Client` from the given `ClientEnd` and spawns it on the current fuchsia-async
/// executor.
///
/// The spawned client will handle any incoming events with `handler`.
///
/// Returns a `ClientSender` for the spawned client.
pub fn spawn_client_detached<P, T, H>(client_end: ClientEnd<P, T>, handler: H) -> ClientSender<P, T>
where
    P: ClientProtocol<H, T> + 'static,
    T: Transport + 'static,
    H: Send + 'static,
{
    let mut client = Client::new(client_end);
    let sender = client.sender().clone();
    Task::spawn(async move { client.run(handler).await }).detach_on_drop();
    sender
}

/// Creates a `Client` from the given `ClientEnd` and spawns it on the current fuchsia-async
/// executor.
///
/// The spawned client will ignore any incoming events.
///
/// Returns a `ClientSender` for the spawned client.
pub fn spawn_client_sender_detached<P, T>(client_end: ClientEnd<P, T>) -> ClientSender<P, T>
where
    T: Transport + 'static,
    P: 'static,
{
    let mut client = Client::new(client_end);
    let sender = client.sender().clone();
    Task::spawn(async move { client.run_sender().await }).detach_on_drop();
    sender
}

/// Creates a `Server` from the given `ServerEnd` and spawns it on the current fuchsia-async
/// executor.
///
/// The spawned server will handle any incoming requests with the provided handler.
///
/// Returns a `ServerSender` for the spawned server.
pub fn spawn_server_detached<P, T, H>(server_end: ServerEnd<P, T>, handler: H) -> ServerSender<P, T>
where
    T: Transport + 'static,
    P: ServerProtocol<H, T> + 'static,
    H: Send + 'static,
{
    let mut server = Server::new(server_end);
    let sender = server.sender().clone();
    Task::spawn(async move { server.run(handler).await }).detach_on_drop();
    sender
}
