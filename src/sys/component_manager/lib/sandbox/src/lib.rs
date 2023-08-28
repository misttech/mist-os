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
mod handle;
mod open;
mod receiver;
mod sender;

pub use self::any::{AnyCapability, AnyCast};
pub use self::capability::{Capability, Convert, Remote, TryClone};
pub use self::data::Data;
pub use self::dict::{Dict, Key as DictKey};
pub use self::handle::Handle;
pub use self::open::Open;
pub use receiver::{Message, Receiver};
pub use sender::Sender;
