// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    on_signals::OnSignals,
    rwhandle::{RWHandle, ReadableHandle, ReadableState, WritableHandle, WritableState},
};
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::io::{self, AsyncRead, AsyncWrite};
use futures::{future::poll_fn, ready, stream::Stream, task::Context};
use std::fmt;
use std::pin::Pin;
use std::task::Poll;

/// An I/O object representing a `Socket`.
pub struct Socket(RWHandle<zx::Socket>);

impl AsRef<zx::Socket> for Socket {
    fn as_ref(&self) -> &zx::Socket {
        &self.0.get_ref()
    }
}

impl AsHandleRef for Socket {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.0.get_ref().as_handle_ref()
    }
}

impl Socket {
    /// Create a new `Socket` from a previously-created `zx::Socket`.
    pub fn from_socket(socket: zx::Socket) -> Result<Self, zx::Status> {
        Ok(Socket(RWHandle::new(socket)?))
    }

    /// Consumes `self` and returns the underlying `zx::Socket`.
    pub fn into_zx_socket(self) -> zx::Socket {
        self.0.into_inner()
    }

    /// Returns true if the socket received the `OBJECT_PEER_CLOSED` signal.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// Returns a future that completes when the socket received the `OBJECT_PEER_CLOSED` signal.
    pub fn on_closed(&self) -> OnSignals<'_> {
        self.0.on_closed()
    }

    /// Attempt to read from the socket, registering for wakeup if the socket doesn't have any
    /// contents available. Used internally in the `AsyncRead` implementation, exposed for users
    /// who know the concrete type they're using and don't want to pin the socket.
    pub fn poll_read_ref(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, zx::Status>> {
        ready!(self.poll_readable(cx))?;
        let res = self.0.get_ref().read(buf);
        if res == Err(zx::Status::SHOULD_WAIT) {
            self.need_readable(cx)?;
            return Poll::Pending;
        }
        if res == Err(zx::Status::PEER_CLOSED) {
            return Poll::Ready(Ok(0));
        }
        Poll::Ready(res)
    }

    /// Attempt to write into the socket, registering for wakeup if the socket is not ready. Used
    /// internally in the `AsyncWrite` implementation, exposed for users who know the concrete type
    /// they're using and don't want to pin the socket.
    pub fn poll_write_ref(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, zx::Status>> {
        ready!(self.poll_writable(cx))?;
        let res = self.0.get_ref().write(buf);
        if res == Err(zx::Status::SHOULD_WAIT) {
            self.need_writable(cx)?;
            Poll::Pending
        } else {
            Poll::Ready(res)
        }
    }

    /// Polls for the next data on the socket, appending it to the end of |out| if it has arrived.
    /// Not very useful for a non-datagram socket as it will return all available data
    /// on the socket.
    pub fn poll_datagram(
        &self,
        cx: &mut Context<'_>,
        out: &mut Vec<u8>,
    ) -> Poll<Result<usize, zx::Status>> {
        ready!(self.poll_readable(cx))?;
        let avail = self.0.get_ref().outstanding_read_bytes()?;
        let len = out.len();
        out.resize(len + avail, 0);
        let (_, mut tail) = out.split_at_mut(len);
        match self.0.get_ref().read(&mut tail) {
            Err(zx::Status::SHOULD_WAIT) => {
                self.need_readable(cx)?;
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
            Ok(bytes) => {
                if bytes == avail {
                    Poll::Ready(Ok(bytes))
                } else {
                    Poll::Ready(Err(zx::Status::BAD_STATE))
                }
            }
        }
    }

    /// Reads the next datagram that becomes available onto the end of |out|.  Note: Using this
    /// multiple times concurrently is an error and the first one will never complete.
    pub async fn read_datagram<'a>(&'a self, out: &'a mut Vec<u8>) -> Result<usize, zx::Status> {
        poll_fn(move |cx| self.poll_datagram(cx, out)).await
    }

    /// Use this socket as a stream of `Result<Vec<u8>, zx::Status>` datagrams.
    ///
    /// Note: multiple concurrent streams from the same socket are not supported.
    pub fn as_datagram_stream<'a>(&'a self) -> DatagramStream<&'a Self> {
        DatagramStream(self)
    }

    /// Convert this socket into a stream of `Result<Vec<u8>, zx::Status>` datagrams.
    pub fn into_datagram_stream(self) -> DatagramStream<Self> {
        DatagramStream(self)
    }
}

impl ReadableHandle for Socket {
    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<Result<ReadableState, zx::Status>> {
        self.0.poll_readable(cx)
    }

    fn need_readable(&self, cx: &mut Context<'_>) -> Result<(), zx::Status> {
        self.0.need_readable(cx)
    }
}

impl WritableHandle for Socket {
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<Result<WritableState, zx::Status>> {
        self.0.poll_writable(cx)
    }

    fn need_writable(&self, cx: &mut Context<'_>) -> Result<(), zx::Status> {
        self.0.need_writable(cx)
    }
}

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.get_ref().fmt(f)
    }
}

impl AsyncRead for Socket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_read_ref(cx, buf).map_err(Into::into)
    }
}

impl AsyncWrite for Socket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_ref(cx, buf).map_err(Into::into)
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl<'a> AsyncRead for &'a Socket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_read_ref(cx, buf).map_err(Into::into)
    }
}

impl<'a> AsyncWrite for &'a Socket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_ref(cx, buf).map_err(Into::into)
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// A datagram stream from a `Socket`.
#[derive(Debug)]
pub struct DatagramStream<S>(pub S);

fn poll_datagram_as_stream(
    socket: &Socket,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<Vec<u8>, zx::Status>>> {
    let mut res = Vec::<u8>::new();
    Poll::Ready(match ready!(socket.poll_datagram(cx, &mut res)) {
        Ok(_size) => Some(Ok(res)),
        Err(zx::Status::PEER_CLOSED) => None,
        Err(e) => Some(Err(e)),
    })
}

impl Stream for DatagramStream<Socket> {
    type Item = Result<Vec<u8>, zx::Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_datagram_as_stream(&self.0, cx)
    }
}

impl Stream for DatagramStream<&Socket> {
    type Item = Result<Vec<u8>, zx::Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_datagram_as_stream(self.0, cx)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{TestExecutor, Time, TimeoutExt, Timer},
        fuchsia_zircon::prelude::*,
        futures::{
            future::{self, join},
            io::{AsyncReadExt as _, AsyncWriteExt as _},
            stream::TryStreamExt,
            task::noop_waker_ref,
            FutureExt,
        },
        std::pin::pin,
    };

    #[test]
    fn can_read_write() {
        let mut exec = TestExecutor::new();
        let bytes = &[0, 1, 2, 3];

        let (tx, rx) = zx::Socket::create_stream();
        let (mut tx, mut rx) = (Socket::from_socket(tx).unwrap(), Socket::from_socket(rx).unwrap());

        let receive_future = async {
            let mut buf = vec![];
            rx.read_to_end(&mut buf).await.expect("reading socket");
            assert_eq!(&*buf, bytes);
        };

        // add a timeout to receiver so if test is broken it doesn't take forever
        // Note: if debugging a hang, you may want to lower the timeout to `300.millis()` to get
        // faster feedback. This is set to 10s rather than something shorter to avoid triggering
        // flakes if things happen to be slow.
        let receiver = receive_future.on_timeout(Time::after(10.seconds()), || panic!("timeout"));

        // Sends a message after the timeout has passed
        let sender = async move {
            Timer::new(Time::after(100.millis())).await;
            tx.write_all(bytes).await.expect("writing into socket");
            // close socket to signal no more bytes will be written
            drop(tx);
        };

        let done = join(receiver, sender);
        exec.run_singlethreaded(done);
    }

    #[test]
    fn can_read_datagram() {
        let mut exec = TestExecutor::new();

        let (one, two) = (&[0, 1], &[2, 3, 4, 5]);

        let (tx, rx) = zx::Socket::create_datagram();
        let rx = Socket::from_socket(rx).unwrap();

        let mut out = vec![50];

        assert!(tx.write(one).is_ok());
        assert!(tx.write(two).is_ok());

        let size = exec.run_singlethreaded(rx.read_datagram(&mut out));

        assert!(size.is_ok());
        assert_eq!(one.len(), size.unwrap());

        assert_eq!([50, 0, 1], out.as_slice());

        let size = exec.run_singlethreaded(rx.read_datagram(&mut out));

        assert!(size.is_ok());
        assert_eq!(two.len(), size.unwrap());

        assert_eq!([50, 0, 1, 2, 3, 4, 5], out.as_slice());
    }

    #[test]
    fn stream_datagram() {
        let mut exec = TestExecutor::new();

        let (tx, rx) = zx::Socket::create_datagram();
        let mut rx = Socket::from_socket(rx).unwrap().into_datagram_stream();

        let packets = 20;

        for size in 1..packets + 1 {
            let mut vec = Vec::<u8>::new();
            vec.resize(size, size as u8);
            assert!(tx.write(&vec).is_ok());
        }

        // Close the socket.
        drop(tx);

        let stream_read_fut = async move {
            let mut count = 0;
            while let Some(packet) = rx.try_next().await.expect("received error from stream") {
                count = count + 1;
                assert_eq!(packet.len(), count);
                assert!(packet.iter().all(|&x| x == count as u8));
            }
            assert_eq!(packets, count);
        };

        exec.run_singlethreaded(stream_read_fut);
    }

    #[test]
    fn peer_closed_signal_raised() {
        let mut executor = TestExecutor::new();

        let (s1, s2) = zx::Socket::create_stream();
        let mut async_s2 = Socket::from_socket(s2).expect("failed to create async socket");

        // The socket won't start watching for peer-closed until we actually try reading from it.
        let _ = executor.run_until_stalled(&mut pin!(async {
            let mut buf = [0; 16];
            let _ = async_s2.read(&mut buf).await;
        }));

        let on_closed_fut = async_s2.on_closed();

        drop(s1);

        // Now make sure all packets get processed before we poll the socket.
        let _ = executor.run_until_stalled(&mut future::pending::<()>());

        // Dropping s1 raises a closed signal on s2 when the executor next polls the signal port.
        let mut rx_fut = poll_fn(|cx| async_s2.poll_readable(cx));

        if let Poll::Ready(Ok(state)) = executor.run_until_stalled(&mut rx_fut) {
            assert_eq!(state, ReadableState::Closed);
        } else {
            panic!("Expected future to be ready and Ok");
        }
        assert!(async_s2.is_closed());
        assert_eq!(on_closed_fut.now_or_never(), Some(Ok(zx::Signals::CHANNEL_PEER_CLOSED)));
    }

    #[test]
    fn need_read_ensures_freshness() {
        let mut executor = TestExecutor::new();

        let (s1, s2) = zx::Socket::create_stream();
        let async_s2 = Socket::from_socket(s2).expect("failed to create async socket");

        // The read signal is optimistically set on socket creation, so even though there is
        // nothing to read, poll_readable returns Ready.
        let mut rx_fut = poll_fn(|cx| async_s2.poll_readable(cx));
        assert!(executor.run_until_stalled(&mut rx_fut).is_ready());

        // Call need_readable to reacquire the read signal. The socket now knows
        // that the signal is not actually set, so returns Pending.
        async_s2
            .need_readable(&mut Context::from_waker(noop_waker_ref()))
            .expect("failed need read");
        let mut rx_fut = poll_fn(|cx| async_s2.poll_readable(cx));
        assert!(executor.run_until_stalled(&mut rx_fut).is_pending());

        assert_eq!(s1.write(b"hello!").expect("failed to write 6 bytes"), 6);

        // After writing to s1, its peer now has an actual read signal and is Ready.
        assert!(executor.run_until_stalled(&mut rx_fut).is_ready());
    }

    #[test]
    fn need_write_ensures_freshness() {
        let mut executor = TestExecutor::new();

        let (s1, s2) = zx::Socket::create_stream();

        // Completely fill the transmit buffer. This socket is no longer writable.
        let socket_info = s2.info().expect("failed to get socket info");
        let bytes = vec![0u8; socket_info.tx_buf_max];
        assert_eq!(socket_info.tx_buf_max, s2.write(&bytes).expect("failed to write to socket"));

        let async_s2 = Socket::from_socket(s2).expect("failed to create async rx socket");

        // The write signal is optimistically set on socket creation, so even though it's not
        // possible to write, poll_writable returns Ready.
        let mut tx_fut = poll_fn(|cx| async_s2.poll_writable(cx));
        assert!(executor.run_until_stalled(&mut tx_fut).is_ready());

        // Call need_writable to reacquire the write signal. The socket now
        // knows that the signal is not actually set, so returns Pending.
        async_s2
            .need_writable(&mut Context::from_waker(noop_waker_ref()))
            .expect("failed need write");
        let mut tx_fut = poll_fn(|cx| async_s2.poll_writable(cx));
        assert!(executor.run_until_stalled(&mut tx_fut).is_pending());

        let mut buffer = [0u8; 5];
        assert_eq!(s1.read(&mut buffer).expect("failed to read 5 bytes"), 5);

        // After reading from s1, its peer is now able to write and should have a write signal.
        assert!(executor.run_until_stalled(&mut tx_fut).is_ready());
    }
}
