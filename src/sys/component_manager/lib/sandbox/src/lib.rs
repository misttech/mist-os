// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component sandbox traits and capability types.

mod capability;
mod connector;
mod data;
mod dict;
mod dir_connector;
mod dir_entry;
mod directory;
mod handle;
mod instance_token;
mod receiver;
mod router;
mod unit;

#[cfg(target_os = "fuchsia")]
pub mod fidl;

pub use self::capability::{Capability, CapabilityBound, ConversionError, RemoteError};
pub use self::connector::{Connectable, Connector, Message};
pub use self::data::Data;
pub use self::dict::{Dict, Key as DictKey};
pub use self::dir_connector::{DirConnectable, DirConnector};
pub use self::dir_entry::DirEntry;
pub use self::directory::Directory;
pub use self::handle::Handle;
pub use self::instance_token::{WeakInstanceToken, WeakInstanceTokenAny};
pub use self::receiver::{DirReceiver, Receiver};
pub use self::router::{Request, Routable, Router, RouterResponse};
pub use self::unit::Unit;

#[cfg(target_os = "fuchsia")]
pub use {
    self::fidl::store::serve_capability_store, fidl::router::dict_routers_to_dir_entry,
    fidl::RemotableCapability,
};
