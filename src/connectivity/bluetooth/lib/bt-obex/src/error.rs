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
    #[error("Invalid header identifier: {:?}", .0)]
    Identifier(u8),
    #[error("Field is RFA.")]
    Reserved,
}
