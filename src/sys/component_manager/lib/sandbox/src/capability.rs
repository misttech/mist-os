// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{AnyCapability, AnyCast};
use fidl_fuchsia_component_sandbox as fsandbox;
use from_enum::FromEnum;
use fuchsia_zircon::{AsHandleRef, HandleRef};
use std::{fmt::Debug, sync::Arc};
use thiserror::Error;
use vfs::directory::entry::DirectoryEntry;

#[derive(Error, Debug, Clone)]
pub enum ConversionError {
    #[error("could not get handle: {err:?}")]
    Handle { err: fsandbox::HandleCapabilityError },
    #[error("`{0}` is not a valid `fuchsia.io` node name")]
    ParseName(#[from] vfs::name::ParseNameError),
    #[error("conversion to type is not supported")]
    NotSupported,
    #[error("value at `{key}` could not be converted")]
    Nested {
        key: String,
        #[source]
        err: Box<ConversionError>,
    },
}

/// Errors arising from conversion between Rust and FIDL types.
#[derive(Error, Debug)]
pub enum RemoteError {
    #[error("unknown FIDL variant")]
    UnknownVariant,

    #[error("unregistered capability; only capabilities created by sandbox are allowed")]
    Unregistered,
}

#[derive(FromEnum, Debug, Clone)]
pub enum Capability {
    Unit(crate::Unit),
    Sender(crate::Sender),
    Open(crate::Open),
    Dictionary(crate::Dict),
    Data(crate::Data),
    Directory(crate::Directory),
    OneShotHandle(crate::OneShotHandle),
    // The Router type is defined outside of this crate, so it needs to still be an
    // AnyCapability.
    // TODO(b/324870669): Move Router to the bedrock library.
    Router(AnyCapability),
}

impl Capability {
    pub fn to_dictionary(self) -> Option<crate::Dict> {
        match self {
            Capability::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    pub fn into_fidl(self) -> fsandbox::Capability {
        match self {
            Capability::Sender(s) => s.into_fidl(),
            Capability::Open(s) => s.into_fidl(),
            Capability::Router(s) => s.into_fidl(),
            Capability::Dictionary(s) => s.into_fidl(),
            Capability::Data(s) => s.into_fidl(),
            Capability::Unit(s) => s.into_fidl(),
            Capability::Directory(s) => s.into_fidl(),
            Capability::OneShotHandle(s) => s.into_fidl(),
        }
    }

    pub fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        match self {
            Capability::Sender(s) => s.try_into_directory_entry(),
            Capability::Open(s) => s.try_into_directory_entry(),
            Capability::Router(s) => s.try_into_directory_entry(),
            Capability::Dictionary(s) => s.try_into_directory_entry(),
            Capability::Data(s) => s.try_into_directory_entry(),
            Capability::Unit(s) => s.try_into_directory_entry(),
            Capability::Directory(s) => s.try_into_directory_entry(),
            Capability::OneShotHandle(s) => s.try_into_directory_entry(),
        }
    }
}

/// Given a reference to a handle, returns a copy of a capability from the registry that was added
/// with the handle's koid.
///
/// Returns [RemoteError::Unregistered] if the capability is not in the registry.
fn try_from_handle_in_registry<'a>(handle_ref: HandleRef<'_>) -> Result<Capability, RemoteError> {
    let koid = handle_ref.get_koid().unwrap();
    let capability = crate::registry::remove(koid).ok_or(RemoteError::Unregistered)?;
    Ok(capability)
}

impl From<Capability> for fsandbox::Capability {
    fn from(capability: Capability) -> Self {
        capability.into_fidl()
    }
}

impl TryFrom<fsandbox::Capability> for Capability {
    type Error = RemoteError;

    /// Converts the FIDL capability back to a Rust AnyCapability.
    ///
    /// In most cases, the AnyCapability was previously inserted into the registry when it
    /// was converted to a FIDL capability. This method takes it out of the registry.
    fn try_from(capability: fsandbox::Capability) -> Result<Self, Self::Error> {
        match capability {
            fsandbox::Capability::Unit(_) => Ok(crate::Unit::default().into()),
            fsandbox::Capability::Handle(client_end) => {
                try_from_handle_in_registry(client_end.as_handle_ref())
            }
            fsandbox::Capability::Data(data_capability) => {
                Ok(crate::Data::try_from(data_capability)?.into())
            }
            fsandbox::Capability::Cloneable(client_end) => {
                try_from_handle_in_registry(client_end.as_handle_ref())
            }
            fsandbox::Capability::Dictionary(client_end) => {
                let mut any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                // Cache the client end so it can be reused in future conversions to FIDL.
                {
                    match any {
                        Capability::Dictionary(ref mut d) => {
                            d.set_client_end(client_end);
                        }
                        _ => panic!("BUG: registry has a non-Dict capability under a Dict koid"),
                    }
                }
                Ok(any)
            }
            fsandbox::Capability::Sender(client_end) => {
                let mut any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                // Cache the client end so it can be reused in future conversions to FIDL.
                {
                    match any {
                        Capability::Sender(ref mut s) => {
                            s.set_client_end(client_end);
                        }
                        _ => {
                            panic!("BUG: registry has a non-Sender capability under a Sender koid")
                        }
                    }
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
                    match any {
                        Capability::Directory(ref mut d) => {
                            d.set_client_end(client_end);
                        }
                        _ => panic!(
                            "BUG: registry has a non-Directory capability under a Directory koid"
                        ),
                    }
                }
                Ok(any)
            }
            fsandbox::CapabilityUnknown!() => Err(RemoteError::UnknownVariant),
        }
    }
}

/// The capability trait, implemented by all capabilities.
pub trait CapabilityTrait:
    AnyCast + Into<fsandbox::Capability> + Clone + Debug + Send + Sync
{
    fn into_fidl(self) -> fsandbox::Capability {
        self.into()
    }

    /// Attempt to convert `self` to a DirectoryEntry which can be served in a
    /// VFS.
    ///
    /// The default implementation always returns an error.
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Err(ConversionError::NotSupported)
    }
}
