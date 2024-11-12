// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::handle::handle_type;
use crate::responder::Responder;
use crate::{ordinals, Error, Handle};
use fidl_fuchsia_fdomain as proto;
use futures::channel::mpsc::UnboundedReceiver;
use futures::{FutureExt, StreamExt};
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
    /// Read up to `max_bytes` from the socket.
    pub fn read(&self, max_bytes: usize) -> impl Future<Output = Result<Vec<u8>, Error>> {
        let client = self.0.client();
        let handle = self.0.proto();
        let result = client.map(|client| {
            client
                .transaction(
                    ordinals::READ_SOCKET,
                    proto::SocketReadSocketRequest {
                        handle,
                        max_bytes: max_bytes.try_into().unwrap_or(u64::MAX),
                    },
                    Responder::ReadSocket,
                )
                .map(|f| f.map(|x| x.data))
        });
        async move { result?.await }
    }

    /// Write all of the given data to the socket.
    pub fn write_all(&self, bytes: &[u8]) -> impl Future<Output = Result<(), Error>> {
        let data = bytes.to_vec();
        let len = bytes.len();
        let hid = self.0.proto();

        let client = self.0.client();
        let result = client.map(|client| {
            client
                .transaction(
                    ordinals::WRITE_SOCKET,
                    proto::SocketWriteSocketRequest { handle: hid, data },
                    move |x| Responder::WriteSocket(x, hid),
                )
                .map(move |x| x.map(|y| assert!(y.wrote as usize == len)))
        });
        async move { result?.await }
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
        let result = client.map(|client| {
            client.transaction(
                ordinals::SET_SOCKET_DISPOSITION,
                proto::SocketSetSocketDispositionRequest { handle, disposition, disposition_peer },
                Responder::SetSocketDisposition,
            )
        });
        async move { result?.await }
    }

    /// Split this socket into a streaming reader and a writer. This is more
    /// efficient on the read side if you intend to consume all of the data from
    /// the socket. However it will prevent you from transferring the handle in
    /// the future. It also means data will build up in the buffer, so it may
    /// lead to memory issues if you don't intend to use the data from the
    /// socket as fast as it comes.
    pub fn stream(self) -> Result<(SocketReadStream, SocketWriter), Error> {
        let (sender, channel) = futures::channel::mpsc::unbounded();
        self.0.client()?.start_socket_streaming(self.0.proto(), sender)?;

        let a = Arc::new(self);
        let b = Arc::clone(&a);

        Ok((SocketReadStream { socket: a, buf: Vec::new(), channel }, SocketWriter(b)))
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
pub struct SocketReadStream {
    socket: Arc<Socket>,
    buf: Vec<u8>,
    channel: UnboundedReceiver<Result<Vec<u8>, Error>>,
}

impl SocketReadStream {
    /// Read from the socket into the supplied buffer. Returns the number of bytes read.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if self.buf.is_empty() {
            let Some(payload) = self.channel.next().await else {
                return Ok(0);
            };

            self.buf = payload?;
        }

        let size = std::cmp::min(buf.len(), self.buf.len());
        buf[..size].copy_from_slice(&self.buf[..size]);
        self.buf.drain(..size);
        Ok(size)
    }
}

impl Drop for SocketReadStream {
    fn drop(&mut self) {
        if let Ok(client) = self.socket.0.client() {
            client.stop_socket_streaming(self.socket.0.proto());
        }
    }
}
