// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error, Result},
    fidl::endpoints::{ProtocolMarker, Request, RequestStream},
    fuchsia_async as fasync,
    futures::TryStreamExt,
    tracing::error,
};

/// RequestHandler handles incoming FIDL requests.
pub trait RequestHandler<P: ProtocolMarker>: Send {
    /// Handles a request. If it returns an error, the server will shut down.
    fn handle_request(&self, request: Request<P>) -> Result<(), Error>;
}

impl<P, F> RequestHandler<P> for F
where
    P: ProtocolMarker,
    F: Fn(Request<P>) -> Result<(), Error> + Send,
{
    fn handle_request(&self, request: Request<P>) -> Result<(), Error> {
        self(request)
    }
}

/// Serves all requests on `stream` using `handler`.
///
/// Stops and returns an error if a FIDL error occurs while reading a request,
/// or if `handler` fails. The caller should log this error.
pub async fn serve<S, H>(mut stream: S, handler: H) -> Result<(), Error>
where
    S: RequestStream,
    H: RequestHandler<S::Protocol>,
{
    while let Some(request) = stream
        .try_next()
        .await
        .with_context(|| format!("error reading {} request", S::Protocol::DEBUG_NAME))?
    {
        handler
            .handle_request(request)
            .with_context(|| format!("error handling {} request", S::Protocol::DEBUG_NAME))?;
    }
    Ok(())
}

/// Runs the server in the background and logs the error if one occurs.
///
/// This implements the most common case where FIDL service authors serve a
/// RequestStream on some remote task using `Task::spawn(...).detach()`.
///
/// When using this function, prefer prefixing your program's log messages by
/// annotating `main()` with one of the following:
///
/// ```
/// #[fuchsia::main(logging = true)]
/// #[fuchsia::main(logging_prefix = "my_prefix")]
/// ```
pub fn serve_detached<S, H>(stream: S, handler: H)
where
    S: RequestStream + 'static,
    H: RequestHandler<S::Protocol> + 'static,
{
    fasync::Task::spawn(async move {
        if let Err(err) = serve(stream, handler).await {
            error!("{:?}", err);
        }
    })
    .detach();
}
