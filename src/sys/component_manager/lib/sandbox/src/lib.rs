// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component sandbox traits and capability types.

mod any;
mod capability;
mod data;
mod dict;
mod directory;
mod handle;
mod open;
mod receiver;
mod registry;
mod sender;
mod unit;

pub use self::any::{AnyCapability, AnyCast, ErasedCapability};
pub use self::capability::{Capability, CapabilityTrait, ConversionError, RemoteError};
pub use self::data::Data;
pub use self::dict::{Dict, Key as DictKey};
pub use self::directory::Directory;
pub use self::handle::OneShotHandle;
pub use self::open::{Open, Path};
pub use self::receiver::Receiver;
pub use self::sender::{Message, Sendable, Sender};
pub use self::unit::Unit;
