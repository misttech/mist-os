// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{CapabilityTrait, ConversionError};
use crate_local::ObjectSafeCapability;
use dyn_clone::{clone_trait_object, DynClone};
use fidl_fuchsia_component_sandbox as fsandbox;
use std::{any::Any, fmt::Debug, sync::Arc};
use vfs::directory::entry::DirectoryEntry;

/// An object-safe version of [Capability] that represents a type-erased capability.
///
/// This trait contains object-safe methods of the [Capability] trait, making it possible to hold
/// a capability in a trait object. For example, [AnyCapability], a boxed, type-erased Capability.
///
/// The object-safe supertraits are not meant to be used directly, and so are private to this
/// module. [AnyCast] is public and used in both [Capability] and [AnyCapability].
///
/// # Implementation details
///
/// [AnyCapability] implements [Capability] and clients call its non-object-safe trait methods.
/// The common [Capability] API is used for both concrete and type-erased capabilities.
/// [ErasedCapability] traits are used internally in this module.
pub trait ErasedCapability:
    AnyCast + ObjectSafeCapability + DynClone + Debug + Send + Sync
{
}

clone_trait_object!(ErasedCapability);

impl<T: CapabilityTrait> ErasedCapability for T {}

pub(crate) mod crate_local {
    use super::*;

    /// An object-safe version of the [Capability] trait that operates on boxed types.
    pub trait ObjectSafeCapability {
        fn into_fidl(self: Box<Self>) -> fsandbox::Capability;

        fn try_into_directory_entry(
            self: Box<Self>,
        ) -> Result<Arc<dyn DirectoryEntry>, ConversionError>;
    }

    impl<T: CapabilityTrait> ObjectSafeCapability for T {
        fn into_fidl(self: Box<Self>) -> fsandbox::Capability {
            (*self).into()
        }

        fn try_into_directory_entry(
            self: Box<Self>,
        ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
            (*self).try_into_directory_entry()
        }
    }
}

/// Trait object that holds any kind of capability.
pub type AnyCapability = Box<dyn ErasedCapability>;

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
    use super::*;
    use crate::{Capability, Data, Open, Unit};
    use assert_matches::assert_matches;
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use std::sync::mpsc;
    use vfs::{execution_scope::ExecutionScope, service::endpoint};

    /// Tests that [AnyCapability] can be converted to a FIDL Capability.
    ///
    /// This exercises that the `Into<fsandbox::Capability> for AnyCapability` impl delegates
    /// to the corresponding `Into` impl on the underlying capability.
    #[test]
    fn test_into_fidl() {
        let unit = Unit::default();
        let any: Capability = unit.into();
        let fidl_capability: fsandbox::Capability = any.into();
        assert_eq!(fidl_capability, fsandbox::Capability::Unit(fsandbox::UnitCapability {}));
    }

    /// Tests that AnyCapability can be converted to Open.
    ///
    /// This exercises that the [try_into_open] implementation delegates to the the underlying
    /// Capability's [try_into_open] through [ObjectSafeCapability].
    #[fuchsia::test]
    async fn test_any_try_into_open() {
        let (tx, rx) = mpsc::channel::<()>();
        let open = Open::new(endpoint(move |_scope, _server_end| {
            tx.send(()).unwrap();
        }));
        let any: AnyCapability = Box::new(open);

        // Convert the Any back to Open.
        let open = Open::new(any.try_into_directory_entry().unwrap());
        let (ch1, _ch2) = zx::Channel::create();
        open.open(ExecutionScope::new(), fio::OpenFlags::empty(), vfs::path::Path::dot(), ch1);
        rx.recv().unwrap();
    }

    /// Tests that an AnyCapability can be cloned.
    #[test]
    fn test_any_clone() {
        let cap = Data::String("hello".to_string());
        let any: AnyCapability = Box::new(cap);

        let any_clone = any.clone();
        let clone = any_clone.into_any().downcast::<Data>().unwrap();

        assert_matches!(*clone, Data::String(string) if string == "hello".to_string());
    }
}
