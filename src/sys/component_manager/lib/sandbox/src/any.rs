// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{Capability, Convert, Handle, Remote, TryClone},
    crate_local::{BoxConvert, BoxRemote, TryCloneAny},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::any::{Any, TypeId},
    std::fmt::Debug,
};

/// An object-safe version of [Capability] that represents a type-erased capability.
///
/// This trait uses object-safe variants of the [Capability] supertraits, like [BoxRemote] instead
/// of [Remote]. This makes it possible to hold a capability in the [AnyCapability] trait object.
///
/// The object-safe supertraits are not meant to be used directly, and so are private to this
/// module. [AnyCast] is public and used in both [Capability] and [AnyCapability].
///
/// # Implementation details
///
/// [AnyCapability] implements [Capability] and clients call its non-object-safe trait methods.
/// The common [Capability] API is used for both concrete and type-erased capabilities.
/// [ErasedCapability] traits are used internally in this module.
///
/// For example, [Remote.to_zx_handle] on an [AnyCapability] calls the object-safe
/// [BoxRemote.to_zx_handle], which then calls [Remote.to_zx_handle] on the underlying
/// Capability type. The [Capability] traits are both entry and exit points, with
/// [ErasedCapability] traits in the middle, performing object safety conversions.
pub trait ErasedCapability:
    AnyCast + BoxConvert + BoxRemote + TryCloneAny + Debug + Send + Sync
{
}

impl<T: Capability> ErasedCapability for T {}

pub(crate) mod crate_local {
    use super::*;

    /// An object-safe version of the [Remote] trait that operates on boxed types.
    pub trait BoxRemote {
        fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>);
    }

    impl<T: Remote> BoxRemote for T {
        #[inline]
        fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
            (*self).to_zx_handle()
        }
    }

    /// An object-safe version of the [Convert] trait that operates on boxed types.
    pub trait BoxConvert {
        fn try_into_capability(self: Box<Self>, type_id: TypeId) -> Result<Box<dyn Any>, ()>;
    }

    impl<T: Convert> BoxConvert for T {
        #[inline]
        fn try_into_capability(self: Box<Self>, type_id: TypeId) -> Result<Box<dyn Any>, ()> {
            (*self).try_into_capability(type_id)
        }
    }

    /// An object-safe trait that attempts to clone as a type-erased capability.
    pub trait TryCloneAny {
        /// Attempts to clone the type-erased capability.
        fn try_clone_any(&self) -> Result<AnyCapability, ()>;
    }

    impl<T: Capability + TryClone + 'static> TryCloneAny for T {
        fn try_clone_any(&self) -> Result<AnyCapability, ()> {
            let clone = self.try_clone()?;
            let any: AnyCapability = Box::new(clone);
            Ok(any)
        }
    }
}

/// Trait object that holds any kind of capability.
pub type AnyCapability = Box<dyn ErasedCapability>;

impl Capability for AnyCapability {}

impl Remote for AnyCapability {
    #[inline]
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        self.to_zx_handle()
    }
}

impl Convert for AnyCapability {
    #[inline]
    fn try_into_capability(self, type_id: TypeId) -> Result<Box<dyn Any>, ()> {
        self.try_into_capability(type_id)
    }
}

impl TryClone for AnyCapability {
    fn try_clone(&self) -> Result<Self, ()> {
        Ok(self.as_ref().try_clone_any()?)
    }
}

impl<T: zx::HandleBased> From<T> for AnyCapability {
    fn from(value: T) -> Self {
        Box::new(Handle::from(value.into_handle()))
    }
}

/// Types implementing the [AnyCast] trait will be convertible to `dyn Any`.
pub trait AnyCast: Any {
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Any> AnyCast for T {
    #[inline]
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
    #[inline]
    fn as_any(&self) -> &dyn Any {
        self
    }
    #[inline]
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnyCapability, Capability, Convert, Handle, Remote, TryClone};
    use fuchsia_zircon::{self as zx, AsHandleRef};
    use futures::future::BoxFuture;
    use std::any::TypeId;

    /// A test-only capability that holds a Zicron handle.
    #[derive(Capability, Debug)]
    #[capability(try_clone = "err")]
    struct TestHandle(zx::Handle);

    impl Remote for TestHandle {
        fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
            (self.0, None)
        }
    }

    impl Convert for TestHandle {
        fn try_into_capability(
            self,
            type_id: std::any::TypeId,
        ) -> Result<Box<dyn std::any::Any>, ()> {
            assert_eq!(type_id, TypeId::of::<Handle>());
            Ok(Box::new(Handle::from(self.0)))
        }
    }

    /// Tests that [AnyCapability] can be converted to a zx handle.
    ///
    /// This exercises that the [Remote] implementation delegates to the the underlying
    /// Capability's [Remote] through [BoxRemote].
    #[test]
    fn test_any_remote() {
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let cap = TestHandle(event.into());
        let any: AnyCapability = Box::new(cap);
        let (handle, fut) = <AnyCapability as Remote>::to_zx_handle(any);

        assert_eq!(handle.get_koid().unwrap(), expected_koid);
        assert!(fut.is_none());
    }

    /// Tests that AnyCapability can be converted to another capability.
    ///
    /// This exercises that the [Convert] implementation delegates to the the underlying
    /// Capability's [Convert] through [BoxConvert].
    #[test]
    fn test_any_convert() {
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let cap = TestHandle(event.into());
        let any: AnyCapability = Box::new(cap);

        let cap = <AnyCapability as Convert>::try_into_capability(any, TypeId::of::<Handle>())
            .expect("failed to convert")
            .downcast::<Handle>()
            .unwrap();
        assert_eq!(cap.as_handle_ref().get_koid().unwrap(), expected_koid);
    }

    /// A cloneable capability that holds a string.
    #[derive(Capability, Clone, Debug)]
    #[capability(try_clone = "clone", convert = "to_self_only")]
    struct TestCloneable(pub String);

    impl Remote for TestCloneable {
        fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
            unimplemented!()
        }
    }

    /// Tests that an AnyCapability that holds a cloneable capability can be cloned.
    ///
    /// This exercises that the [TryClone] implementation delegates to the the underlying
    /// Capability's [TryClone] through [TryCloneAny].
    #[test]
    fn test_any_try_clone() {
        let cap = TestCloneable("hello".to_string());
        let any: AnyCapability = Box::new(cap);

        let any_clone = <AnyCapability as TryClone>::try_clone(&any).expect("failed to clone");
        let clone = any_clone.into_any().downcast::<TestCloneable>().unwrap();

        assert_eq!(clone.0, "hello".to_string());
    }
}
