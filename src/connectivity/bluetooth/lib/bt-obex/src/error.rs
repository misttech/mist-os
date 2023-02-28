// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

/// Errors that occur during the use of the OBEX library.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Error parsing packet: {:?}", .0)]
    Packet(#[from] PacketError),
}

/// Errors that occur during the encoding & decoding of OBEX packets.
#[derive(Error, Debug)]
pub enum PacketError {
    #[error("Buffer is too small")]
    BufferTooSmall,
    #[error("Invalid data length")]
    DataLength,
    #[error("Invalid data: {}", .0)]
    Data(String),
    #[error("Invalid header identifier: {:?}", .0)]
    Identifier(u8),
    #[error("Invalid header encoding")]
    HeaderEncoding,
    #[error("Field is RFA.")]
    Reserved,
    /// An error from another source
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl PacketError {
    pub fn external(e: impl Into<anyhow::Error>) -> Self {
        Self::Other(e.into())
    }

    pub fn data(e: impl Into<String>) -> Self {
        Self::Data(e.into())
    }
}
