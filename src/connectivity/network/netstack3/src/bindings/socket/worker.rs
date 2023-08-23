// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::ControlFlow;

use async_utils::stream::OneOrMany;
use fidl::endpoints::{ControlHandle, RequestStream};
use futures::StreamExt as _;
use netstack3_core::SyncCtx;
use tracing::error;

use crate::bindings::{socket::SocketWorkerProperties, BindingsNonSyncCtxImpl, Ctx};

pub(crate) struct SocketWorker<Data> {
    ctx: Ctx,
    data: Data,
}

/// Handler for individual requests on a socket.
///
/// Implementations should hold on to a single socket from [`netstack3_core`]
/// and handle incoming requests for that socket. Note that this is a single
/// socket from the perspective of `netstack3_core`, though it might be used to
/// handle requests from multiple streams (if one of the possible values from
/// the stream is a "clone" request). In that case, requests from the
/// originating stream and any derived streams are multiplexed over a single
/// handler instance.
pub(crate) trait SocketWorkerHandler: Send + 'static {
    /// The type of request that this worker can handle.
    type Request: Send;

    /// A stream of requests that this handler might produce.
    ///
    /// If the requests for this handler can include a "clone" request, this
    /// is the type of new request streams that will be produced as a result.
    type RequestStream: RequestStream<Item = Result<Self::Request, fidl::Error>> + 'static;

    /// A responder for a "close" request.
    type CloseResponder: CloseResponder;

    /// Type of futures spawned by the worker that must be polled to
    /// completion when the worker terminates.
    type TaskFuture: futures::Future<Output = ()>;

    /// Handles a single request.
    ///
    /// Implementations should handle the incoming request in the appropriate
    /// fashion and respond with one of three values:
    /// - [`ControlFlow::Break`] to signal that the stream that produced the
    ///   request should be closed, with the responder to signal when the close
    ///   operation is complete;
    /// - [`ControlFlow::Continue`] to continue operating the request stream. If
    ///   `(Some(_), _)`, the yielded request stream is also operated on by this
    ///   same worker, as part of initiating a new workflow on the same socket
    ///   ("clone"). If `(_, Some(_)))`, a task spawned while handling the
    ///   request is being yielded to be polled to completion by the caller.
    fn handle_request(
        &mut self,
        ctx: &mut Ctx,
        request: Self::Request,
    ) -> ControlFlow<Self::CloseResponder, (Option<Self::RequestStream>, Option<Self::TaskFuture>)>;

    /// Closes the socket managed by this handler.
    ///
    /// This is called when the last stream for the managed socket is closed,
    /// and should be used to free up any resources in `netstack3_core` for the
    /// socket.
    fn close(
        self,
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    );
}

/// Abstraction over the "close" behavior for a socket.
pub(crate) trait CloseResponder: Send {
    /// Dispatches the provided response.
    ///
    /// Attempts to send the provided response, returning any error that arises.
    fn send(self, response: Result<(), i32>) -> Result<(), fidl::Error>;
}

impl<H: SocketWorkerHandler> SocketWorker<H> {
    /// Starts servicing events from the provided state and event stream.
    pub(crate) async fn serve_stream_with<
        F: FnOnce(
                &SyncCtx<BindingsNonSyncCtxImpl>,
                &mut BindingsNonSyncCtxImpl,
                SocketWorkerProperties,
            ) -> H
            + Send
            + 'static,
    >(
        mut ctx: Ctx,
        make_data: F,
        properties: SocketWorkerProperties,
        events: H::RequestStream,
    ) {
        let data = {
            let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();

            make_data(sync_ctx, non_sync_ctx, properties)
        };
        let worker = Self { ctx, data };

        // When the worker finishes, that means `self` goes out of scope and is
        // dropped, meaning that the event stream's underlying channel is
        // closed. If any errors occurred as a result of the closure, we just
        // log them.
        worker
            .handle_stream(events)
            .await
            .unwrap_or_else(|e: fidl::Error| error!("socket control request error: {:?}", e))
    }

    /// Handles a stream of POSIX socket requests.
    ///
    /// Returns when getting the first `Close` request.
    async fn handle_stream(mut self, events: H::RequestStream) -> Result<(), fidl::Error> {
        let mut request_streams = OneOrMany::new(events.into_future());
        let mut tasks = futures::stream::FuturesUnordered::new();
        let respond_close = loop {
            let request = futures::select! {
                request = request_streams.next() => request,
                // Continuously poll tasks to pop them from the collection and
                // avoid unbounded memory growth.
                r = tasks.by_ref().next() => {
                    r.unwrap_or(());
                    continue;
                }
            };
            let Some((request, request_stream)) = request else {
                // There are no more streams left, so there's no close responder
                // to defer responding to.
                break None
            };
            let request = match request {
                None => {
                    // The stream ended without a close request, so no need to
                    // defer responding to it for later.
                    continue;
                }
                Some(Err(e)) => {
                    tracing::error!("got error while polling for requests: {}", e);
                    // Continuing implicitly drops the request stream that
                    // produced the error, which would otherwise be re-enqueued
                    // below.
                    continue;
                }
                Some(Ok(t)) => t,
            };
            let Self { ctx, data } = &mut self;
            match data.handle_request(ctx, request) {
                ControlFlow::Continue((stream, task)) => {
                    if let Some(stream) = stream {
                        request_streams.push(stream.into_future());
                    }
                    if let Some(task) = task {
                        tasks.push(task);
                    }
                }
                ControlFlow::Break(close_responder) => {
                    let respond_close = move || {
                        close_responder
                            .send(Ok(()))
                            .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                        request_stream.control_handle().shutdown();
                    };
                    if request_streams.is_empty() {
                        // Save the final close request to be performed after
                        // the socket state is removed from Core.
                        break Some(respond_close);
                    } else {
                        // This isn't the last stream for the socket, so we can
                        // respond to the close request immediately since it
                        // only closed the stream, not the underlying socket.
                        respond_close();
                        continue;
                    }
                }
            };
            request_streams.push(request_stream.into_future());
        };

        let Self { mut ctx, data } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        data.close(sync_ctx, non_sync_ctx);

        if let Some(respond_close) = respond_close {
            respond_close();
        }
        // Socket should be closed, join on all spawned tasks.
        tasks.collect::<()>().await;
        Ok(())
    }
}
