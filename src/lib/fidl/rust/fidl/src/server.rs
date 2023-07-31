// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of a server for a fidl interface.

use {
    crate::{
        encoding::{
            DynamicFlags, EmptyStruct, Encode, Encoder, Flexible, FlexibleType, FrameworkErr,
            TransactionHeader, TransactionMessage, TransactionMessageType, TypeMarker,
        },
        epitaph,
        handle::HandleDisposition,
        AsyncChannel, Error, HandleInfo,
    },
    fuchsia_zircon_status as zx_status,
    futures::task::{AtomicWaker, Context},
    std::sync::atomic::{self, AtomicBool},
};

/// A type used from the innards of server implementations.
#[derive(Debug)]
pub struct ServeInner {
    waker: AtomicWaker,
    shutdown: AtomicBool,
    channel: AsyncChannel,
}

impl ServeInner {
    /// Creates a new set of server innards.
    pub fn new(channel: AsyncChannel) -> Self {
        let waker = AtomicWaker::new();
        let shutdown = AtomicBool::new(false);
        ServeInner { waker, shutdown, channel }
    }

    /// Gets a reference to the inner channel.
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

    /// Sets the server to shutdown the next time the stream is polled.
    ///
    /// TODO(fxbug.dev/74241): This should cause the channel to close on the
    /// next poll, but in fact the channel won't close until the user of the
    /// bindings drops their request stream, responders, and control handles.
    pub fn shutdown(&self) {
        self.shutdown.store(true, atomic::Ordering::Relaxed);
        self.waker.wake();
    }

    /// Sets the server to shutdown with an epitaph the next time the stream is polled.
    ///
    /// TODO(fxbug.dev/74241): This should cause the channel to close on the
    /// next poll, but in fact the channel won't close until the user of the
    /// bindings drops their request stream, responders, and control handles.
    pub fn shutdown_with_epitaph(&self, status: zx_status::Status) {
        let already_shutting_down = self.shutdown.swap(true, atomic::Ordering::Relaxed);
        if !already_shutting_down {
            // Ignore the error, best effort sending an epitaph.
            let _ = epitaph::write_epitaph_impl(&self.channel, status);
            self.waker.wake();
        }
    }

    /// Checks if the server has been set to shutdown.
    pub fn check_shutdown(&self, cx: &Context<'_>) -> bool {
        if self.shutdown.load(atomic::Ordering::Relaxed) {
            return true;
        }
        self.waker.register(cx.waker());
        self.shutdown.load(atomic::Ordering::Relaxed)
    }

    /// Sends an encodable message to the client.
    #[inline]
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

    /// Sends a framework error to the client.
    ///
    /// The caller must be inside a `with_tls_decode_buf` callback, and pass the
    /// buffers used to decode the request as the `tls_decode_buf` parameter.
    #[inline]
    pub fn send_framework_err(
        &self,
        framework_err: FrameworkErr,
        tx_id: u32,
        ordinal: u64,
        dynamic_flags: DynamicFlags,
        tls_decode_buf: (&mut Vec<u8>, &mut Vec<HandleInfo>),
    ) -> Result<(), Error> {
        type Msg = TransactionMessageType<FlexibleType<EmptyStruct>>;
        let msg = TransactionMessage {
            header: TransactionHeader::new(tx_id, ordinal, dynamic_flags),
            body: Flexible::<()>::FrameworkErr(framework_err),
        };

        // RFC-0138 requires us to close handles in the incoming message before replying.
        let (bytes, handle_infos) = tls_decode_buf;
        handle_infos.clear();
        // Reuse the request decoding byte buffer for encoding (we can't call
        // `with_tls_encoded` as we're already inside `with_tls_decode_buf`).
        let mut handle_dispositions = Vec::new();
        Encoder::encode::<Msg>(bytes, &mut handle_dispositions, msg)?;
        debug_assert!(handle_dispositions.is_empty());
        self.send_raw_msg(&*bytes, &mut [])
    }

    /// Sends a raw message to the client.
    fn send_raw_msg(
        &self,
        bytes: &[u8],
        handles: &mut [HandleDisposition<'_>],
    ) -> Result<(), Error> {
        match self.channel.write_etc(bytes, handles) {
            Ok(()) | Err(zx_status::Status::PEER_CLOSED) => Ok(()),
            Err(e) => Err(Error::ServerResponseWrite(e)),
        }
    }
}
