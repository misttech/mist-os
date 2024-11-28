// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::CapabilityBound;
use core::fmt;
use fidl::endpoints::ClientEnd;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

/// A capability that is a `fuchsia.io` directory.
///
/// The directory may optionally be backed by a future that serves its contents.
pub struct Directory {
    /// The FIDL representation of this [Directory].
    client_end: ClientEnd<fio::DirectoryMarker>,
}

impl Directory {
    /// Create a new [Directory] capability.
    ///
    /// Arguments:
    ///
    /// * `client_end` - A `fuchsia.io/Directory` client endpoint.
    pub fn new(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end }
    }
}

impl CapabilityBound for Directory {
    fn debug_typename() -> &'static str {
        "Directory"
    }
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory").field("client_end", &self.client_end).finish()
    }
}

impl From<ClientEnd<fio::DirectoryMarker>> for Directory {
    fn from(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end }
    }
}

impl From<Directory> for ClientEnd<fio::DirectoryMarker> {
    /// Return a channel to the Directory and store the channel in
    /// the registry.
    fn from(directory: Directory) -> Self {
        let Directory { client_end } = directory;
        client_end
    }
}

impl From<Directory> for fsandbox::Capability {
    fn from(directory: Directory) -> Self {
        Self::Directory(directory.into())
    }
}
