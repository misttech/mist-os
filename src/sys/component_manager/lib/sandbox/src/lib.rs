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
mod open;
mod optional;
mod receiver;
mod router;
mod sender;

pub use self::any::{AnyCapability, AnyCast, ErasedCapability};
pub use self::capability::{Capability, CloneError, ConversionError};
pub use self::data::Data;
pub use self::dict::{Dict, Key as DictKey, TryIntoOpenError};
pub use self::directory::Directory;
pub use self::handle::Handle;
pub use self::lazy::Lazy;
pub use self::open::Open;
pub use self::optional::Optional;
pub use self::receiver::Receiver;
pub use self::router::{route, Completer, Path, Request, Routable, Router};
pub use self::sender::Sender;
