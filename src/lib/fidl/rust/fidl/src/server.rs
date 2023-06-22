// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of a server for a fidl interface.

use {
    crate::{
        encoding::{
            DynamicFlags, Encode, TransactionHeader, TransactionMessage, TransactionMessageType,
            TypeMarker,
        },
        epitaph,
        handle::HandleDisposition,
        AsyncChannel, Error,
    },
    fuchsia_zircon_status as zx_status,
    futures::task::{AtomicWaker, Context},
    std::sync::atomic::{self, AtomicBool},
};

/// A type used from the innards of server implementations
#[derive(Debug)]
pub struct ServeInner {
    waker: AtomicWaker,
    shutdown: AtomicBool,
    channel: AsyncChannel,
}

impl ServeInner {
    /// Create a new set of server innards.
    pub fn new(channel: AsyncChannel) -> Self {
        let waker = AtomicWaker::new();
        let shutdown = AtomicBool::new(false);
        ServeInner { waker, shutdown, channel }
    }

    /// Get a reference to the inner channel.
    pub fn channel(&self) -> &AsyncChannel {
        &self.channel
    }

    /// Converts the [`ServerInner`] back into a channel.
    ///
    /// **Warning**: This operation is dangerous, since the returned channel
    /// could have unread messages intended for this server. Use it carefully.
    pub fn into_channel(self) -> AsyncChannel {
        self.channel
    }

    /// Set the server to shutdown the next time the stream is polled.
    pub fn shutdown(&self) {
        self.shutdown.store(true, atomic::Ordering::Relaxed);
        self.waker.wake();
    }

    /// Set the server to shutdown with an epitaph.
    pub fn shutdown_with_epitaph(&self, status: zx_status::Status) {
        let already_shutting_down = self.shutdown.swap(true, atomic::Ordering::Relaxed);
        if !already_shutting_down {
            // Ignore the error, best effort sending an epitaph.
            let _ = epitaph::write_epitaph_impl(&self.channel, status);
            self.waker.wake();
        }
    }

    /// Check if the server has been set to shutdown.
    pub fn poll_shutdown(&self, cx: &mut Context<'_>) -> bool {
        if self.shutdown.load(atomic::Ordering::Relaxed) {
            return true;
        }
        self.waker.register(cx.waker());
        self.shutdown.load(atomic::Ordering::Relaxed)
    }

    /// Send an encodable message to the client.
    pub fn send<T: TypeMarker>(
        &self,
        body: impl Encode<T>,
        tx_id: u32,
        ordinal: u64,
        dynamic_flags: DynamicFlags,
    ) -> Result<(), Error> {
        let msg = TransactionMessage {
            header: TransactionHeader::new(tx_id, ordinal, dynamic_flags),
            body,
        };
        crate::encoding::with_tls_encoded::<TransactionMessageType<T>, ()>(msg, |bytes, handles| {
            self.send_raw_msg(bytes, handles)
        })
    }

    /// Send a raw message to the client.
    pub fn send_raw_msg(
        &self,
        bytes: &[u8],
        handles: &mut Vec<HandleDisposition<'_>>,
    ) -> Result<(), Error> {
        match self.channel.write_etc(bytes, handles) {
            Ok(()) | Err(zx_status::Status::PEER_CLOSED) => Ok(()),
            Err(e) => Err(Error::ServerResponseWrite(e.into())),
        }
    }
}
