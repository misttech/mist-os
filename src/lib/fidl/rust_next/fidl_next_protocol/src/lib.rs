// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Protocol support for FIDL.
//!
//! This crate provides a number of types and traits related to FIDL protocols. These types and
//! traits are all "untyped" - that is, they do not know about the specific types of the FIDL
//! messages being sent and received. They only deal with protocol-layer semantics: requests and
//! responses, clients and servers, and transports.
//!
//! ## Transports
//!
//! This crate uses "transport" to refer to the specific object which moves bytes and handles from a
//! sender to a receiver. For example, this crate may refer to a channel, socket, file, or other
//! byte source/sink as a "transport". This differs from the `@transport(..)` annotation in the FIDL
//! language, which can be added to protocols.
//!
//! FIDL transports implement the [`Transport`] trait. This trait defines several key properties:
//!
//! - The associated [`Sender`](Transport::Sender) and [`Receiver`](Transport::Receiver) types.
//! - The buffer types for sending and receiving data.
//! - The `async` methods for sending and receiving data with those buffers.
//!
//! All types in the protocol layer are generic over the transport, making it easy to add support
//! for new types of transports.
//!
//! By default, both sending and receiving data with a transport are asynchronous operations.
//! However, transports may support synchronous send operations by implementing the
//! [`NonBlockingTransport`] trait. This trait allows users to replace `.await`-ing a send operation
//! with [`.send_immediately()`](NonBlockingTransport::send_immediately), which synchronously
//! completes the send future.
//!
//! This crate provides an implementation of `Transport` for Fuchsia channels.
//!
//! ## Clients and servers
//!
//! [`Client`]s and [`Server`]s are constructed from a transport, and can be `run` with a
//! corresponding [`ClientHandler`] or [`ServerHandler`]. The client or server will then run its
//! event loop to receive data through the transport. Clients use the handler to handle incoming
//! events, and communicate with any [`ClientSender`]s to route two-way method responses to waiting
//! futures. Servers use the handler to handle incoming one-way and two-way method requests, but do
//! not generally communicate with its associated [`ServerSender`]s.
//!
//! [`ClientSender`]s and [`ServerSender`]s generally implement `Clone`, and should be cloned to
//! send from multiple locations.

#![deny(
    future_incompatible,
    missing_docs,
    nonstandard_style,
    unused,
    warnings,
    clippy::all,
    clippy::alloc_instead_of_core,
    clippy::missing_safety_doc,
    clippy::std_instead_of_core,
    // TODO: re-enable this lint after justifying unsafe blocks
    // clippy::undocumented_unsafe_blocks,
    rustdoc::broken_intra_doc_links,
    rustdoc::missing_crate_level_docs
)]
#![forbid(unsafe_op_in_unsafe_fn)]

mod buffer;
mod client;
mod error;
mod flexible;
mod flexible_result;
mod framework_error;
#[cfg(feature = "fuchsia")]
pub mod fuchsia;
mod lockers;
pub mod mpsc;
mod server;
mod service;
#[cfg(test)]
mod testing;
mod transport;
mod wire;

pub use self::buffer::*;
pub use self::client::*;
pub use self::error::*;
pub use self::flexible::*;
pub use self::flexible_result::*;
pub use self::framework_error::*;
pub use self::server::*;
pub use self::service::*;
pub use self::transport::*;
pub use self::wire::*;
