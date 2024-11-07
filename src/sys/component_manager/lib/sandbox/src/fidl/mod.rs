// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod capability;
mod connector;
mod connector_router;
mod data;
mod data_router;
pub(crate) mod dict;
mod dictionary_router;
mod dir_connector;
mod dir_entry;
mod dir_entry_router;
mod directory;
mod handle;
mod instance_token;
pub(crate) mod receiver;
pub(crate) mod registry;
pub(crate) mod router;
pub(crate) mod store;
mod unit;

use crate::ConversionError;
use fidl_fuchsia_component_sandbox as fsandbox;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;

/// The trait which remotes Capabilities, either by turning them into
/// FIDL or serving them in a VFS.
pub trait RemotableCapability: Into<fsandbox::Capability> {
    /// Attempt to convert `self` to a DirectoryEntry which can be served in a
    /// VFS.
    ///
    /// The default implementation always returns an error.
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Err(ConversionError::NotSupported)
    }
}
