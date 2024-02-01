// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    registry, Capability, ConversionError, Data, Dict, Directory, Open, Optional, Receiver,
    RemoteError, Sender, Unit,
};
use crate_local::ObjectSafeCapability;
use dyn_clone::{clone_trait_object, DynClone};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon::{AsHandleRef, HandleRef};
use std::any::Any;
use std::fmt::Debug;
use std::ops::DerefMut;

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

impl<T: Capability> ErasedCapability for T {}

pub(crate) mod crate_local {
    use super::*;

    /// An object-safe version of the [Capability] trait that operates on boxed types.
    pub trait ObjectSafeCapability {
        fn into_fidl(self: Box<Self>) -> fsandbox::Capability;

        fn try_into_open(self: Box<Self>) -> Result<Open, ConversionError>;
    }

    impl<T: Capability> ObjectSafeCapability for T {
        fn into_fidl(self: Box<Self>) -> fsandbox::Capability {
            (*self).into()
        }

        fn try_into_open(self: Box<Self>) -> Result<Open, ConversionError> {
            (*self).try_into_open()
        }
    }
}

/// Trait object that holds any kind of capability.
pub type AnyCapability = Box<dyn ErasedCapability>;

impl Capability for AnyCapability {
    #[inline]
    fn into_fidl(self) -> fsandbox::Capability {
        self.into_fidl()
    }

    #[inline]
    fn try_into_open(self) -> Result<Open, ConversionError> {
        self.try_into_open()
    }
}

impl TryFrom<fsandbox::Capability> for AnyCapability {
    type Error = RemoteError;

    /// Converts the FIDL capability back to a Rust AnyCapability.
    ///
    /// In most cases, the AnyCapability was previously inserted into the registry when it
    /// was converted to a FIDL capability. This method takes it out of the registry.
    fn try_from(capability: fsandbox::Capability) -> Result<Self, Self::Error> {
        match capability {
            fsandbox::Capability::Unit(_) => Ok(Box::new(Unit::default())),
            fsandbox::Capability::Opaque(handle) => {
                try_from_handle_in_registry(handle.as_handle_ref())
            }
            fsandbox::Capability::Handle(client_end) => {
                try_from_handle_in_registry(client_end.as_handle_ref())
            }
            fsandbox::Capability::Data(data_capability) => {
                Ok(Box::new(Data::try_from(data_capability)?))
            }
            fsandbox::Capability::Cloneable(client_end) => {
                try_from_handle_in_registry(client_end.as_handle_ref())
            }
            fsandbox::Capability::Dictionary(client_end) => {
                let mut any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                // Cache the client end so it can be reused in future conversions to FIDL.
                {
                    let dict: &mut Dict = any
                        .deref_mut()
                        .try_into()
                        .expect("BUG: registry has a non-Dict capability under a Dict koid");
                    dict.set_client_end(client_end);
                }
                Ok(any)
            }
            fsandbox::Capability::Sender(client_end) => {
                let mut any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                // Cache the client end so it can be reused in future conversions to FIDL.
                {
                    // FIXME: We need a concrete Sender type here but don't know the generic
                    // type, so use (). This should be fixed by making Sender
                    // non-generic.
                    let sender: &mut Sender = any
                        .deref_mut()
                        .try_into()
                        .expect("BUG: registry has a non-Sender capability under a Sender koid");
                    sender.set_client_end(client_end);
                }
                Ok(any)
            }
            fsandbox::Capability::Receiver(server_end) => {
                let mut any = try_from_handle_in_registry(server_end.as_handle_ref())?;
                // Cache the client end so it can be reused in future conversions to FIDL.
                {
                    // FIXME: We need a concrete Receiver type here but don't know the generic
                    // type, so use (). This should be fixed by making Receiver
                    // non-generic.
                    let receiver: &mut Receiver = any.deref_mut().try_into().expect(
                        "BUG: registry has a non-Receiver capability under a Receiver koid",
                    );
                    receiver.set_server_end(server_end);
                }
                Ok(any)
            }
            fsandbox::Capability::Open(client_end) => {
                try_from_handle_in_registry(client_end.as_handle_ref())
            }
            fsandbox::Capability::Directory(client_end) => {
                let mut any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                // Cache the client end so it can be reused in future conversions to FIDL.
                {
                    let directory: &mut Directory = any.deref_mut().try_into().expect(
                        "BUG: registry has a non-Directory capability under a Directory koid",
                    );
                    directory.set_client_end(client_end);
                }
                Ok(any)
            }
            fsandbox::Capability::Optional(optional) => match optional.value {
                Some(capability) => (*capability).try_into(),
                None => Ok(Box::new(Optional::void())),
            },
            fsandbox::CapabilityUnknown!() => Err(RemoteError::UnknownVariant),
        }
    }
}

/// Given a reference to a handle, returns a copy of a capability from the registry that was added
/// with the handle's koid.
///
/// Returns [RemoteError::Unregistered] if the capability is not in the registry.
fn try_from_handle_in_registry<'a>(
    handle_ref: HandleRef<'_>,
) -> Result<AnyCapability, RemoteError> {
    let koid = handle_ref.get_koid().unwrap();
    let capability = registry::remove(koid).ok_or(RemoteError::Unregistered)?;
    Ok(capability)
}

impl From<AnyCapability> for fsandbox::Capability {
    fn from(any: AnyCapability) -> Self {
        any.into_fidl()
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
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use std::sync::mpsc;
    use vfs::execution_scope::ExecutionScope;

    /// Tests that [AnyCapability] can be converted to a FIDL Capability.
    ///
    /// This exercises that the `Into<fsandbox::Capability> for AnyCapability` impl delegates
    /// to the corresponding `Into` impl on the underlying capability.
    #[test]
    fn test_into_fidl() {
        let unit = Unit::default();
        let any: AnyCapability = Box::new(unit);
        let fidl_capability: fsandbox::Capability = any.into();
        assert_eq!(fidl_capability, fsandbox::Capability::Unit(fsandbox::UnitCapability {}));
    }

    /// Tests that AnyCapability can be converted to Open.
    ///
    /// This exercises that the [try_into_open] implementation delegates to the the underlying
    /// Capability's [try_into_open] through [ObjectSafeCapability].
    #[test]
    fn test_any_try_into_open() {
        let (tx, rx) = mpsc::channel::<()>();
        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  _relative_path: vfs::path::Path,
                  _server_end: zx::Channel| {
                tx.send(()).unwrap();
            },
            fio::DirentType::Unknown,
        );
        let any: AnyCapability = Box::new(open);

        // Convert the Any back to Open.
        let open = any.try_into_open().unwrap();
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
