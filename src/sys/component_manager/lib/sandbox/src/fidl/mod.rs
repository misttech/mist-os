// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod capability;
mod component;
mod connector;
mod data;
mod dict;
mod directory;
mod handle;
pub mod registry;
mod router;
mod unit;

use {
    crate::ConversionError, fidl_fuchsia_component_sandbox as fsandbox, std::fmt::Debug,
    std::sync::Arc, vfs::directory::entry::DirectoryEntry,
};

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
