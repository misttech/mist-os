// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, Remote},
    core::fmt,
    fidl::endpoints::ClientEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::sync::Mutex,
};

/// A capability that is a `fuchsia.io` directory.
///
/// The directory may optionally be backed by a future that serves its contents.
#[derive(Capability)]
#[capability(try_clone = "err", convert = "to_self_only")]
pub struct Directory {
    client_end: ClientEnd<fio::DirectoryMarker>,

    // The mutex makes the Directory implement `Sync`.
    future: Mutex<Option<BoxFuture<'static, ()>>>,
}

impl Directory {
    /// Create a new [Directory] capability.
    ///
    /// Arguments:
    ///
    /// * `client_end` - A `fuchsia.io/Directory` client endpoint.
    ///
    /// * `future` - If present, the future will serve the contents in the directory.
    ///
    pub fn new(
        client_end: ClientEnd<fio::DirectoryMarker>,
        future: Option<BoxFuture<'static, ()>>,
    ) -> Self {
        Directory { client_end, future: Mutex::new(future) }
    }
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory").field("client_end", &self.client_end).finish()
    }
}

impl From<ClientEnd<fio::DirectoryMarker>> for Directory {
    fn from(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end, future: Mutex::new(None) }
    }
}

impl Remote for Directory {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (self.client_end.into(), self.future.into_inner().unwrap())
    }
}
