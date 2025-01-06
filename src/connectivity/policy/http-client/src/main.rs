// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context as _;
use fidl::endpoints::ServerEnd;
use fidl::prelude::*;
use fuchsia_async::{self as fasync, TimeoutExt as _};
use fuchsia_component::server::{Item, ServiceFs, ServiceFsDir};
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::future::Either;
use futures::prelude::*;
use futures::StreamExt;
use http_client_config::Config;
use log::{debug, error, info, trace};
use std::str::FromStr as _;
use zx::{self as zx, AsHandleRef};
use {
    fidl_fuchsia_io as fio, fidl_fuchsia_net_http as net_http,
    fidl_fuchsia_process_lifecycle as flifecycle, fuchsia_hyper as fhyper,
};

static MAX_REDIRECTS: u8 = 10;
static DEFAULT_DEADLINE_DURATION: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(15);

fn to_status_line(version: hyper::Version, status: hyper::StatusCode) -> Vec<u8> {
    match status.canonical_reason() {
        None => format!("{:?} {}", version, status.as_str()),
        Some(canonical_reason) => format!("{:?} {} {}", version, status.as_str(), canonical_reason),
    }
    .as_bytes()
    .to_vec()
}

fn tcp_options() -> fhyper::TcpOptions {
    let mut options: fhyper::TcpOptions = std::default::Default::default();

    // Use TCP keepalive to notice stuck connections.
    // After 60s with no data received send a probe every 15s.
    options.keepalive_idle = Some(std::time::Duration::from_secs(60));
    options.keepalive_interval = Some(std::time::Duration::from_secs(15));
    // After 8 probes go unacknowledged treat the connection as dead.
    options.keepalive_count = Some(8);

    options
}

struct RedirectInfo {
    url: Option<hyper::Uri>,
    referrer: Option<hyper::Uri>,
    method: hyper::Method,
}

fn redirect_info(
    old_uri: &hyper::Uri,
    method: &hyper::Method,
    hyper_response: &hyper::Response<hyper::Body>,
) -> Option<RedirectInfo> {
    if hyper_response.status().is_redirection() {
        Some(RedirectInfo {
            url: hyper_response
                .headers()
                .get(hyper::header::LOCATION)
                .and_then(|loc| calculate_redirect(old_uri, loc)),
            referrer: hyper_response
                .headers()
                .get(hyper::header::REFERER)
                .and_then(|loc| calculate_redirect(old_uri, loc)),
            method: if hyper_response.status() == hyper::StatusCode::SEE_OTHER {
                hyper::Method::GET
            } else {
                method.clone()
            },
        })
    } else {
        None
    }
}

async fn to_success_response(
    current_url: &hyper::Uri,
    current_method: &hyper::Method,
    mut hyper_response: hyper::Response<hyper::Body>,
    scope: vfs::execution_scope::ExecutionScope,
) -> net_http::Response {
    let redirect_info = redirect_info(current_url, current_method, &hyper_response);
    let headers = hyper_response
        .headers()
        .iter()
        .map(|(name, value)| net_http::Header {
            name: name.as_str().as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        })
        .collect();

    let (tx, rx) = zx::Socket::create_stream();
    let response = net_http::Response {
        error: None,
        body: Some(rx),
        final_url: Some(current_url.to_string()),
        status_code: Some(hyper_response.status().as_u16() as u32),
        status_line: Some(to_status_line(hyper_response.version(), hyper_response.status())),
        headers: Some(headers),
        redirect: redirect_info.map(|info| net_http::RedirectTarget {
            method: Some(info.method.to_string()),
            url: info.url.map(|u| u.to_string()),
            referrer: info.referrer.map(|r| r.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let _ = scope.spawn(async move {
        let hyper_body = hyper_response.body_mut();
        while let Some(chunk) = hyper_body.next().await {
            if let Ok(chunk) = chunk {
                let mut offset: usize = 0;
                while offset < chunk.len() {
                    let pending = match tx.wait_handle(
                        zx::Signals::SOCKET_PEER_CLOSED | zx::Signals::SOCKET_WRITABLE,
                        zx::MonotonicInstant::INFINITE,
                    ) {
                        Err(status) => {
                            error!("tx.wait() failed - status: {}", status);
                            return;
                        }
                        Ok(pending) => pending,
                    };
                    if pending.contains(zx::Signals::SOCKET_PEER_CLOSED) {
                        info!("tx.wait() saw signal SOCKET_PEER_CLOSED");
                        return;
                    }
                    assert!(pending.contains(zx::Signals::SOCKET_WRITABLE));
                    let written = match tx.write(&chunk[offset..]) {
                        Err(status) => {
                            // Because of the wait above, we shouldn't ever see SHOULD_WAIT here, but to avoid
                            // brittle-ness, continue and wait again in that case.
                            if status == zx::Status::SHOULD_WAIT {
                                error!("Saw SHOULD_WAIT despite waiting first - expected now? - continuing");
                                continue;
                            }
                            info!("tx.write() failed - status: {}", status);
                            return;
                        }
                        Ok(written) => written,
                    };
                    offset += written;
                }
            }
        }
    });

    response
}

fn to_fidl_error(error: &hyper::Error) -> net_http::Error {
    #[allow(clippy::if_same_then_else)] // TODO(https://fxbug.dev/42176989)
    if error.is_parse() {
        net_http::Error::UnableToParse
    } else if error.is_user() {
        //TODO(zmbush): handle this case.
        net_http::Error::Internal
    } else if error.is_canceled() {
        //TODO(zmbush): handle this case.
        net_http::Error::Internal
    } else if error.is_closed() {
        net_http::Error::ChannelClosed
    } else if error.is_connect() {
        net_http::Error::Connect
    } else if error.is_incomplete_message() {
        //TODO(zmbush): handle this case.
        net_http::Error::Internal
    } else if error.is_body_write_aborted() {
        //TODO(zmbush): handle this case.
        net_http::Error::Internal
    } else {
        net_http::Error::Internal
    }
}

fn to_error_response(error: net_http::Error) -> net_http::Response {
    net_http::Response {
        error: Some(error),
        body: None,
        final_url: None,
        status_code: None,
        status_line: None,
        headers: None,
        redirect: None,
        ..Default::default()
    }
}

struct Loader {
    method: hyper::Method,
    url: hyper::Uri,
    headers: hyper::HeaderMap,
    body: Vec<u8>,
    deadline: fasync::MonotonicInstant,
    scope: vfs::execution_scope::ExecutionScope,
}

impl Loader {
    async fn new(
        req: net_http::Request,
        scope: vfs::execution_scope::ExecutionScope,
    ) -> Result<Self, anyhow::Error> {
        let net_http::Request { method, url, headers, body, deadline, .. } = req;
        let method = method.as_ref().map(|method| hyper::Method::from_str(method)).transpose()?;
        let method = method.unwrap_or(hyper::Method::GET);
        if let Some(url) = url {
            let url = hyper::Uri::try_from(url)?;
            let headers = headers
                .unwrap_or_else(|| vec![])
                .into_iter()
                .map(|net_http::Header { name, value }| {
                    let name = hyper::header::HeaderName::from_bytes(&name)?;
                    let value = hyper::header::HeaderValue::from_bytes(&value)?;
                    Ok((name, value))
                })
                .collect::<Result<hyper::HeaderMap, anyhow::Error>>()?;

            let body = match body {
                Some(net_http::Body::Buffer(buffer)) => {
                    let mut bytes = vec![0; buffer.size as usize];
                    buffer.vmo.read(&mut bytes, 0)?;
                    bytes
                }
                Some(net_http::Body::Stream(socket)) => {
                    let mut stream = fasync::Socket::from_socket(socket)
                        .into_datagram_stream()
                        .map(|r| r.context("reading from datagram stream"));
                    let mut bytes = Vec::new();
                    while let Some(chunk) = stream.next().await {
                        bytes.extend(chunk?);
                    }
                    bytes
                }
                None => Vec::new(),
            };

            let deadline = deadline
                .map(|deadline| fasync::MonotonicInstant::from_nanos(deadline))
                .unwrap_or_else(|| fasync::MonotonicInstant::after(DEFAULT_DEADLINE_DURATION));

            trace!("Starting request {} {}", method, url);

            Ok(Loader { method, url, headers, body, deadline, scope })
        } else {
            Err(anyhow::Error::msg("Request missing URL"))
        }
    }

    fn build_request(&self) -> hyper::Request<hyper::Body> {
        let Self { method, url, headers, body, deadline: _, scope: _ } = self;
        let mut request = hyper::Request::new(body.clone().into());
        *request.method_mut() = method.clone();
        *request.uri_mut() = url.clone();
        *request.headers_mut() = headers.clone();
        request
    }

    async fn start(mut self, loader_client: net_http::LoaderClientProxy) -> Result<(), zx::Status> {
        let client = fhyper::new_https_client_from_tcp_options(tcp_options());
        loop {
            break match client.request(self.build_request()).await {
                Ok(hyper_response) => {
                    let redirect = redirect_info(&self.url, &self.method, &hyper_response);
                    if let Some(redirect) = redirect {
                        if let Some(url) = redirect.url {
                            self.url = url;
                            self.method = redirect.method;
                            trace!(
                                "Reporting redirect to OnResponse: {} {}",
                                self.method,
                                self.url
                            );
                            let response = to_success_response(
                                &self.url,
                                &self.method,
                                hyper_response,
                                self.scope.clone(),
                            )
                            .await;
                            match loader_client.on_response(response).await {
                                Ok(()) => {}
                                Err(e) => {
                                    debug!("Not redirecting because: {}", e);
                                    break Ok(());
                                }
                            };
                            trace!("Redirect allowed to {} {}", self.method, self.url);
                            continue;
                        }
                    }
                    let response = to_success_response(
                        &self.url,
                        &self.method,
                        hyper_response,
                        self.scope.clone(),
                    )
                    .await;
                    // We don't care if on_response returns an error since this is the last
                    // callback.
                    let _: Result<_, _> = loader_client.on_response(response).await;
                    Ok(())
                }
                Err(error) => {
                    info!("Received network level error from hyper: {}", error);
                    // We don't care if on_response returns an error since this is the last
                    // callback.
                    let _: Result<_, _> =
                        loader_client.on_response(to_error_response(to_fidl_error(&error))).await;
                    Ok(())
                }
            };
        }
    }

    async fn fetch(
        mut self,
    ) -> Result<(hyper::Response<hyper::Body>, hyper::Uri, hyper::Method), net_http::Error> {
        let deadline = self.deadline;
        if deadline < fasync::MonotonicInstant::now() {
            return Err(net_http::Error::DeadlineExceeded);
        }
        let client = fhyper::new_https_client_from_tcp_options(tcp_options());

        async move {
            let mut redirects = 0;
            loop {
                break match client.request(self.build_request()).await {
                    Ok(hyper_response) => {
                        if redirects != MAX_REDIRECTS {
                            let redirect = redirect_info(&self.url, &self.method, &hyper_response);
                            if let Some(redirect) = redirect {
                                if let Some(url) = redirect.url {
                                    self.url = url;
                                    self.method = redirect.method;
                                    trace!("Redirecting to {} {}", self.method, self.url);
                                    redirects += 1;
                                    continue;
                                }
                            }
                        }
                        Ok((hyper_response, self.url, self.method))
                    }
                    Err(e) => {
                        info!("Received network level error from hyper: {}", e);
                        Err(to_fidl_error(&e))
                    }
                };
            }
        }
        .on_timeout(deadline, || Err(net_http::Error::DeadlineExceeded))
        .await
    }
}

fn calculate_redirect(
    old_url: &hyper::Uri,
    location: &hyper::header::HeaderValue,
) -> Option<hyper::Uri> {
    let old_parts = old_url.clone().into_parts();
    let mut new_parts = hyper::Uri::try_from(location.as_bytes()).ok()?.into_parts();
    if new_parts.scheme.is_none() {
        new_parts.scheme = old_parts.scheme;
    }
    if new_parts.authority.is_none() {
        new_parts.authority = old_parts.authority;
    }
    Some(hyper::Uri::from_parts(new_parts).ok()?)
}

async fn loader_server(
    stream: net_http::LoaderRequestStream,
    idle_timeout: fasync::MonotonicDuration,
) -> Result<(), anyhow::Error> {
    let background_tasks = vfs::execution_scope::ExecutionScope::new();
    let (stream, unbind_if_stalled) = detect_stall::until_stalled(stream, idle_timeout);

    stream
        .err_into::<anyhow::Error>()
        .try_for_each_concurrent(None, |message| {
            let scope = background_tasks.clone();
            async move {
                match message {
                    net_http::LoaderRequest::Fetch { request, responder } => {
                        debug!(
                            "Fetch request received (url: {}): {:?}",
                            request
                                .url
                                .as_ref()
                                .and_then(|url| Some(url.as_str()))
                                .unwrap_or_default(),
                            request
                        );
                        let result = Loader::new(request, scope.clone()).await?.fetch().await;
                        responder.send(match result {
                            Ok((hyper_response, final_url, final_method)) => {
                                to_success_response(
                                    &final_url,
                                    &final_method,
                                    hyper_response,
                                    scope.clone(),
                                )
                                .await
                            }
                            Err(error) => to_error_response(error),
                        })?;
                    }
                    net_http::LoaderRequest::Start { request, client, control_handle } => {
                        debug!(
                            "Start request received (url: {}): {:?}",
                            request
                                .url
                                .as_ref()
                                .and_then(|url| Some(url.as_str()))
                                .unwrap_or_default(),
                            request
                        );
                        Loader::new(request, scope).await?.start(client.into_proxy()).await?;
                        control_handle.shutdown();
                    }
                }
                Ok(())
            }
        })
        .await?;

    background_tasks.wait().await;

    // If the connection did not close or receive new messages within the timeout, send it
    // over to component manager to wait for it on our behalf.
    if let Ok(Some(server_end)) = unbind_if_stalled.await {
        fuchsia_component::client::connect_channel_to_protocol_at::<net_http::LoaderMarker>(
            server_end.into(),
            "/escrow",
        )?;
    }

    Ok(())
}

enum HttpServices {
    Loader(net_http::LoaderRequestStream),
}

#[fuchsia::main]
pub async fn main() -> Result<(), anyhow::Error> {
    log::info!("http-client starting");

    // TODO(https://fxbug.dev/333080598): This is quite some boilerplate to escrow the outgoing dir.
    // Design some library function to handle the lifecycle requests.
    let lifecycle =
        fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::Lifecycle, 0)).unwrap();
    let lifecycle: zx::Channel = lifecycle.into();
    let lifecycle: ServerEnd<flifecycle::LifecycleMarker> = lifecycle.into();
    let (mut lifecycle_request_stream, lifecycle_control_handle) =
        lifecycle.into_stream_and_control_handle();

    let config = Config::take_from_startup_handle();
    let idle_timeout = if config.stop_on_idle_timeout_millis >= 0 {
        fasync::MonotonicDuration::from_millis(config.stop_on_idle_timeout_millis)
    } else {
        fasync::MonotonicDuration::INFINITE
    };

    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> =
        fs.take_and_serve_directory_handle()?.dir("svc").add_fidl_service(HttpServices::Loader);

    let lifecycle_task = async move {
        let Some(Ok(request)) = lifecycle_request_stream.next().await else {
            return std::future::pending::<()>().await;
        };
        match request {
            flifecycle::LifecycleRequest::Stop { .. } => {
                // TODO(https://fxbug.dev/332341289): If the framework asks us to stop, we still
                // end up dropping requests. If we teach the `ServiceFs` etc. libraries to skip
                // the timeout when this happens, we can cleanly stop the component.
                return;
            }
        }
    };

    let outgoing_dir_task = async move {
        fs.until_stalled(idle_timeout)
            .for_each_concurrent(None, |item| async {
                match item {
                    Item::Request(services, _active_guard) => {
                        let HttpServices::Loader(stream) = services;
                        loader_server(stream, idle_timeout)
                            .await
                            .unwrap_or_else(|e: anyhow::Error| error!("{:?}", e))
                    }
                    Item::Stalled(outgoing_directory) => {
                        escrow_outgoing(lifecycle_control_handle.clone(), outgoing_directory.into())
                    }
                }
            })
            .await;
    };

    match futures::future::select(lifecycle_task.boxed_local(), outgoing_dir_task.boxed_local())
        .await
    {
        Either::Left(_) => log::info!("http-client stopping because we are told to stop"),
        Either::Right(_) => log::info!("http-client stopping because it is idle"),
    }

    Ok(())
}

/// Escrow the outgoing directory server endpoint to component manager, such that we will receive
/// the same server endpoint on the next execution.
fn escrow_outgoing(
    lifecycle_control_handle: flifecycle::LifecycleControlHandle,
    outgoing_dir: ServerEnd<fio::DirectoryMarker>,
) {
    let outgoing_dir = Some(outgoing_dir);
    lifecycle_control_handle
        .send_on_escrow(flifecycle::LifecycleOnEscrowRequest { outgoing_dir, ..Default::default() })
        .unwrap();
}
