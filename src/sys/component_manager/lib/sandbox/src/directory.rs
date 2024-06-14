// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use core::fmt;
use fidl::endpoints::ClientEnd;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

#[cfg(target_os = "fuchsia")]
use fidl::handle::{AsHandleRef, Channel, Handle};

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

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory").field("client_end", &self.client_end).finish()
    }
}

#[cfg(target_os = "fuchsia")]
impl Clone for Directory {
    fn clone(&self) -> Self {
        // Call `fuchsia.io/Directory.Clone` without converting the ClientEnd into a proxy.
        // This is necessary because we the conversion consumes the ClientEnd, but we can't take
        // it out of non-mut `&self`.
        let (clone_client_end, clone_server_end) = Channel::create();
        let raw_handle = self.client_end.as_handle_ref().raw_handle();
        // SAFETY: the channel is forgotten at the end of scope so it is not double closed.
        unsafe {
            let borrowed: Channel = Handle::from_raw(raw_handle).into();
            let directory = fio::DirectorySynchronousProxy::new(borrowed);
            let _ = directory.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, clone_server_end.into());
            std::mem::forget(directory.into_channel());
        }
        let client_end: ClientEnd<fio::DirectoryMarker> = clone_client_end.into();
        Self { client_end: client_end }
    }
}

#[cfg(not(target_os = "fuchsia"))]
impl Clone for Directory {
    fn clone(&self) -> Self {
        // TODO(343379094): get this to work on host.
        let (client_end, _server_end) = fidl::endpoints::create_endpoints();
        Self { client_end }
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
