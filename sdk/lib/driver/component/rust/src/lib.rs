// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around the mechanisms of driver registration for the driver
//! framework for implementing startup and shutdown of the driver in rust.

#![warn(missing_docs, unsafe_op_in_unsafe_fn)]

use core::future::Future;

use zx::Status;

mod context;
mod incoming;
pub mod macros;
mod node;
mod server;

pub use context::*;
pub use incoming::*;
pub use node::*;

/// Entry points into a driver for starting and stopping.
///
/// Driver authors should implement this trait, taking information from the [`DriverContext`]
/// passed to the [`Driver::start`] method to set up, and then tearing down any resources they use
/// in the [`Driver::stop`] method.
pub trait Driver: Sized + Send + Sync + 'static {
    /// The name of the driver as it will appear in logs
    const NAME: &str;

    /// This will be called when the driver is started.
    ///
    /// The given [`DriverContext`] contains information and functionality necessary to get at the
    /// driver's incoming and outgoing namespaces, add child nodes in the driver topology, and
    /// manage dispatchers.
    ///
    /// In order for the driver to be properly considered started, it must return [`Status::OK`]
    /// and bind the client end for the [`DriverStartArgs::node`] given in
    /// [`DriverContext::start_args`].
    fn start(context: DriverContext) -> impl Future<Output = Result<Self, Status>> + Send;

    /// This will be called when the driver has been asked to stop, and should do any
    /// asynchronous cleanup necessary before the driver is fully shut down.
    ///
    /// Note: The driver will not be considered fully stopped until the node client end bound in
    /// [`Driver::start`] has been closed.
    fn stop(&self) -> impl Future<Output = ()> + Send;
}
