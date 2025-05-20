// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use fidl_fuchsia_update::{
    ListenerRequest, ListenerRequestStream, ListenerWaitForFirstUpdateCheckToCompleteResponder,
};
use fidl_test_persistence_factory::{ControllerRequest, ControllerRequestStream};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{ChildOptions, ChildRef, LocalComponentHandles, RealmBuilder};
use fuchsia_sync::Mutex;
use futures::prelude::*;
use std::sync::Arc;

enum IncomingService {
    Controller(ControllerRequestStream),
    Listener(ListenerRequestStream),
}

enum ServerState {
    Waiting(Vec<ListenerWaitForFirstUpdateCheckToCompleteResponder>),
    Satisfied,
}

impl ServerState {
    fn new() -> Self {
        ServerState::Waiting(vec![])
    }

    fn become_satisfied(&mut self) {
        if let ServerState::Waiting(ref mut responders) = self {
            for responder in responders.drain(..) {
                let _ = responder.send();
            }
            *self = ServerState::Satisfied;
        }
    }

    fn respond_when_appropriate(
        &mut self,
        responder: ListenerWaitForFirstUpdateCheckToCompleteResponder,
    ) {
        match self {
            ServerState::Waiting(ref mut responses) => responses.push(responder),
            ServerState::Satisfied => {
                // If the client has closed the connection, that's not our concern.
                let _ = responder.send();
            }
        }
    }
}

pub(crate) async fn handle_update_check_services(
    builder: &RealmBuilder,
) -> Result<ChildRef, Error> {
    // Add a mock component that serves fuchsia.update.Listener and fuchsia.persistence.test.Controller.
    Ok(builder
        .add_local_child(
            "fidl-server",
            move |handles: LocalComponentHandles| Box::pin(fidl_server_mock(handles)),
            ChildOptions::new(),
        )
        .await?)
}

async fn fidl_server_mock(handles: LocalComponentHandles) -> Result<(), Error> {
    let state = Arc::new(Mutex::new(ServerState::new()));
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingService::Controller);
    fs.dir("svc").add_fidl_service(IncomingService::Listener);
    fs.serve_connection(handles.outgoing_dir)?;
    fs.for_each_concurrent(None, move |server: IncomingService| {
        let state = state.clone();
        async move {
            match server {
                IncomingService::Controller(stream) => {
                    handle_controller_stream(stream, state.clone()).await
                }
                IncomingService::Listener(stream) => {
                    handle_listener_stream(stream, state.clone()).await
                }
            }
        }
    })
    .await;
    Ok(())
}

async fn handle_controller_stream(stream: ControllerRequestStream, state: Arc<Mutex<ServerState>>) {
    stream
        .map(|result| result.context("Request came with error"))
        .try_for_each(|request| {
            let state = state.clone();
            async move {
                match request {
                    ControllerRequest::SetUpdateCompleted { responder } => {
                        state.lock().become_satisfied();
                        responder.send().expect("Tried to respond to SetUpdateCompleted");
                    }
                    ControllerRequest::_UnknownMethod { .. } => {}
                }
                Ok(())
            }
        })
        .await
        .context("Failed to serve request stream")
        .unwrap_or_else(|e| eprintln!("Error encountered: {e:?}"));
}

async fn handle_listener_stream(stream: ListenerRequestStream, state: Arc<Mutex<ServerState>>) {
    stream
        .map(|result| result.context("Request came with error"))
        .try_for_each(|request| {
            let state = state.clone();
            async move {
                match request {
                    ListenerRequest::WaitForFirstUpdateCheckToComplete { responder } => {
                        state.lock().respond_when_appropriate(responder);
                    }
                    ListenerRequest::_UnknownMethod { .. } => {}
                }
                Ok(())
            }
        })
        .await
        .context("Failed to serve request stream")
        .unwrap_or_else(|e| eprintln!("Error encountered: {e:?}"));
}
