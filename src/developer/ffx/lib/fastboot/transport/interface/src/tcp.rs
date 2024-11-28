// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context as _, Result};
use futures::prelude::*;
use futures::task::{Context, Poll};
use netext::{TokioAsyncReadExt, TokioAsyncWrapper};
use std::fmt;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use timeout::timeout;
use tokio::net::TcpStream;

const FB_HANDSHAKE: [u8; 4] = *b"FB01";

///////////////////////////////////////////////////////////////////////////////
// TcpNetworkInterface
//

pub struct TcpNetworkInterface<T> {
    stream: T,
    read_avail_bytes: Option<u64>,
    /// Returns a tuple of (avail_bytes, bytes_read, bytes)
    read_task: Option<Pin<Box<dyn Future<Output = std::io::Result<(u64, usize, Vec<u8>)>>>>>,
    write_task: Option<Pin<Box<dyn Future<Output = std::io::Result<usize>>>>>,
    /// Flag to indicate if the header for a given Write operation was completed
    wrote_header: bool,
    /// Task to write the "header" for the Fastboot over TCP message
    /// In TCP mode the Fastboot Protocol expects the first 8 bytes (sizeof u64)
    /// to be prepended to any write operation. These bytes indicate the total
    /// size in bytes of the message that is about to be sent.
    ///
    /// These bytes must be sent in their entirety before execution of `write_task`
    /// in order to follow the `AsyncWrite` contract correctly. For example, if we
    /// are writing a `16` byte message, in reality the entirety of the message being
    /// sent is `16` bytes AND the header, but the total written bytes returned to
    /// `AsyncWrite` will still only be `16`. If we keep all this in a single task,
    /// then the total bytes will be larger than intended, causing `AsyncWrite`
    /// operations like `write_all` to fail in confusing ways.
    write_header_task: Option<Pin<Box<dyn Future<Output = std::io::Result<()>>>>>,
}

impl<T> fmt::Debug for TcpNetworkInterface<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpNetworkInterface")
            .field("read_avail_bytes", &self.read_avail_bytes)
            .finish()
    }
}

impl<T> AsyncRead for TcpNetworkInterface<T>
where
    T: AsyncRead + AsyncWrite + std::marker::Unpin + Clone + 'static,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.read_task.is_none() {
            let mut stream = self.stream.clone();
            let avail_bytes = self.read_avail_bytes;
            let _length = buf.len();
            self.read_task.replace(Box::pin(async move {
                let mut avail_bytes = match avail_bytes {
                    Some(value) => value,
                    None => {
                        let mut pkt_len = [0; 8];
                        let bytes_read = stream.read(&mut pkt_len).await?;
                        if bytes_read != pkt_len.len() {
                            return Err(std::io::Error::new(
                                ErrorKind::Other,
                                format!("Could not read packet header"),
                            ));
                        }
                        u64::from_be_bytes(pkt_len)
                    }
                };

                let mut data_buf = vec![0; avail_bytes.try_into().unwrap()];
                let bytes_read: u64 =
                    stream.read(data_buf.as_mut_slice()).await?.try_into().unwrap();
                avail_bytes -= bytes_read;

                Ok((avail_bytes, bytes_read.try_into().unwrap(), data_buf))
            }));
        }

        let task = self.read_task.as_mut().unwrap();
        match task.as_mut().poll(cx) {
            Poll::Ready(Ok((avail_bytes, bytes_read, data))) => {
                self.read_task = None;
                self.read_avail_bytes = if avail_bytes == 0 { None } else { Some(avail_bytes) };
                buf[0..bytes_read].copy_from_slice(&data[0..bytes_read]);
                Poll::Ready(Ok(bytes_read))
            }
            Poll::Ready(Err(e)) => {
                self.read_task = None;
                Poll::Ready(Err(e))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> AsyncWrite for TcpNetworkInterface<T>
where
    T: AsyncWrite + std::marker::Unpin + Clone + 'static,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.write_header_task.is_none() && !self.wrote_header {
            tracing::debug!("About to start header task");
            let mut stream = self.stream.clone();
            let mut data = vec![];
            data.extend(TryInto::<u64>::try_into(buf.len()).unwrap().to_be_bytes());
            self.write_header_task.replace(Box::pin(async move { stream.write_all(&data).await }));
        }

        if self.write_header_task.is_some() {
            tracing::debug!("Checking header task status");
            let task = self.write_header_task.as_mut().unwrap();
            let res = match task.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => {
                    self.write_header_task = None;
                    self.wrote_header = true;
                    cx.waker().clone().wake();
                    Poll::Pending
                }
                Poll::Ready(Err(e)) => {
                    self.write_header_task = None;
                    self.wrote_header = true;
                    Poll::Ready(Err(e))
                }
                Poll::Pending => Poll::Pending,
            };
            tracing::debug!("Header Check: returning: {:#?}", res);
            return res;
        }

        if self.write_task.is_none() {
            let mut stream = self.stream.clone();
            let mut data = vec![];
            data.extend(buf);
            self.write_task.replace(Box::pin(async move {
                let mut start = 0;
                while start < data.len() {
                    // We won't always succeed in writing the entire buffer at once, so
                    // we try repeatedly until everything is written.
                    let written = stream.write(&data[start..]).await?;
                    if written == 0 {
                        return Err(std::io::Error::new(
                            ErrorKind::Other,
                            format!("Write made no progress"),
                        ));
                    }

                    start += written;
                }
                Ok(data.len())
            }));
        }

        let task = self.write_task.as_mut().unwrap();
        match task.as_mut().poll(cx) {
            Poll::Ready(Ok(s)) => {
                self.write_task = None;
                self.wrote_header = false;
                self.write_header_task = None;
                Poll::Ready(Ok(s))
            }
            Poll::Ready(Err(e)) => {
                self.write_task = None;
                self.wrote_header = false;
                self.write_header_task = None;
                Poll::Ready(Err(e))
            }
            Poll::Pending => {
                cx.waker().clone().wake();
                Poll::Pending
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unimplemented!();
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unimplemented!();
    }
}

async fn handshake<T: AsyncWrite + AsyncRead + std::marker::Unpin>(stream: &mut T) -> Result<()> {
    stream.write(&FB_HANDSHAKE).await.context("Sending handshake")?;
    let mut response = [0; 4];
    stream.read_exact(&mut response).await.context("Receiving handshake response")?;
    if response != FB_HANDSHAKE {
        bail!("Invalid response to handshake");
    }
    Ok(())
}

/// Timeout in seconds waiting for a valid fastboot TCP handshake.
pub const HANDSHAKE_TIMEOUT: &str = "fastboot.tcp.handshake.timeout";

pub const RETRY_WAIT_SECONDS: u64 = 5;
const FASTBOOT_PORT: u16 = 5554;
pub const HANDSHAKE_TIMEOUT_MILLIS: u64 = 1000;

pub async fn open_once(
    target: &SocketAddr,
    handshake_timeout: Duration,
) -> Result<TcpNetworkInterface<TokioAsyncWrapper<TcpStream>>> {
    let mut addr: SocketAddr = target.clone();
    if addr.port() == 0 {
        tracing::debug!(
            "Address does not have port set ({addr:?}. Using default:  {FASTBOOT_PORT}"
        );
        addr.set_port(FASTBOOT_PORT);
    }

    tracing::debug!("Trying to establish TCP Connection to address: {addr:?}");
    timeout(handshake_timeout, async {
        let mut stream = TcpStream::connect(addr)
            .await
            .context("Establishing TCP connection")?
            .into_futures_stream();
        handshake(&mut stream).await?;
        Ok(TcpNetworkInterface {
            stream,
            read_avail_bytes: None,
            read_task: None,
            write_task: None,
            write_header_task: None,
            wrote_header: false,
        })
    })
    .await?
}

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct TestInnerWriter {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl AsyncWrite for TestInnerWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            futures::executor::block_on(async {
                self.inner.lock().unwrap().write_all(buf).await.expect("Writing should work")
            });
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            unimplemented!();
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            unimplemented!();
        }
    }

    impl AsyncRead for TestInnerWriter {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            unimplemented!();
        }
    }

    #[fuchsia::test]
    async fn test_async_write_includes_header() -> Result<()> {
        let inner = Arc::new(Mutex::new(vec![]));
        let stream = TestInnerWriter { inner: inner.clone() };
        let mut interface = TcpNetworkInterface {
            stream,
            read_avail_bytes: None,
            read_task: None,
            write_task: None,
            write_header_task: None,
            wrote_header: false,
        };

        let bytes = vec![0, 1, 2];
        interface.write_all(&bytes).await?;
        assert_eq!(
            *inner.lock().unwrap(),
            vec![0, 0, 0, 0, 0, 0, 0, 3, 0, 1, 2],
            "stream contents"
        );
        Ok(())
    }
}
