// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Target side networking utilities for host tools.

mod error;
mod port_forwarder;
mod socket_provider;

pub use error::Error;
pub use port_forwarder::PortForwarder;
pub use socket_provider::{SocketProvider, TargetTcpListener, TargetTcpStream};

pub(crate) type Result<T> = std::result::Result<T, Error>;
