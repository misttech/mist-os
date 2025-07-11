// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_component::client::connect_to_protocol;
use futures::AsyncReadExt as _;
use {fidl_fuchsia_net_http as fnet_http, fuchsia_async as fasync};

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("connect to fuchsia.net.http.Loader protocol")]
    ConnectToProtocol(#[source] anyhow::Error),

    #[error("fuchsia.net.http.Loader FIDL")]
    Fidl(#[from] fidl::Error),

    #[error("Loader fetch response error: {0:?}")]
    LoaderFetch(fnet_http::Error),

    #[error("No body socket in response")]
    NoBody,

    #[error("Failed to read bytes from the socket")]
    SocketRead(#[source] std::io::Error),
}

pub async fn fetch_url(url: impl Into<String>) -> Result<Vec<u8>, FetchError> {
    let loader =
        connect_to_protocol::<fnet_http::LoaderMarker>().map_err(FetchError::ConnectToProtocol)?;
    let request = fnet_http::Request {
        url: Some(url.into()),
        method: Some("GET".into()),
        ..Default::default()
    };
    let response = loader.fetch(request).await.map_err(FetchError::Fidl)?;
    if let Some(e) = response.error {
        return Err(FetchError::LoaderFetch(e));
    }
    let zx_socket = response.body.ok_or(FetchError::NoBody)?;
    let mut socket = fasync::Socket::from_socket(zx_socket);
    let mut body = Vec::new();
    let _bytes_received = socket.read_to_end(&mut body).await.map_err(FetchError::SocketRead)?;
    Ok(body)
}
