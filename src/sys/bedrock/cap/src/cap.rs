// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::handle::{CloneHandle, Handle},
    downcast_rs::{impl_downcast, Downcast},
    dyn_clone::{clone_trait_object, DynClone},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
};

/// The capability trait, implemented by all capabilities.
pub trait Capability: Downcast + Remote + Send + Sync {}
impl_downcast!(Capability);

/// Trait object used to hold any kind of capability.
pub type AnyCapability = Box<dyn Capability>;

impl Capability for AnyCapability {}

impl std::fmt::Debug for AnyCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Capability").field(&self.as_any().type_id()).finish()
    }
}

impl<T> From<T> for AnyCapability
where
    T: zx::HandleBased,
{
    fn from(value: T) -> Self {
        Box::new(Handle::from(value.into_handle()))
    }
}

/// A Capability that can be cloned.
pub trait CloneCapability: Capability + DynClone {}

clone_trait_object!(CloneCapability);

impl<T: Capability + Clone> CloneCapability for T {}

pub type AnyCloneCapability = Box<dyn CloneCapability>;

impl Capability for AnyCloneCapability {}

impl TryFrom<zx::Handle> for AnyCloneCapability {
    type Error = zx::Status;

    fn try_from(value: zx::Handle) -> Result<Self, Self::Error> {
        // TODO(fxbug.dev/122024): Convert Remote trait handles back to the original type
        Ok(Box::new(CloneHandle::try_from(value)?))
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

impl Remote for AnyCapability {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (*self).to_zx_handle()
    }
}

impl Remote for AnyCloneCapability {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (*self).to_zx_handle()
    }
}
