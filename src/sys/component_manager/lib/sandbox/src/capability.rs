// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCapability, AnyCast},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::fmt::Debug,
};

/// The capability trait, implemented by all capabilities.
pub trait Capability:
    AnyCast + TryFrom<AnyCapability> + Convert + Remote + TryClone + Debug + Send + Sync
{
}

/// Trait for capabilities that can be transferred as a Zircon handle.
pub trait Remote {
    /// Convert this capability to a Zircon handle.
    ///
    /// This may return a future that implements the object represented by the handle. For example,
    /// the future serves one end of a channel, and the handle is the other end passed to a client.
    ///
    /// # Lifetime
    ///
    /// - If the user drops the future, work backing the handle will be terminated. For example,
    ///   if the framework serves the peer handle to this handle, the peer will be closed.
    /// - If the user drops the handle, the future will complete.
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>);
}

/// Trait for capabilities that can be converted to another type of capability.
pub trait Convert {
    /// Attempt to convert `self` to a capability of type `type_id`.
    fn try_into_capability(self, type_id: std::any::TypeId) -> Result<Box<dyn std::any::Any>, ()>;
}

/// Trait for types that can be optionally cloned.
pub trait TryClone: Sized {
    /// Attempts to create a copy of the value.
    fn try_clone(&self) -> Result<Self, ()>;
}
