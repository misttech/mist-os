// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL protocols.

mod buffer;
pub mod client;
mod error;
mod lockers;
pub mod mpsc;
pub mod server;
mod transport;
mod wire;

pub use self::buffer::*;
pub use self::client::make_client;
pub use self::error::*;
pub use self::server::make_server;
pub use self::transport::*;
pub use self::wire::*;
