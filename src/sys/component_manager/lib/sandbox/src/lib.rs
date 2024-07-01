// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component sandbox traits and capability types.

mod capability;
mod connector;
mod data;
mod dict;
mod directory;
mod handle;
mod instance_token;
mod receiver;
mod router;
mod unit;

// TODO(340891837): dir_entry only builds on target due to its reliance on the vfs library. There's
// no point investing time into reducing that reliance, as dir_entry is going to be deleted.
#[cfg(target_os = "fuchsia")]
mod dir_entry;

#[cfg(target_os = "fuchsia")]
mod fidl;

pub use self::capability::{Capability, ConversionError, RemoteError};
pub use self::connector::{Connectable, Connector, Message};
pub use self::data::Data;
pub use self::dict::{Dict, Key as DictKey};
pub use self::directory::Directory;
pub use self::handle::Handle;
pub use self::instance_token::{WeakInstanceToken, WeakInstanceTokenAny};
pub use self::receiver::Receiver;
pub use self::router::{Request, Routable, Router};
pub use self::unit::Unit;

#[cfg(target_os = "fuchsia")]
pub use {self::dir_entry::DirEntry, fidl::RemotableCapability};
