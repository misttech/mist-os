// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_command_error::Result;
use ffx_target::connection::Connection;
use futures::future::LocalBoxFuture;
use mockall::automock;
use std::net::SocketAddr;
use std::sync::Arc;

/// An object used for connecting to a Fuchsia Device. This represents the entire underlying
/// connection to a fuchsia device. If this object is dropped, then all FIDL protocols will be
/// closed with `PEER_CLOSED` errors as a result.
#[automock]
pub trait DirectConnector {
    // A note on the shape of this trait: the object must _not_ be sized in order for it to be used as
    // a trait object, due to the bounds around object safety in rust. Furthermore, a safe trait object
    // cannot return `impl Trait` of some kind, so using `impl Future` and making this a trait object
    // is not feasible. This is functionally what `async_trait` already does, but as a whole the tools
    // team is moving away from its usage where possible.

    /// Attempts to connect to the Fuchsia device. This function can be run until it succeeds, but
    /// once it succeeds running this function again will takes the current connection, tear it
    /// down, then create a new connection.
    fn connect(&self) -> LocalBoxFuture<'_, Result<()>>;

    /// Attempts to pull any errors off of the connection and wrap the passed error in one larger
    /// error encompassing the entire connection failure. This is usually done after something
    /// else depending on the connection fails (e.g. a failure in the operation of a FIDL
    /// protocol).
    ///
    /// In the event that `Some(_)` the enclosed vector will always be of size 1 or larger.
    fn wrap_connection_errors(&self, e: crate::Error) -> LocalBoxFuture<'_, crate::Error>;

    /// Returns the device address (if there currently is one).
    fn device_address(&self) -> LocalBoxFuture<'_, Option<SocketAddr>>;

    /// Returns the host address of the ssh connection from the device perspective.
    fn host_ssh_address(&self) -> LocalBoxFuture<'_, Option<String>>;

    /// Returns the spec of the target to which we are connecting/connected.
    fn target_spec(&self) -> Option<String>;

    /// Returns the `Connection` (connecting if it is not yet connected)
    fn connection(&self) -> LocalBoxFuture<'_, Result<Arc<async_lock::Mutex<Option<Connection>>>>>;
}
