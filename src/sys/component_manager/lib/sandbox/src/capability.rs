// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use from_enum::FromEnum;
use fuchsia_zircon_status as zx_status;
use router_error::Explain;
use std::fmt::Debug;
use thiserror::Error;

#[cfg(target_os = "fuchsia")]
use {
    fidl::{AsHandleRef, HandleRef},
    fidl_fuchsia_component_sandbox as fsandbox,
    std::sync::Arc,
    vfs::directory::entry::DirectoryEntry,
};

#[derive(Error, Debug, Clone)]
pub enum ConversionError {
    #[error("invalid `fuchsia.io` node name: name `{0}` is too long")]
    ParseNameErrorTooLong(String),

    #[error("invalid `fuchsia.io` node name: name cannot be empty")]
    ParseNameErrorEmpty,

    #[error("invalid `fuchsia.io` node name: name cannot be `.`")]
    ParseNameErrorDot,

    #[error("invalid `fuchsia.io` node name: name cannot be `..`")]
    ParseNameErrorDotDot,

    #[error("invalid `fuchsia.io` node name: name cannot contain `/`")]
    ParseNameErrorSlash,

    #[error("invalid `fuchsia.io` node name: name cannot contain embedded NUL")]
    ParseNameErrorEmbeddedNul,

    #[error("conversion to type is not supported")]
    NotSupported,

    #[error("value at `{key}` could not be converted")]
    Nested {
        key: String,
        #[source]
        err: Box<ConversionError>,
    },
}

#[cfg(target_os = "fuchsia")]
impl From<vfs::name::ParseNameError> for ConversionError {
    fn from(parse_name_error: vfs::name::ParseNameError) -> Self {
        match parse_name_error {
            vfs::name::ParseNameError::TooLong(s) => ConversionError::ParseNameErrorTooLong(s),
            vfs::name::ParseNameError::Empty => ConversionError::ParseNameErrorEmpty,
            vfs::name::ParseNameError::Dot => ConversionError::ParseNameErrorDot,
            vfs::name::ParseNameError::DotDot => ConversionError::ParseNameErrorDotDot,
            vfs::name::ParseNameError::Slash => ConversionError::ParseNameErrorSlash,
            vfs::name::ParseNameError::EmbeddedNul => ConversionError::ParseNameErrorEmbeddedNul,
        }
    }
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
    fn as_zx_status(&self) -> zx_status::Status {
        match self {
            RemoteError::UnknownVariant => zx_status::Status::NOT_SUPPORTED,
            RemoteError::Unregistered => zx_status::Status::INVALID_ARGS,
        }
    }
}

#[derive(FromEnum, Debug, Clone)]
pub enum Capability {
    Unit(crate::Unit),
    Connector(crate::Connector),
    Dictionary(crate::Dict),
    Data(crate::Data),
    Directory(crate::Directory),
    OneShotHandle(crate::OneShotHandle),
    Router(crate::Router),
    Component(crate::WeakComponentToken),

    #[cfg(target_os = "fuchsia")]
    Open(crate::Open),
}

impl Capability {
    pub fn to_dictionary(self) -> Option<crate::Dict> {
        match self {
            Self::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    #[cfg(target_os = "fuchsia")]
    pub fn into_fidl(self) -> fsandbox::Capability {
        match self {
            Self::Connector(s) => s.into_fidl(),
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

    #[cfg(target_os = "fuchsia")]
    pub fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        match self {
            Self::Connector(s) => s.try_into_directory_entry(),
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
            Self::Connector(_) => "Sender",
            Self::Router(_) => "Router",
            Self::Dictionary(_) => "Dictionary",
            Self::Data(_) => "Data",
            Self::Unit(_) => "Unit",
            Self::Directory(_) => "Directory",
            Self::OneShotHandle(_) => "Handle",
            Self::Component(_) => "Component",

            #[cfg(target_os = "fuchsia")]
            Self::Open(_) => "Open",
        }
    }
}

/// Given a reference to a handle, returns a copy of a capability from the registry that was added
/// with the handle's koid.
///
/// Returns [RemoteError::Unregistered] if the capability is not in the registry.
#[cfg(target_os = "fuchsia")]
fn try_from_handle_in_registry<'a>(handle_ref: HandleRef<'_>) -> Result<Capability, RemoteError> {
    let koid = handle_ref.get_koid().unwrap();
    let capability = crate::registry::get(koid).ok_or(RemoteError::Unregistered)?;
    Ok(capability)
}

#[cfg(target_os = "fuchsia")]
impl From<Capability> for fsandbox::Capability {
    fn from(capability: Capability) -> Self {
        capability.into_fidl()
    }
}

#[cfg(target_os = "fuchsia")]
impl TryFrom<fsandbox::Capability> for Capability {
    type Error = RemoteError;

    /// Converts the FIDL capability back to a Rust Capability.
    ///
    /// In most cases, the Capability was previously inserted into the registry when it
    /// was converted to a FIDL capability. This method takes it out of the registry.
    fn try_from(capability: fsandbox::Capability) -> Result<Self, Self::Error> {
        match capability {
            fsandbox::Capability::Unit(_) => Ok(crate::Unit::default().into()),
            fsandbox::Capability::Handle(handle) => {
                try_from_handle_in_registry(handle.token.as_handle_ref())
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
            fsandbox::Capability::Connector(connector) => {
                let any = try_from_handle_in_registry(connector.token.as_handle_ref())?;
                match &any {
                    Capability::Connector(_) => (),
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
#[cfg(target_os = "fuchsia")]
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
