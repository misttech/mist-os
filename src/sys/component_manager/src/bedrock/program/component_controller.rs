// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{EscrowRequest, StopInfo};
use fuchsia_sync::Mutex;
use futures::channel::oneshot;
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt, StreamExt};
use std::ops::Deref;
use std::sync::Arc;
use {
    fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_diagnostics_types as fdiagnostics,
    fuchsia_async as fasync, fuchsia_zircon as zx,
};

/// Wrapper around the `ComponentControllerProxy` with utilities for handling events.
pub struct ComponentController {
    /// The wrapped `ComponentController` connection.
    inner: fcrunner::ComponentControllerProxy,

    /// Receiver for epitaphs coming from the connection.
    epitaph_value_recv: Shared<oneshot::Receiver<zx::Status>>,

    /// State stored by the `ComponentController` server.
    state: Arc<Mutex<State>>,

    /// The task listening for events.
    _event_listener_task: fasync::Task<()>,
}

struct State {
    /// The last value seen in an `ComponentController.OnEscrow` event.
    escrow: Option<EscrowRequest>,

    /// The last value seen in an `ComponentController.OnStopInfo` event.
    stop_info: Option<StopInfo>,
}

impl Deref for ComponentController {
    type Target = fcrunner::ComponentControllerProxy;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> ComponentController {
    /// Create a new wrapper around a `ComponentControllerProxy`.
    ///
    /// A task will be spawned to dispatch events on the `ComponentControllerProxy`.
    pub fn new(
        proxy: fcrunner::ComponentControllerProxy,
        diagnostics_sender: Option<oneshot::Sender<fdiagnostics::ComponentDiagnostics>>,
    ) -> Self {
        let (epitaph_sender, epitaph_value_recv) = oneshot::channel();

        let state = Arc::new(Mutex::new(State { escrow: None, stop_info: None }));
        let event_stream = proxy.take_event_stream();
        let events_fut = Self::listen_for_events(
            event_stream,
            epitaph_sender,
            state.clone(),
            diagnostics_sender,
        );
        let event_listener_task = fasync::Task::spawn(events_fut);

        Self {
            inner: proxy,
            epitaph_value_recv: epitaph_value_recv.shared(),
            state,
            _event_listener_task: event_listener_task,
        }
    }

    /// Returns state that the program has escrowed, if any.
    pub fn finalize(self) -> (Option<EscrowRequest>, Option<StopInfo>) {
        let mut state = self.state.lock();
        (state.escrow.take(), state.stop_info.take())
    }

    /// Obtain a future for the epitaph value.
    ///
    /// If the async task handling the `ComponentControllerProxy` unexpectedly exits, the future
    /// will resolve to `PEER_CLOSED`. Otherwise, it will resolve to the `zx::Status` representing
    /// the epitaph sent on the channel.
    ///
    /// This method may be called multiple times from multiple threads.
    pub fn wait_for_epitaph(&self) -> BoxFuture<'static, zx::Status> {
        let val = self.epitaph_value_recv.clone();
        async move { val.await.unwrap_or(zx::Status::PEER_CLOSED) }.boxed()
    }

    async fn listen_for_events(
        mut event_stream: fcrunner::ComponentControllerEventStream,
        epitaph_sender: oneshot::Sender<zx::Status>,
        state: Arc<Mutex<State>>,
        mut diagnostics_sender: Option<oneshot::Sender<fdiagnostics::ComponentDiagnostics>>,
    ) {
        let mut epitaph_sender = Some(epitaph_sender);
        while let Some(value) = event_stream.next().await {
            match value {
                Err(fidl::Error::ClientChannelClosed { status, .. }) => {
                    epitaph_sender.take().and_then(|sender| sender.send(status).ok());
                }
                Err(_) => {
                    epitaph_sender
                        .take()
                        .and_then(|sender| sender.send(zx::Status::PEER_CLOSED).ok());
                }
                Ok(event) => match event {
                    fcrunner::ComponentControllerEvent::OnPublishDiagnostics {
                        payload, ..
                    } => {
                        diagnostics_sender.take().and_then(|sender| sender.send(payload).ok());
                    }
                    fcrunner::ComponentControllerEvent::OnEscrow { payload } => {
                        state.lock().escrow = Some(payload.into());
                    }
                    fcrunner::ComponentControllerEvent::OnStopInfo { payload } => {
                        state.lock().stop_info = Some(payload.into());
                    }
                },
            }
        }
        epitaph_sender.take().map(|sender| sender.send(zx::Status::PEER_CLOSED).unwrap_or(()));
    }

    /// Get the KOID of the `ComponentController` FIDL server endpoint.
    #[cfg(test)]
    pub fn peer_koid(&self) -> zx::Koid {
        use fidl::endpoints::Proxy;
        use zx::AsHandleRef;

        self.inner
            .as_channel()
            .as_handle_ref()
            .basic_info()
            .expect("basic info should not require any rights")
            .related_koid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::prelude::*;

    #[fuchsia::test]
    async fn handles_diagnostics_event() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fcrunner::ComponentControllerMarker>()
                .unwrap();
        let (sender, receiver) = oneshot::channel();
        let _controller = ComponentController::new(proxy, Some(sender));
        stream
            .control_handle()
            .send_on_publish_diagnostics(fdiagnostics::ComponentDiagnostics {
                tasks: Some(fdiagnostics::ComponentTasks::default()),
                ..Default::default()
            })
            .expect("sent diagnostics");
        assert_matches!(
            receiver.await,
            Ok(fdiagnostics::ComponentDiagnostics { tasks: Some(_), .. })
        );
    }

    #[fuchsia::test]
    async fn handles_connection_epitaph() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fcrunner::ComponentControllerMarker>()
                .unwrap();
        let controller = ComponentController::new(proxy, None);
        let epitaph_fut = controller.wait_for_epitaph();
        stream.control_handle().shutdown_with_epitaph(zx::Status::UNAVAILABLE);
        assert_eq!(epitaph_fut.await, zx::Status::UNAVAILABLE);
    }

    #[fuchsia::test]
    async fn handles_epitaph_for_closed_connection() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fcrunner::ComponentControllerMarker>()
                .unwrap();
        let controller = ComponentController::new(proxy, None);
        let epitaph_fut = controller.wait_for_epitaph();
        drop(stream);
        assert_eq!(epitaph_fut.await, zx::Status::PEER_CLOSED);
    }

    #[fuchsia::test]
    async fn handles_epitaph_for_dropped_controller() {
        let (proxy, _) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentControllerMarker>().unwrap();
        let controller = ComponentController::new(proxy, None);
        let epitaph_fut = controller.wait_for_epitaph();
        drop(controller);
        assert_eq!(epitaph_fut.await, zx::Status::PEER_CLOSED);
    }
}
