// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A futures-rs executor design specifically for Fuchsia OS.
//!
//! # Example:
//! A simple, singlethreaded print server:
//!
//! ```no_run
//! #[fuchsia_async::run_singlethreaded]
//! async fn main() {
//!     let op = say_world();
//!
//!     // This println! will happen first
//!     println!("Hello...");
//!
//!     // Calling `.await` on `op` starts executing `say_world`.
//!     op.await;
//! }
//!
//!
//! async fn say_world() {
//!     println!("...world");
//! }
//! ```

#![warn(missing_docs)]
#![deny(clippy::await_holding_lock)]
#![deny(clippy::await_holding_refcell_ref)]

mod runtime;
pub use self::runtime::*;

mod handle;
pub use self::handle::channel::{Channel, RecvMsg};
pub use self::handle::on_signals::OnSignals;
pub use self::handle::socket::Socket;

/// Asynchronous networking abstractions.
pub mod net;

#[cfg(target_os = "fuchsia")]
pub use self::handle::{
    fifo::{Fifo, FifoEntry, FifoReadable, FifoWritable, ReadEntries, ReadOne, WriteEntries},
    rwhandle::{RWHandle, ReadableHandle, ReadableState, WritableHandle, WritableState},
};

/// An emulation library for Zircon handles on non-Fuchsia platforms.
#[cfg(not(target_os = "fuchsia"))]
pub mod emulated_handle {
    pub use super::handle::{
        shut_down_handles, AsHandleRef, Channel, ChannelProxyProtocol, EmulatedHandleRef, Event,
        EventPair, Handle, HandleBased, HandleDisposition, HandleInfo, HandleOp, HandleRef,
        MessageBuf, MessageBufEtc, ObjectType, Peered, Rights, Signals, Socket, SocketOpts,
    };
}

/// A future which can be used by multiple threads at once.
pub mod atomic_future;

// Re-export pin_mut as its used by the async proc macros
pub use pin_utils::pin_mut;

pub use fuchsia_async_macro::{run, run_singlethreaded, run_until_stalled};

/// Testing support for repeated runs
pub mod test_support;
