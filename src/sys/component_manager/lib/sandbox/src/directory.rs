// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use core::fmt;
use fidl::endpoints::{create_endpoints, ClientEnd};
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{future::BoxFuture, FutureExt};
use std::sync::Mutex;
use vfs::execution_scope::ExecutionScope;

use crate::{AnyCast, Capability, ConversionError, Open};

/// A capability that is a `fuchsia.io` directory.
///
/// The directory may optionally be backed by a future that serves its contents.
#[derive(Capability)]
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

    /// Create a new [Directory] capability that will open entries using the [Open] capability.
    ///
    /// Arguments:
    ///
    /// * `open_flags` - The flags that will be used to open a new connection from the [Open]
    ///   capability.
    ///
    pub fn from_open(open: Open, open_flags: fio::OpenFlags) -> Self {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
        // If this future is dropped, stop serving the connection.
        let guard = scopeguard::guard(scope.clone(), move |scope| {
            scope.shutdown();
        });
        let fut = async move {
            let _guard = guard;
            // Wait for the client endpoint to be written or closed. These are the only two
            // operations the client could do that warrants our attention.
            let server_end = fasync::Channel::from_channel(server_end.into_channel())
                .expect("failed to convert server_end into async channel");
            let on_signal_fut = fasync::OnSignals::new(
                &server_end,
                zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
            );
            let signals = on_signal_fut.await.unwrap();
            if signals & zx::Signals::CHANNEL_READABLE != zx::Signals::NONE {
                open.open(
                    scope.clone(),
                    open_flags,
                    vfs::path::Path::dot(),
                    server_end.into_zx_channel().into(),
                );
            }
            scope.wait().await;
        }
        .boxed();

        Self::new(client_end, Some(fut))
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

impl Capability for Directory {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (self.client_end.into(), self.future.into_inner().unwrap())
    }

    fn try_into_capability(
        self,
        type_id: std::any::TypeId,
    ) -> Result<Box<dyn std::any::Any>, ConversionError> {
        if type_id == std::any::TypeId::of::<Self>() {
            return Ok(Box::new(self).into_any());
        }
        Err(ConversionError::NotSupported)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, fuchsia_zircon as zx, lazy_static::lazy_static, test_util::Counter,
        vfs::path::Path,
    };

    #[fuchsia::test]
    async fn test_remote_from_open() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let directory = open.into_directory(fio::OpenFlags::DIRECTORY);
        let (client_end, fut) = directory.to_zx_handle();
        zx::Channel::from(client_end)
            .write(&[1], &mut [])
            .expect("should be able to write to the client endpoint");
        fut.unwrap().await;
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    #[fuchsia::test]
    async fn test_remote_from_open_not_used_if_not_written_to() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let directory = open.into_directory(fio::OpenFlags::DIRECTORY);
        let (client_end, fut) = directory.to_zx_handle();
        drop(client_end);
        fut.unwrap().await;
        assert_eq!(OPEN_COUNT.get(), 0);
    }
}
