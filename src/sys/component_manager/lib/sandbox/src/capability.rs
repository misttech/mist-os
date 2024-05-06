// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_component_sandbox as fsandbox;
use from_enum::FromEnum;
use fuchsia_zircon::{self as zx, AsHandleRef, HandleRef};
use router_error::Explain;
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

impl Explain for RemoteError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            RemoteError::UnknownVariant => zx::Status::NOT_SUPPORTED,
            RemoteError::Unregistered => zx::Status::INVALID_ARGS,
        }
    }
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
    Router(crate::Router),
    Component(crate::WeakComponentToken),
}

impl Capability {
    pub fn to_dictionary(self) -> Option<crate::Dict> {
        match self {
            Self::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    pub fn into_fidl(self) -> fsandbox::Capability {
        match self {
            Self::Sender(s) => s.into_fidl(),
            Self::Open(s) => s.into_fidl(),
            Self::Router(s) => s.into_fidl(),
            Self::Dictionary(s) => s.into_fidl(),
            Self::Data(s) => s.into_fidl(),
            Self::Unit(s) => s.into_fidl(),
            Self::Directory(s) => s.into_fidl(),
            Self::OneShotHandle(s) => s.into_fidl(),
            Self::Component(s) => s.into_fidl(),
        }
    }

    pub fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        match self {
            Self::Sender(s) => s.try_into_directory_entry(),
            Self::Open(s) => s.try_into_directory_entry(),
            Self::Router(s) => s.try_into_directory_entry(),
            Self::Dictionary(s) => s.try_into_directory_entry(),
            Self::Data(s) => s.try_into_directory_entry(),
            Self::Unit(s) => s.try_into_directory_entry(),
            Self::Directory(s) => s.try_into_directory_entry(),
            Self::OneShotHandle(s) => s.try_into_directory_entry(),
            Self::Component(s) => s.try_into_directory_entry(),
        }
    }

    pub fn debug_typename(&self) -> &'static str {
        match self {
            Self::Sender(_) => "Sender",
            Self::Open(_) => "Open",
            Self::Router(_) => "Router",
            Self::Dictionary(_) => "Dictionary",
            Self::Data(_) => "Data",
            Self::Unit(_) => "Unit",
            Self::Directory(_) => "Directory",
            Self::OneShotHandle(_) => "Handle",
            Self::Component(_) => "Component",
        }
    }
}

/// Given a reference to a handle, returns a copy of a capability from the registry that was added
/// with the handle's koid.
///
/// Returns [RemoteError::Unregistered] if the capability is not in the registry.
fn try_from_handle_in_registry<'a>(handle_ref: HandleRef<'_>) -> Result<Capability, RemoteError> {
    let koid = handle_ref.get_koid().unwrap();
    let capability = crate::registry::get(koid).ok_or(RemoteError::Unregistered)?;
    Ok(capability)
}

impl From<Capability> for fsandbox::Capability {
    fn from(capability: Capability) -> Self {
        capability.into_fidl()
    }
}

impl TryFrom<fsandbox::Capability> for Capability {
    type Error = RemoteError;

    /// Converts the FIDL capability back to a Rust Capability.
    ///
    /// In most cases, the Capability was previously inserted into the registry when it
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
            fsandbox::Capability::Dictionary(client_end) => {
                let any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                match &any {
                    Capability::Dictionary(_) => (),
                    _ => panic!("BUG: registry has a non-Dict capability under a Dict koid"),
                };
                Ok(any)
            }
            fsandbox::Capability::Sender(sender) => {
                let any = try_from_handle_in_registry(sender.token.as_handle_ref())?;
                match &any {
                    Capability::Sender(_) => (),
                    _ => panic!("BUG: registry has a non-Sender capability under a Sender koid"),
                };
                Ok(any)
            }
            fsandbox::Capability::Directory(client_end) => {
                Ok(crate::Directory::new(client_end).into())
            }
            fsandbox::Capability::Router(client_end) => {
                let any = try_from_handle_in_registry(client_end.as_handle_ref())?;
                match &any {
                    Capability::Router(_) => (),
                    _ => panic!("BUG: registry has a non-Router capability under a Router koid"),
                };
                Ok(any)
            }
            fsandbox::CapabilityUnknown!() => Err(RemoteError::UnknownVariant),
        }
    }
}

/// The capability trait, implemented by all capabilities.
pub trait CapabilityTrait: Into<fsandbox::Capability> + Clone + Debug + Send + Sync {
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
