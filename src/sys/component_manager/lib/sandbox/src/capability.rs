// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCapability, AnyCast},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::{any, fmt::Debug},
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum ConversionError {
    #[error("conversion to type is not supported")]
    NotSupported,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum CloneError {
    #[error("cloning is not supported")]
    NotSupported,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
#[error("the type does not support trait {0:?}")]
pub struct AsTraitError(pub std::any::TypeId);

/// Trait for capabilities that implements another trait and can provide a reference to the
/// corresponding trait object.
pub trait AsTrait {
    /// Attempt to get a `&dyn SomeTrait` based on the `type_id` of the trait object.
    /// Implementations should cast themselves to `&dyn SomeTrait`, then transmute that
    /// reference to `&dyn std::any::Any` to skip the lifetime checks.
    ///
    /// # Safety
    ///
    /// The returned reference has the same lifetime as `self`, despite `Any` implying a
    /// static lifetime. The returned reference must not outlive `self`. Instead of
    /// directly using this unsafe method, you should use the [`crate::try_as_trait`]
    /// macro instead, which will transmute the `&dyn std::any::Any` back to
    /// `&dyn SomeTrait` and ensure that lifetimes are preserved.
    unsafe fn try_as_trait(
        &self,
        type_id: std::any::TypeId,
    ) -> Result<&dyn std::any::Any, AsTraitError>;
}

/// The capability trait, implemented by all capabilities.
pub trait Capability: AsTrait + AnyCast + TryFrom<AnyCapability> + Debug + Send + Sync {
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

    /// Attempt to convert `self` to a capability of type `type_id`.
    ///
    /// The default implementation supports only the trivial conversion to `self`.
    fn try_into_capability(
        self,
        type_id: any::TypeId,
    ) -> Result<Box<dyn any::Any>, ConversionError> {
        if type_id == any::TypeId::of::<Self>() {
            return Ok(Box::new(self) as Box<dyn any::Any>);
        }
        Err(ConversionError::NotSupported)
    }

    /// Attempts to create a copy of the value.
    ///
    /// The default implementation always returns an error.
    fn try_clone(&self) -> Result<Self, CloneError> {
        Err(CloneError::NotSupported)
    }
}
