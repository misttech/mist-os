// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::registry::try_from_handle_in_registry;
use crate::{Capability, ConversionError, RemotableCapability, RemoteError};
use fidl::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;

impl From<Capability> for fsandbox::Capability {
    fn from(capability: Capability) -> Self {
        match capability {
            Capability::Connector(s) => s.into(),
            Capability::DirEntry(s) => s.into(),
            Capability::Router(s) => s.into(),
            Capability::Dictionary(s) => s.into(),
            Capability::Data(s) => s.into(),
            Capability::Unit(s) => s.into(),
            Capability::Directory(s) => s.into(),
            Capability::Handle(s) => s.into(),
            Capability::Instance(s) => s.into(),
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
            fsandbox::Capability::Handle(handle) => Ok(crate::Handle::new(handle).into()),
            fsandbox::Capability::Data(data_capability) => {
                Ok(crate::Data::try_from(data_capability)?.into())
            }
            fsandbox::Capability::Dictionary(dict) => Ok(crate::Dict::try_from(dict)?.into()),
            fsandbox::Capability::Connector(connector) => {
                let any = try_from_handle_in_registry(connector.token.as_handle_ref())?;
                match &any {
                    Capability::Connector(_) => (),
                    _ => panic!(
                        "BUG: registry has a non-Connector capability under a Connector koid"
                    ),
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
            fsandbox::Capability::DirEntry(dir_entry) => {
                let any = try_from_handle_in_registry(dir_entry.token.as_handle_ref())?;
                match &any {
                    Capability::DirEntry(_) => (),
                    _ => {
                        panic!("BUG: registry has a non-DirEntry capability under a DirEntry koid")
                    }
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
            Self::DirEntry(s) => s.try_into_directory_entry(),
            Self::Router(s) => s.try_into_directory_entry(),
            Self::Dictionary(s) => s.try_into_directory_entry(),
            Self::Data(s) => s.try_into_directory_entry(),
            Self::Unit(s) => s.try_into_directory_entry(),
            Self::Directory(s) => s.try_into_directory_entry(),
            Self::Handle(s) => s.try_into_directory_entry(),
            Self::Instance(s) => s.try_into_directory_entry(),
        }
    }
}
