// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        handle::{CloneHandle, Handle},
        open::Open,
    },
    downcast_rs::{impl_downcast, Downcast},
    dyn_clone::{clone_trait_object, DynClone},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
    thiserror::Error,
};

/// The capability trait, implemented by all capabilities.
pub trait Capability: Downcast + Remote + TryIntoOpen + Send + Sync {}
impl_downcast!(Capability);

/// Trait object used to hold any kind of capability.
pub type AnyCapability = Box<dyn Capability>;

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
    /// - If the user drops the future, work backing the handle will be terminated. For example,
    ///   if the framework serves the peer handle to this handle, the peer will be closed.
    /// - If the user drops the handle, the future will complete.
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>);
}

/// This error is returned when a capability cannot be converted into an [Open] capability.
#[derive(Error, Debug)]
pub enum TryIntoOpenError {
    /// Converting to [Open] involved a string name that is not a valid `fuchsia.io` node name.
    #[error(
        "converting to Open involved a string name that is not a valid `fuchsia.io` node name"
    )]
    ParseNameError(#[from] vfs::name::ParseNameError),

    /// The capability does not support converting into an [Open] capability.
    #[error("the capability does not support converting into an Open capability")]
    DoesNotSupportOpen,
}

/// This trait is introduced as an extra indirection behind [TryInto<Open>] because
/// [TryInto] is not object-safe [1]. The type-erased `dyn Capability` will require all
/// traits behind `Capability` to be object-safe.
///
/// [1]: https://doc.rust-lang.org/reference/items/traits.html#object-safety
pub trait TryIntoOpen {
    /// Attempt to transform the capability into an [Open] capability, which let us mount it
    /// as a node inside a `fuchsia.io` directory and open it using `fuchsia.io/Directory.Open`.
    fn try_into_open(self: Box<Self>) -> Result<Open, TryIntoOpenError> {
        Err(TryIntoOpenError::DoesNotSupportOpen)
    }
}

impl<T: Into<Open>> TryIntoOpen for T {
    fn try_into_open(self: Box<Self>) -> Result<Open, TryIntoOpenError> {
        Ok((*self).into())
    }
}

impl TryInto<Open> for AnyCapability {
    type Error = TryIntoOpenError;

    fn try_into(self: Self) -> Result<Open, Self::Error> {
        self.try_into_open()
    }
}

impl TryInto<Open> for AnyCloneCapability {
    type Error = TryIntoOpenError;

    fn try_into(self: Self) -> Result<Open, Self::Error> {
        self.try_into_open()
    }
}
