// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::endpoints::RequestStream;
use fidl::AsyncChannel;
use fidl_fuchsia_boot as fuchsia_boot;
use fidl_fuchsia_io as fuchsia_io;
use fuchsia_async as fasync;
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use futures::channel::oneshot;
use futures::future::{BoxFuture, Shared};
use futures::prelude::*;
use futures::FutureExt;
use std::sync::Arc;

pub struct SvcController {
    /// The last value seen in an `SvcStash.Store` event.
    svc_endpoint: Arc<Mutex<Option<fidl::endpoints::ServerEnd<fuchsia_io::DirectoryMarker>>>>,

    /// Receiver for epitaphs coming from the connection.
    epitaph_value_recv: Shared<oneshot::Receiver<zx::Status>>,

    // The task listening for events.
    _server_task: fasync::Task<Result<(), Error>>,
}

impl<'a> SvcController {
    pub fn new(channel: zx::Channel) -> Self {
        let (epitaph_sender, epitaph_value_recv) = oneshot::channel();
        let svc_endpoint = Arc::new(Mutex::new(None));

        let stream =
            fuchsia_boot::SvcStashRequestStream::from_channel(AsyncChannel::from_channel(channel));

        let events_fut = Self::serve(stream, epitaph_sender, svc_endpoint.clone());

        let event_listener_task = fasync::Task::spawn(events_fut);

        Self {
            svc_endpoint,
            epitaph_value_recv: epitaph_value_recv.shared(),
            _server_task: event_listener_task,
        }
    }

    pub fn svc_endpoint(
        &self,
    ) -> Arc<Mutex<Option<fidl::endpoints::ServerEnd<fuchsia_io::DirectoryMarker>>>> {
        self.svc_endpoint.clone()
    }

    pub fn wait_for_epitaph(&self) -> BoxFuture<'static, zx::Status> {
        let val = self.epitaph_value_recv.clone();
        async move { val.await.unwrap_or(zx::Status::PEER_CLOSED) }.boxed()
    }

    // We process the first message (and possible the only one) in the stream.
    pub async fn serve(
        mut stream: fuchsia_boot::SvcStashRequestStream,
        epitaph_sender: oneshot::Sender<zx::Status>,
        endpoint: Arc<Mutex<Option<fidl::endpoints::ServerEnd<fuchsia_io::DirectoryMarker>>>>,
    ) -> Result<(), Error> {
        let mut epitaph_sender = Some(epitaph_sender);
        while let Some(fuchsia_boot::SvcStashRequest::Store { svc_endpoint, control_handle: _ }) =
            stream.try_next().await?
        {
            *endpoint.lock() = Some(svc_endpoint.into());
        }
        epitaph_sender.take().and_then(|sender| sender.send(zx::Status::OK).ok());
        Ok(())
    }
}
