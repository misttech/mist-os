// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Error, Result},
    fidl::endpoints::{create_endpoints, create_proxy_and_stream},
    fidl_fuchsia_examples::{EchoMarker, EchoRequest},
    fidl_server::*,
    fuchsia_async as fasync,
    tracing::info,
};

#[derive(Clone)]
struct EchoHandler;

impl RequestHandler<EchoMarker> for EchoHandler {
    fn handle_request(&self, request: EchoRequest) -> Result<(), Error> {
        handle_echo_request(request)
    }
}

fn handle_echo_request(request: EchoRequest) -> Result<(), Error> {
    info!("{:?}", request);
    let EchoRequest::EchoString { value, responder } = request else { panic!(); };
    responder.send(&value)?;
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn should_accept_handler_function() -> Result<(), Error> {
    let (client, stream) = create_proxy_and_stream::<EchoMarker>()?;

    serve_detached(stream, handle_echo_request);

    assert_eq!(client.echo_string("message 1").await?, "message 1");
    assert_eq!(client.echo_string("message 2").await?, "message 2");
    assert_eq!(client.echo_string("message 3").await?, "message 3");
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn should_accept_handler_closure() -> Result<(), Error> {
    let (client, stream) = create_proxy_and_stream::<EchoMarker>()?;

    serve_detached(stream, |request| handle_echo_request(request));

    assert_eq!(client.echo_string("message 1").await?, "message 1");
    assert_eq!(client.echo_string("message 2").await?, "message 2");
    assert_eq!(client.echo_string("message 3").await?, "message 3");
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn should_accept_handler_object() -> Result<(), Error> {
    let (client, stream) = create_proxy_and_stream::<EchoMarker>()?;

    serve_detached(stream, EchoHandler);

    assert_eq!(client.echo_string("message 1").await?, "message 1");
    assert_eq!(client.echo_string("message 2").await?, "message 2");
    assert_eq!(client.echo_string("message 3").await?, "message 3");
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn should_serve_all_requests() -> Result<(), Error> {
    let (client, stream) = create_proxy_and_stream::<EchoMarker>()?;
    let server_fut = async move {
        serve(stream, EchoHandler).await.unwrap();
    };
    let client_fut = async move {
        assert_eq!(client.echo_string("message 1").await.unwrap(), "message 1");
        assert_eq!(client.echo_string("message 2").await.unwrap(), "message 2");
        assert_eq!(client.echo_string("message 3").await.unwrap(), "message 3");
    };
    futures::join!(server_fut, client_fut);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn should_stop_after_fidl_error() -> Result<(), Error> {
    let (client, server) = create_endpoints::<EchoMarker>();
    let server_fut = async move {
        let server = server.into_stream().unwrap();
        serve(server, EchoHandler).await.unwrap_err();
    };
    let client_fut = async move {
        // Write an invalid request.
        client.channel().write(&[0xab, 0xcd, 0xef], &mut []).unwrap();
        let client = client.into_proxy().unwrap();
        assert!(client.echo_string("message 2").await.unwrap_err().is_closed());
        assert!(client.echo_string("message 3").await.unwrap_err().is_closed());
    };
    futures::join!(server_fut, client_fut);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn should_stop_after_handler_returns_error() -> Result<(), Error> {
    let (client, stream) = create_proxy_and_stream::<EchoMarker>()?;
    let handler = |request| {
        let EchoRequest::EchoString { value, responder } = request else { panic!(); };
        if value == "message 2" {
            Err(anyhow!("failing on message 2"))
        } else {
            responder.send(&value)?;
            Ok(())
        }
    };
    let server_fut = async move {
        serve(stream, handler).await.unwrap_err();
    };
    let client_fut = async move {
        assert_eq!(client.echo_string("message 1").await.unwrap(), "message 1");
        assert!(client.echo_string("message 2").await.unwrap_err().is_closed());
        assert!(client.echo_string("message 3").await.unwrap_err().is_closed());
    };
    futures::join!(server_fut, client_fut);
    Ok(())
}
