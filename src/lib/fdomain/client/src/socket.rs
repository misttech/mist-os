// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::handle::handle_type;
use crate::responder::Responder;
use crate::{ordinals, Error, Handle};
use fidl_fuchsia_fdomain as proto;
use futures::FutureExt;
use std::future::Future;
use std::sync::Arc;

/// A socket in a remote FDomain.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Socket(pub(crate) Handle);

handle_type!(Socket SOCKET peered);

/// Disposition of a socket.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SocketDisposition {
    WriteEnabled,
    WriteDisabled,
}

impl SocketDisposition {
    /// Convert to a proto::SocketDisposition
    fn proto(self) -> proto::SocketDisposition {
        match self {
            SocketDisposition::WriteEnabled => proto::SocketDisposition::WriteEnabled,
            SocketDisposition::WriteDisabled => proto::SocketDisposition::WriteDisabled,
        }
    }
}

impl Socket {
    /// Read up to the given buffer's length from the socket.
    pub fn read<'a>(&self, buf: &'a mut [u8]) -> impl Future<Output = Result<usize, Error>> + 'a {
        let client = self.0.client();
        let handle = self.0.proto();

        futures::future::poll_fn(move |ctx| client.poll_socket(handle, ctx, buf))
    }

    /// Write all of the given data to the socket.
    pub fn write_all(&self, bytes: &[u8]) -> impl Future<Output = Result<(), Error>> {
        let data = bytes.to_vec();
        let len = bytes.len();
        let hid = self.0.proto();

        let client = self.0.client();
        client
            .transaction(
                ordinals::WRITE_SOCKET,
                proto::SocketWriteSocketRequest { handle: hid, data },
                move |x| Responder::WriteSocket(x),
            )
            .map(move |x| x.map(|y| assert!(y.wrote as usize == len)))
    }

    /// Set the disposition of this socket and/or its peer.
    pub fn set_socket_disposition(
        &self,
        disposition: Option<SocketDisposition>,
        disposition_peer: Option<SocketDisposition>,
    ) -> impl Future<Output = Result<(), Error>> {
        let disposition =
            disposition.map(SocketDisposition::proto).unwrap_or(proto::SocketDisposition::NoChange);
        let disposition_peer = disposition_peer
            .map(SocketDisposition::proto)
            .unwrap_or(proto::SocketDisposition::NoChange);
        let client = self.0.client();
        let handle = self.0.proto();
        client.transaction(
            ordinals::SET_SOCKET_DISPOSITION,
            proto::SocketSetSocketDispositionRequest { handle, disposition, disposition_peer },
            Responder::SetSocketDisposition,
        )
    }

    /// Split this socket into a streaming reader and a writer. This is more
    /// efficient on the read side if you intend to consume all of the data from
    /// the socket. However it will prevent you from transferring the handle in
    /// the future. It also means data will build up in the buffer, so it may
    /// lead to memory issues if you don't intend to use the data from the
    /// socket as fast as it comes.
    pub fn stream(self) -> Result<(SocketReadStream, SocketWriter), Error> {
        self.0.client().start_socket_streaming(self.0.proto())?;

        let a = Arc::new(self);
        let b = Arc::clone(&a);

        Ok((SocketReadStream(a), SocketWriter(b)))
    }
}

/// A write-only handle to a socket.
pub struct SocketWriter(Arc<Socket>);

impl SocketWriter {
    /// Write all of the given data to the socket.
    pub fn write_all(&self, bytes: &[u8]) -> impl Future<Output = Result<(), Error>> {
        self.0.write_all(bytes)
    }
}

/// A stream of data issuing from a socket.
pub struct SocketReadStream(Arc<Socket>);

impl SocketReadStream {
    /// Read from the socket into the supplied buffer. Returns the number of bytes read.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.0.read(buf).await
    }
}

impl Drop for SocketReadStream {
    fn drop(&mut self) {
        if let Some(client) = self.0 .0.client.upgrade() {
            client.stop_socket_streaming(self.0 .0.proto());
        }
    }
}

impl futures::AsyncRead for Socket {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let client = self.0.client();
        client.poll_socket(self.0.proto(), cx, buf).map_err(std::io::Error::other)
    }
}

impl futures::AsyncWrite for Socket {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let _ = self.write_all(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.0 = Handle::invalid();
        std::task::Poll::Ready(Ok(()))
    }
}
