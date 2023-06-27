// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::RequestStream;
use fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream};
use fuchsia_async as fasync;
use fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType};
use futures::{channel::oneshot, StreamExt};
use tracing::{debug, error};

/// Takes the startup handle for LIFECYCLE and returns a stream listening for Lifecycle FIDL
/// requests on it.
pub fn take_lifecycle_request_stream() -> LifecycleRequestStream {
    let lifecycle_handle_info = HandleInfo::new(HandleType::Lifecycle, 0);
    let lifecycle_handle = take_startup_handle(lifecycle_handle_info)
        .expect("must have been provided a lifecycle channel in procargs");
    let async_chan = fasync::Channel::from_channel(lifecycle_handle.into())
        .expect("Async channel conversion failed.");
    LifecycleRequestStream::from_channel(async_chan)
}

/// Serves the Lifecycle protocol from the component runtime used for controlled shutdown of the
/// archivist .
pub(crate) fn serve(
    mut request_stream: LifecycleRequestStream,
) -> (fasync::Task<()>, oneshot::Receiver<()>) {
    let (stop_sender, stop_recv) = oneshot::channel();
    let mut stop_sender = Some(stop_sender);

    let task = fasync::Task::spawn(async move {
        debug!("Awaiting request to close");
        while let Some(Ok(LifecycleRequest::Stop { .. })) = request_stream.next().await {
            debug!("Initiating shutdown.");
            if let Some(sender) = stop_sender.take() {
                if sender.send(()).is_err() {
                    error!("Archivist not shutting down. We lost the stop recv end");
                }
            }
        }
    });
    (task, stop_recv)
}
