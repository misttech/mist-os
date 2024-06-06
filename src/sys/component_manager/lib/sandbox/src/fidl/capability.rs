// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{Capability, ConversionError, RemotableCapability, RemoteError},
    fidl::{AsHandleRef, HandleRef},
    fidl_fuchsia_component_sandbox as fsandbox,
    std::sync::Arc,
    vfs::directory::entry::DirectoryEntry,
};

/// Given a reference to a handle, returns a copy of a capability from the registry that was added
/// with the handle's koid.
///
/// Returns [RemoteError::Unregistered] if the capability is not in the registry.
fn try_from_handle_in_registry<'a>(handle_ref: HandleRef<'_>) -> Result<Capability, RemoteError> {
    let koid = handle_ref.get_koid().unwrap();
    let capability = crate::fidl::registry::get(koid).ok_or(RemoteError::Unregistered)?;
    Ok(capability)
}

impl From<Capability> for fsandbox::Capability {
    fn from(capability: Capability) -> Self {
        match capability {
            Capability::Connector(s) => s.into(),
            Capability::Open(s) => s.into(),
            Capability::Router(s) => s.into(),
            Capability::Dictionary(s) => s.into(),
            Capability::Data(s) => s.into(),
            Capability::Unit(s) => s.into(),
            Capability::Directory(s) => s.into(),
            Capability::OneShotHandle(s) => s.into(),
            Capability::Component(s) => s.into(),
        }
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

impl RemotableCapability for Capability {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
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
}
