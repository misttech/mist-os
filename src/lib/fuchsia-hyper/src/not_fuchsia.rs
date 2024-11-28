// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{parse_ip_addr, HyperConnectorFuture, SocketOptions, TcpOptions, TcpStream};
use futures::io;
use http::uri::{Scheme, Uri};
use hyper::service::Service;
use netext::TokioAsyncReadExt;
use rustls::RootCertStore;
use std::net::ToSocketAddrs;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};
use tokio::net;
use tracing::warn;

pub fn new_root_cert_store() -> Arc<RootCertStore> {
    // It can be expensive to parse the certs, so cache them
    static ROOT_STORE: LazyLock<Arc<RootCertStore>> = LazyLock::new(|| {
        let mut root_store = rustls::RootCertStore::empty();

        let certs = match rustls_native_certs::load_native_certs() {
            Ok(certs) => certs,
            Err(err) => {
                panic!("Could not load TLS CA certificates from platform root store: {err}")
            }
        };

        if !certs.is_empty() {
            let (added, ignored) = root_store.add_parsable_certificates(&certs);

            if ignored != 0 {
                warn!("Failed to load {ignored} certificates into the root store");
            }

            if added == 0 {
                panic!("Unable to load any TLS CA certificates from platform root store")
            }
        }

        Arc::new(root_store)
    });

    Arc::clone(&ROOT_STORE)
}

/// A Async-std-compatible implementation of hyper's `Connect` trait which allows
/// creating a TcpStream to a particular destination.
#[derive(Clone, Debug)]
pub struct HyperConnector {
    tcp_options: TcpOptions,
    socket_options: SocketOptions,
}

impl From<(TcpOptions, SocketOptions)> for HyperConnector {
    fn from((tcp_options, socket_options): (TcpOptions, SocketOptions)) -> Self {
        Self { tcp_options, socket_options }
    }
}

impl HyperConnector {
    pub fn new() -> Self {
        Self::from_tcp_options(TcpOptions::default())
    }

    pub fn from_tcp_options(tcp_options: TcpOptions) -> Self {
        Self { tcp_options, socket_options: SocketOptions::default() }
    }
}

impl Service<Uri> for HyperConnector {
    type Response = TcpStream;
    type Error = std::io::Error;
    type Future = HyperConnectorFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let self_ = self.clone();
        HyperConnectorFuture { fut: Box::pin(async move { self_.call_async(dst).await }) }
    }
}

impl HyperConnector {
    async fn call_async(&self, dst: Uri) -> Result<TcpStream, io::Error> {
        let port = match dst.port() {
            Some(p) => p.as_u16(),
            None => {
                if dst.scheme() == Some(&Scheme::HTTPS) {
                    443
                } else {
                    80
                }
            }
        };

        let host = match dst.host() {
            Some(host) => host,
            _ => return Err(io::Error::new(io::ErrorKind::Other, "missing host in Uri")),
        };

        let addr = parse_ip_addr(host, port, |_| async {
            Err(io::Error::new(io::ErrorKind::Other, "does not yet support non-integer zone ids"))
        })
        .await?;

        if self.socket_options.bind_device.is_some() {
            unimplemented!("TODO(https://fxbug.dev/42083862) fuchsia-hyper does not support bind_device on non-fuchsia devices");
        }

        let stream = if let Some(addr) = addr {
            net::TcpStream::connect(addr).await?
        } else {
            resolve_host_port(host, port).await?
        };
        let () = self.tcp_options.apply(&stream)?;

        Ok(TcpStream { stream: stream.into_multithreaded_futures_stream() })
    }
}

/// Resolve a hostname into an address.
async fn resolve_host_port(host: &str, port: u16) -> Result<net::TcpStream, io::Error> {
    // TODO(https://fxbug.dev/42075095): Implement happy eyeballs algorithm to make this
    // more efficient.
    let mut last_err = None;
    for addr in (host, port).to_socket_addrs()? {
        match net::TcpStream::connect(addr).await {
            Ok(stream) => {
                return Ok(stream);
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }

    if let Some(err) = last_err {
        Err(err)
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "destination resolved to no address"))
    }
}

////////////////////////////////////////////////////////////////////////////////
///// tests

#[cfg(test)]
mod test {
    use crate::*;
    use anyhow::{Error, Result};
    use futures::future::BoxFuture;
    use futures::stream::FuturesUnordered;
    use futures::{StreamExt, TryFutureExt, TryStreamExt};
    use hyper::body::HttpBody;
    use hyper::server::accept::from_stream;
    use hyper::server::Server;
    use hyper::service::{make_service_fn, service_fn};
    use hyper::{Response, StatusCode};
    use std::convert::Infallible;
    use std::io::Write;
    use tokio::net::TcpListener;

    trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send {}
    impl<T> AsyncReadWrite for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send {}

    async fn fetch_url<W: Write>(url: hyper::Uri, mut buffer: W) -> Result<StatusCode> {
        let client = new_https_client();

        let mut res = client.get(url).await?;
        let status = res.status();

        if status == StatusCode::OK {
            while let Some(next) = res.data().await {
                let chunk = next?;
                buffer.write_all(&chunk)?;
            }
            buffer.flush()?;
        }

        Ok(status)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_download_succeeds() -> Result<()> {
        let (listener, addr) = {
            let addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0);
            let listener = TcpListener::bind(&addr).await.unwrap();
            let local_addr = listener.local_addr().unwrap();
            (listener, local_addr)
        };

        #[cfg(target_os = "fuchsia")]
        let listener =
            listener.incoming().map_err(Error::from).map_ok(|conn| TcpStream { stream: conn });
        #[cfg(not(target_os = "fuchsia"))]
        let listener = netext::TcpListenerStream(listener).map_err(Error::from).map_ok(|conn| {
            TcpStream { stream: netext::TokioAsyncReadExt::into_multithreaded_futures_stream(conn) }
        });

        let connections = listener
            .map_ok(|conn| Pin::new(Box::new(conn)) as Pin<Box<dyn AsyncReadWrite>>)
            .boxed();

        let make_svc = make_service_fn(move |_socket| async move {
            Ok::<_, Infallible>(service_fn(move |_req| async move {
                Ok::<_, Infallible>(Response::new(Body::from("Hello")))
            }))
        });

        let (stop, rx_stop) = futures::channel::oneshot::channel();

        let server = async {
            Server::builder(from_stream(connections))
                .executor(Executor)
                .serve(make_svc)
                .with_graceful_shutdown(
                    rx_stop.map(|res| res.unwrap_or_else(|futures::channel::oneshot::Canceled| ())),
                )
                .unwrap_or_else(|e| panic!("error serving repo over http: {}", e))
                .await;
            Ok(())
        };

        let client = async {
            let output: Vec<u8> = Vec::new();
            let status = fetch_url(format!("http://{addr}").parse::<hyper::Uri>().unwrap(), output)
                .await
                .unwrap();
            match status {
                StatusCode::OK | StatusCode::FOUND => {}
                _ => assert!(false, "Unexpected status code: {}", status),
            }
            stop.send(()).expect("server to still be running");
            Ok(())
        };

        let mut tasks: FuturesUnordered<BoxFuture<'_, Result<(), Error>>> = FuturesUnordered::new();
        tasks.push(Box::pin(server));
        tasks.push(Box::pin(client));
        while let Some(Ok(())) = tasks.next().await {}
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_download_handles_bad_domain() -> Result<()> {
        let output: Vec<u8> = Vec::new();
        let res = fetch_url("https://domain.invalid".parse::<hyper::Uri>()?, output).await;
        assert!(res.is_err());
        Ok(())
    }
}
