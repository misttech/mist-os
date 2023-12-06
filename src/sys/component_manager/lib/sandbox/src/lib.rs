// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component sandbox traits and capability types.

extern crate self as sandbox;

#[doc(hidden)]
pub use sandbox_macro::Capability;

mod any;
mod capability;
mod data;
mod dict;
mod directory;
mod handle;
mod lazy;
mod opaque;
mod open;
mod optional;
mod receiver;
mod registry;
mod sender;
mod unit;

pub use self::any::{AnyCapability, AnyCast, ErasedCapability};
pub use self::capability::{Capability, ConversionError, RemoteError};
pub use self::data::Data;
pub use self::dict::{Dict, Key as DictKey};
pub use self::directory::Directory;
pub use self::handle::OneShotHandle;
pub use self::lazy::Lazy;
pub use self::opaque::Opaque;
pub use self::open::{Open, Path};
pub use self::optional::Optional;
pub use self::receiver::Receiver;
pub use self::sender::Sender;
pub use self::unit::Unit;
