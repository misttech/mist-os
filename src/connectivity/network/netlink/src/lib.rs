// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of Linux's Netlink API for Fuchsia.
//!
//! Netlink is a socket-based API provided by Linux that user space applications
//! can use to interact with the kernel. The API is split up into several
//! protocol families each offering different functionality. This crate targets
//! the implementation of families related to networking.

use futures::future::Future;

/// The implementation of the Netlink protocol suite.
pub struct Netlink {
    /// A temporary private field to ensure that `Netlink` cannot be member
    /// initialized by external users. This can be removed once the struct has
    /// actual private fields.
    _private: (),
}

impl Netlink {
    /// Returns a newly instantiated [`Netlink`] and it's associated event loop.
    ///
    /// Callers are responsible for polling the event loop, which drives
    /// the Netlink implementation's asynchronous work. The event loop will
    /// never complete.
    pub fn new() -> (Self, impl Future<Output = ()>) {
        (Netlink { _private: () }, run_event_loop())
    }
}

/// The event loop encompassing all asynchronous Netlink work.
///
/// The event loop is never expected to complete.
async fn run_event_loop() {
    // Temporary to prevent the event loop from terminating.
    futures::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::FutureExt as _;

    // Placeholder test to ensure the build targets are setup properly.
    #[test]
    fn test_event_loop() {
        assert_eq!(run_event_loop().now_or_never(), None);
    }
}
