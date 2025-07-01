// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// A FIDL protocol.
///
/// # Safety
///
/// The associated `ClientSender` and `ServerSender` types must be `#[repr(transparent)]` wrappers
/// around `ClientSender<T>` and `ServerSender<T>` respectively.
pub unsafe trait Protocol<T> {
    /// The client sender for the protocol. It must be a `#[repr(transparent)]` wrapper around
    /// `ClientSender<T>`.
    type ClientSender;

    /// The server sender for the protocol. It must be a `#[repr(transparent)]` wrapper around
    /// `ServerSender<T>`.
    type ServerSender;
}

/// A discoverable protocol.
pub trait Discoverable {
    /// The service name to use to connect to this discoverable protocol.
    const PROTOCOL_NAME: &'static str;
}

/// A method of a protocol.
pub trait Method {
    /// The ordinal associated with the method;
    const ORDINAL: u64;

    /// The protocol the method is a member of.
    type Protocol;

    /// The request payload for the method.
    type Request;

    /// The response payload for the method.
    type Response;
}

/// The request or response type of a method which does not have a request or response.
pub enum Never {}
