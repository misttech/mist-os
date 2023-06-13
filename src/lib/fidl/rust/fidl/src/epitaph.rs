// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Epitaph support for Channel and AsyncChannel.

use {
    crate::{
        encoding::{
            self, DynamicFlags, EpitaphBody, TransactionHeader, TransactionMessage,
            TransactionMessageType,
        },
        error::Error,
        AsyncChannel, Channel, HandleDisposition,
    },
    fuchsia_zircon_status as zx_status,
};

/// Extension trait that provides Channel-like objects with the ability to send a FIDL epitaph.
pub trait ChannelEpitaphExt {
    /// Consumes the channel and writes an epitaph.
    fn close_with_epitaph(self, status: zx_status::Status) -> Result<(), Error>;
}

impl ChannelEpitaphExt for Channel {
    fn close_with_epitaph(self, status: zx_status::Status) -> Result<(), Error> {
        write_epitaph_impl(&self, status)
    }
}

impl ChannelEpitaphExt for AsyncChannel {
    fn close_with_epitaph(self, status: zx_status::Status) -> Result<(), Error> {
        write_epitaph_impl(&self, status)
    }
}

pub(crate) trait ChannelLike {
    fn write_etc<'a>(
        &self,
        bytes: &[u8],
        handles: &mut Vec<HandleDisposition<'a>>,
    ) -> Result<(), zx_status::Status>;
}

impl ChannelLike for Channel {
    fn write_etc<'a>(
        &self,
        bytes: &[u8],
        handles: &mut Vec<HandleDisposition<'a>>,
    ) -> Result<(), zx_status::Status> {
        self.write_etc(bytes, handles)
    }
}

impl ChannelLike for AsyncChannel {
    fn write_etc<'a>(
        &self,
        bytes: &[u8],
        handles: &mut Vec<HandleDisposition<'a>>,
    ) -> Result<(), zx_status::Status> {
        self.write_etc(bytes, handles)
    }
}

pub(crate) fn write_epitaph_impl<T: ChannelLike>(
    channel: &T,
    status: zx_status::Status,
) -> Result<(), Error> {
    let msg = TransactionMessage {
        header: TransactionHeader::new(0, encoding::EPITAPH_ORDINAL, DynamicFlags::empty()),
        body: &EpitaphBody { error: status },
    };
    encoding::with_tls_encoded::<TransactionMessageType<EpitaphBody>, (), false>(
        msg,
        |bytes, handles| match channel.write_etc(bytes, handles) {
            Ok(()) | Err(zx_status::Status::PEER_CLOSED) => Ok(()),
            Err(e) => Err(Error::ServerEpitaphWrite(e)),
        },
    )
}
