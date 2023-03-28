// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component bedrock capabilities.

mod cap;
pub mod dict;
pub mod handle;
pub mod multishot;
pub mod null;
pub mod oneshot;

pub use self::cap::{AnyCapability, Capability, Remote};
pub use self::dict::Dict;
pub use self::handle::Handle;
pub use self::multishot::multishot;
pub use self::null::Null;
pub use self::oneshot::oneshot;
