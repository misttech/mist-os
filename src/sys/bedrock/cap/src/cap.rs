// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    downcast_rs::{impl_downcast, Downcast},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
};

/// The capability trait, implemented by all capabilities.
pub trait Capability: Downcast + Remote + Send + Sync {}
impl_downcast!(Capability);

/// Trait object used to hold any kind of capability.
pub type AnyCapability = Box<dyn Capability>;

impl std::fmt::Debug for AnyCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Capability").field(&self.as_any().type_id()).finish()
    }
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
    /// The lifetime of the future, if any, and the handle, are tied together. The handle will
    /// be closed if the future is completed, and the future will complete if the handle is closed.
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>);
}
