// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_update::{
    ListenerRequest, ListenerRequestStream, ListenerWaitForFirstUpdateCheckToCompleteResponder,
};
use fidl_fuchsia_update_ext::State;
use futures::channel::mpsc;
use futures::{SinkExt as _, StreamExt as _, TryStreamExt as _};
use tracing::warn;

pub(crate) struct CompletionResponder {
    state: CompletionResponderState,
    receiver: mpsc::UnboundedReceiver<Message>,
}

enum CompletionResponderState {
    Waiting(Vec<ListenerWaitForFirstUpdateCheckToCompleteResponder>),
    Satisfied,
}

pub(crate) struct CompletionResponderStateReactor(mpsc::UnboundedSender<Message>);

#[derive(Clone)]
pub(crate) struct CompletionResponderFidlServer(mpsc::UnboundedSender<Message>);

enum Message {
    Completed,
    NewWaiter(ListenerWaitForFirstUpdateCheckToCompleteResponder),
}

impl CompletionResponderFidlServer {
    pub async fn serve_completion_responses(
        mut self,
        mut request_stream: ListenerRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            request_stream.try_next().await.context("extracting request from stream")?
        {
            match request {
                ListenerRequest::WaitForFirstUpdateCheckToComplete { responder, .. } => {
                    if let Err(e) = self.0.send(Message::NewWaiter(responder)).await {
                        warn!(?e, "Internal bug; this send should always succeed");
                    }
                }
                ListenerRequest::_UnknownMethod { .. } => {}
            }
        }
        Ok(())
    }
}

impl CompletionResponderStateReactor {
    pub async fn react_to(&mut self, state: &State) {
        // If a software update has completed, with or without error, and no reboot is needed,
        // then send a Completed message to the CompletionResponder message-handler.
        // Otherwise, just return.
        match state {
            State::ErrorCheckingForUpdate
            | State::NoUpdateAvailable
            | State::InstallationDeferredByPolicy(_) => {
                // Internal stream - discard theoretical errors
                if let Err(e) = self.0.send(Message::Completed).await {
                    warn!(?e, "Internal bug; this send should always succeed");
                }
            }
            State::InstallingUpdate(_)
            | State::WaitingForReboot(_)
            | State::InstallationError(_)
            | State::CheckingForUpdates => {}
        }
    }

    // A non-functional struct for use in tests that don't care about completion responding.
    #[cfg(test)]
    pub fn test_stub() -> Self {
        let (send, _) = mpsc::unbounded();
        Self(send)
    }
}

impl CompletionResponder {
    pub fn build() -> (CompletionResponderStateReactor, CompletionResponderFidlServer, Self) {
        let (sender, receiver) = mpsc::unbounded();
        (
            CompletionResponderStateReactor(sender.clone()),
            CompletionResponderFidlServer(sender),
            Self { state: CompletionResponderState::Waiting(Vec::new()), receiver },
        )
    }

    pub async fn wait_for_messages(mut self) {
        while let Some(message) = self.receiver.next().await {
            match message {
                Message::Completed => self.state.become_satisfied(),
                Message::NewWaiter(responder) => self.state.respond_when_appropriate(responder),
            }
        }
        warn!("CompletionResponder stream shouldn't close");
    }
}

impl CompletionResponderState {
    fn become_satisfied(&mut self) {
        if let CompletionResponderState::Waiting(ref mut responders) = self {
            for responder in responders.drain(..) {
                let _ = responder.send();
            }
            *self = CompletionResponderState::Satisfied;
        }
    }

    fn respond_when_appropriate(
        &mut self,
        responder: ListenerWaitForFirstUpdateCheckToCompleteResponder,
    ) {
        match self {
            CompletionResponderState::Waiting(ref mut responses) => responses.push(responder),
            CompletionResponderState::Satisfied => {
                // If the client has closed the connection, that's not our concern.
                let _ = responder.send();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompletionResponder;
    use core::task::Poll;
    use fidl_fuchsia_update::ListenerMarker;
    use fidl_fuchsia_update_ext::{
        InstallationDeferredData, InstallationErrorData, InstallingData,
    };
    use fuchsia_async::{self as fasync, TestExecutor};
    use futures::pin_mut;

    // Don't drop tasks until the end of the test.
    fn setup_responder(
    ) -> (CompletionResponderStateReactor, CompletionResponderFidlServer, fasync::Task<()>) {
        let (reactor, server, waiter) = CompletionResponder::build();
        let task = fasync::Task::local(waiter.wait_for_messages());
        (reactor, server, task)
    }

    async fn wait_for_completion(server: CompletionResponderFidlServer) {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<ListenerMarker>();
        fasync::Task::local(async move {
            server.serve_completion_responses(stream).await.unwrap();
        })
        .detach();
        proxy.wait_for_first_update_check_to_complete().await.unwrap();
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_whether_state_satisfies() {
        for state in [
            State::InstallationDeferredByPolicy(InstallationDeferredData::default()),
            State::NoUpdateAvailable,
            State::ErrorCheckingForUpdate,
            State::CheckingForUpdates,
            State::InstallingUpdate(InstallingData::default()),
            State::WaitingForReboot(InstallingData::default()),
            State::InstallationError(InstallationErrorData::default()),
        ] {
            let (mut reactor, server, _task) = setup_responder();
            reactor.react_to(&state).await;
            let fut = wait_for_completion(server);
            pin_mut!(fut);
            let result = TestExecutor::poll_until_stalled(&mut fut).await;
            match state {
                State::InstallationDeferredByPolicy(_)
                | State::NoUpdateAvailable
                | State::ErrorCheckingForUpdate => {
                    assert_eq!(result, Poll::Ready(()), "State {:?} didn't satisfy", state);
                }
                State::CheckingForUpdates
                | State::InstallingUpdate(_)
                | State::WaitingForReboot(_)
                | State::InstallationError(_) => {
                    assert_eq!(result, Poll::Pending, "State {:?} satisfied", state);
                }
            }
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn correct_response_sequencing() {
        let (mut reactor, server, _task) = setup_responder();
        let waiter1 = wait_for_completion(server.clone());
        let waiter2 = wait_for_completion(server.clone());
        pin_mut!(waiter1);
        pin_mut!(waiter2);

        reactor.react_to(&State::CheckingForUpdates).await;
        assert_eq!(TestExecutor::poll_until_stalled(&mut waiter1).await, Poll::Pending);
        assert_eq!(TestExecutor::poll_until_stalled(&mut waiter2).await, Poll::Pending);

        reactor.react_to(&State::NoUpdateAvailable).await;
        waiter1.await;
        waiter2.await;
        // Clients that start to wait after the happy state is received should return immediately.
        wait_for_completion(server).await;
    }
}
