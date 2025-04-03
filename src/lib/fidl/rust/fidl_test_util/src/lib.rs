// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Utilities for tests that interact with fidl.

use fidl::endpoints::{create_proxy_and_stream, Proxy, Request, RequestStream};
use fuchsia_async::Task;
use futures::{FutureExt, TryFutureExt, TryStreamExt};
use log::error;

/// Utility that spawns a new task to handle requests of a particular type, requiring a
/// singlethreaded executor. The requests are handled one at a time.
pub fn spawn_local_stream_handler<P, F, Fut>(f: F) -> P
where
    P: Proxy,
    F: FnMut(Request<P::Protocol>) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
{
    let (proxy, stream) = create_proxy_and_stream::<P::Protocol>();
    Task::local(for_each_or_log(stream, f)).detach();
    proxy
}

/// Utility that spawns a new task to handle requests of a particular type. The request handler
/// must be threadsafe. The requests are handled one at a time.
pub fn spawn_stream_handler<P, F, Fut>(f: F) -> P
where
    P: Proxy,
    F: FnMut(Request<P::Protocol>) -> Fut + 'static + Send,
    Fut: Future<Output = ()> + 'static + Send,
{
    let (proxy, stream) = create_proxy_and_stream::<P::Protocol>();
    Task::spawn(for_each_or_log(stream, f)).detach();
    proxy
}

fn for_each_or_log<St, F, Fut>(stream: St, mut f: F) -> impl Future<Output = ()>
where
    St: RequestStream,
    F: FnMut(St::Ok) -> Fut,
    Fut: Future<Output = ()>,
{
    stream
        .try_for_each(move |r| f(r).map(Ok))
        .unwrap_or_else(|e| error!("FIDL stream handler failed: {}", e))
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_test_placeholders::{EchoProxy, EchoRequest};

    #[fuchsia::test]
    async fn test_spawn_local_stream_handler() {
        let f = |req| {
            let EchoRequest::EchoString { value, responder } = req;
            async move {
                responder.send(Some(&value.unwrap())).expect("responder failed");
            }
        };
        let proxy: EchoProxy = spawn_local_stream_handler(f);
        let res = proxy.echo_string(Some("hello world")).await.expect("echo failed");
        assert_eq!(res, Some("hello world".to_string()));
        let res = proxy.echo_string(Some("goodbye world")).await.expect("echo failed");
        assert_eq!(res, Some("goodbye world".to_string()));
    }

    #[fuchsia::test(threads = 2)]
    async fn test_spawn_stream_handler() {
        let f = |req| {
            let EchoRequest::EchoString { value, responder } = req;
            async move {
                responder.send(Some(&value.unwrap())).expect("responder failed");
            }
        };
        let proxy: EchoProxy = spawn_stream_handler(f);
        let res = proxy.echo_string(Some("hello world")).await.expect("echo failed");
        assert_eq!(res, Some("hello world".to_string()));
        let res = proxy.echo_string(Some("goodbye world")).await.expect("echo failed");
        assert_eq!(res, Some("goodbye world".to_string()));
    }
}
