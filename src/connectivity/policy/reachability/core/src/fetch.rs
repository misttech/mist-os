// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context};
use async_trait::async_trait;
use fuchsia_async::net::TcpStream;
use fuchsia_async::TimeoutExt;

use futures::{AsyncReadExt, AsyncWriteExt, TryFutureExt};
use log::warn;
use std::net;

const FETCH_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(10);

fn http_request(path: &str, host: &str) -> String {
    [
        &format!("HEAD {path} HTTP/1.1"),
        &format!("host: {host}"),
        "connection: close",
        "user-agent: fuchsia reachability probe",
    ]
    .join("\r\n")
        + "\r\n\r\n"
}

async fn fetch<FA: FetchAddr + std::marker::Sync>(
    interface_name: &str,
    host: &str,
    path: &str,
    addr: &FA,
) -> anyhow::Result<u8> {
    let timeout = zx::MonotonicInstant::after(FETCH_TIMEOUT);
    let addr = addr.as_socket_addr();
    let socket = socket2::Socket::new(
        match addr {
            net::SocketAddr::V4(_) => socket2::Domain::IPV4,
            net::SocketAddr::V6(_) => socket2::Domain::IPV6,
        },
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .context("while constructing socket")?;
    socket.bind_device(Some(interface_name.as_bytes()))?;
    let mut stream = TcpStream::connect_from_raw(socket, addr)
        .context("while constructing tcp stream")?
        .map_err(|e| format_err!("Opening TcpStream connection failed: {e:?}"))
        .on_timeout(timeout, || Err(format_err!("Opening TcpStream timed out")))
        .await?;
    let message = http_request(path, host);
    stream
        .write_all(message.as_bytes())
        .map_err(|e| format_err!("Writing to TcpStream failed: {e:?}"))
        .on_timeout(timeout, || Err(format_err!("Writing data to TcpStream timed out")))
        .await?;

    let mut bytes = Vec::new();
    let _: usize = stream
        .read_to_end(&mut bytes)
        .map_err(|e| format_err!("Reading response from TcpStream failed: {e:?}"))
        .on_timeout(timeout, || Err(format_err!("Reading response from TcpStream timed out")))
        .await?;
    let resp = String::from_utf8(bytes)?;
    let first_line = resp.split("\r\n").next().expect("split always returns at least one item");
    if let [http, code, ..] = first_line.split(' ').collect::<Vec<_>>().as_slice() {
        if !http.starts_with("HTTP/") {
            return Err(format_err!("Response header malformed: {first_line}"));
        }
        Ok(code.parse().map_err(|e| format_err!("While parsing status code: {e:?}"))?)
    } else {
        Err(format_err!("Response header malformed: {first_line}"))
    }
}

pub trait FetchAddr {
    fn as_socket_addr(&self) -> net::SocketAddr;
}

impl FetchAddr for net::SocketAddr {
    fn as_socket_addr(&self) -> net::SocketAddr {
        *self
    }
}

impl FetchAddr for net::IpAddr {
    fn as_socket_addr(&self) -> net::SocketAddr {
        net::SocketAddr::from((*self, 80))
    }
}

#[async_trait]
pub trait Fetch {
    async fn fetch<FA: FetchAddr + std::marker::Sync>(
        &self,
        interface_name: &str,
        host: &str,
        path: &str,
        addr: &FA,
    ) -> Option<u8>;
}

pub struct Fetcher;

#[async_trait]
impl Fetch for Fetcher {
    async fn fetch<FA: FetchAddr + std::marker::Sync>(
        &self,
        interface_name: &str,
        host: &str,
        path: &str,
        addr: &FA,
    ) -> Option<u8> {
        let r = fetch(interface_name, host, path, addr).await;
        match r {
            Ok(code) => Some(code),
            Err(e) => {
                warn!("error while fetching {host}{path}: {e:?}");
                None
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::net::{Ipv4Addr, SocketAddr};
    use std::pin::pin;

    use fuchsia_async::net::TcpListener;
    use fuchsia_async::{self as fasync};
    use futures::future::Fuse;
    use futures::io::BufReader;
    use futures::{AsyncBufReadExt, FutureExt, StreamExt};
    use test_case::test_case;

    fn server() -> anyhow::Result<(SocketAddr, Fuse<impl futures::Future<Output = Vec<String>>>)> {
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0);
        let listener = TcpListener::bind(&addr).context("binding TCP")?;
        let addr = listener.local_addr()?;

        let server_fut = async {
            let timeout = zx::MonotonicInstant::after(FETCH_TIMEOUT);
            let mut incoming = listener.accept_stream();
            if let Some(result) = incoming
                .next()
                .on_timeout(timeout, || panic!("timeout waiting for connection"))
                .await
            {
                let (stream, _addr) = result.expect("accept incoming TCP connection");
                let mut stream = BufReader::new(stream);
                let mut request = Vec::new();
                loop {
                    let mut s = String::new();
                    let _: usize = stream
                        .read_line(&mut s)
                        .on_timeout(timeout, || panic!("timeout waiting to read data"))
                        .await
                        .expect("read data");
                    if s == "\r\n" {
                        break;
                    }
                    request.push(s.trim().to_string());
                }
                let data = "HTTP/1.1 200 OK\r\n\r\n".to_string();
                stream
                    .write_all(data.as_bytes())
                    .on_timeout(timeout, || panic!("timeout waiting to write response"))
                    .await
                    .expect("reply to request");
                request
            } else {
                Vec::new()
            }
        }
        .fuse();

        Ok((addr, server_fut))
    }

    #[test_case("http://reachability.test/"; "base path")]
    #[test_case("http://reachability.test/path/"; "sub path")]
    #[fasync::run_singlethreaded(test)]
    async fn test_fetch(url_str: &'static str) -> anyhow::Result<()> {
        let url = url::Url::parse(url_str)?;
        let (addr, server_fut) = server()?;
        let domain = url.host().expect("no host").to_string();
        let path = url.path().to_string();

        let mut fetch_fut = pin!(fetch("", &domain, &path, &addr).fuse());

        let mut server_fut = pin!(server_fut);

        let mut request = None;
        let result = loop {
            futures::select! {
                req = server_fut => request = Some(req),
                result = fetch_fut => break result
            };
        };

        assert!(result.is_ok(), "Expected OK, got: {result:?}");
        assert_eq!(result.ok(), Some(200));
        let request = request.expect("no request body");
        assert!(request.contains(&format!("HEAD {path} HTTP/1.1")));
        assert!(request.contains(&format!("host: {domain}")));

        Ok(())
    }
}
