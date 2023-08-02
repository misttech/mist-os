// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component sandbox traits and capability types.

mod capability;
pub mod dict;
pub mod handle;
pub mod open;

pub use self::capability::{
    AnyCapability, AnyCloneCapability, Capability, CloneCapability, Remote, TryIntoOpen,
    TryIntoOpenError,
};
pub use self::dict::SomeDict;
pub use self::handle::{CloneHandle, Handle};
pub use self::open::Open;

pub type CloneDict = dict::Dict<AnyCloneCapability>;
pub type Dict = dict::Dict<AnyCapability>;
