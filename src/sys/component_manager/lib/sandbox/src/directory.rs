// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use core::fmt;
use fidl::endpoints::{create_endpoints, ClientEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{future::BoxFuture, FutureExt};
use std::sync::Mutex;
use vfs::execution_scope::ExecutionScope;

use crate::{registry, AnyCast, Capability, ConversionError, Open};

/// A capability that is a `fuchsia.io` directory.
///
/// The directory may optionally be backed by a future that serves its contents.
#[derive(Capability)]
pub struct Directory {
    /// The FIDL representation of this [Directory].
    ///
    /// Invariant: Always Some when the Directory is outside of the registry.
    client_end: Option<ClientEnd<fio::DirectoryMarker>>,

    // The mutex makes the Directory implement `Sync`.
    future: Mutex<Option<BoxFuture<'static, ()>>>,
}

impl Directory {
    /// Create a new [Directory] capability.
    ///
    /// Arguments:
    ///
    /// * `client_end` - A `fuchsia.io/Directory` client endpoint.
    /// * `future` - If present, the future will serve the contents in the directory.
    pub fn new(
        client_end: ClientEnd<fio::DirectoryMarker>,
        future: Option<BoxFuture<'static, ()>>,
    ) -> Self {
        Directory { client_end: Some(client_end), future: Mutex::new(future) }
    }

    /// Create a new [Directory] capability that will open entries using the [Open] capability.
    ///
    /// Arguments:
    ///
    /// * `open_flags` - The flags that will be used to open a new connection from the [Open]
    ///   capability.
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

    /// Sets this directory's client end to the provided one.
    ///
    /// This should only be used to put a remoted client end back into the Directory
    /// after it is removed from the registry.
    pub(crate) fn set_client_end(&mut self, client_end: ClientEnd<fio::DirectoryMarker>) {
        self.client_end = Some(client_end)
    }
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory").field("client_end", &self.client_end).finish()
    }
}

impl Clone for Directory {
    fn clone(&self) -> Self {
        todo!("fxbug.dev/314843797: Implement Clone for sandbox::Directory")
    }
}

impl Capability for Directory {
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

impl From<ClientEnd<fio::DirectoryMarker>> for Directory {
    fn from(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end: Some(client_end), future: Mutex::new(None) }
    }
}

impl From<Directory> for ClientEnd<fio::DirectoryMarker> {
    /// Returns the `fuchsia.io.Directory` client stored in this Directory, taking it out,
    /// and moves the capability into the registry.
    ///
    /// The client end is put back when the Directory is removed from the registry.
    fn from(mut directory: Directory) -> Self {
        let client_end = directory.client_end.take().expect("BUG: missing client end");

        // Move this capability into the registry.
        let koid = client_end.get_koid().unwrap();
        let future = directory.future.lock().unwrap().take();
        if let Some(fut) = future {
            let task = fasync::Task::spawn(fut);
            registry::insert_with_task(Box::new(directory), koid, task);
        } else {
            registry::insert(Box::new(directory), koid);
        }

        client_end
    }
}

impl From<Directory> for fsandbox::Capability {
    fn from(directory: Directory) -> Self {
        Self::Directory(directory.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::ClientEnd;
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use futures::channel::oneshot;
    use lazy_static::lazy_static;
    use std::sync::Mutex;
    use test_util::Counter;
    use vfs::path::Path;

    #[fuchsia::test]
    async fn test_remote_from_open() {
        let (open_tx, open_rx) = oneshot::channel::<()>();
        let open_tx = Mutex::new(Some(open_tx));

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  _server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                open_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            },
            fio::DirentType::Directory,
        );

        let directory = open.into_directory(fio::OpenFlags::DIRECTORY);
        let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
        zx::Channel::from(client_end)
            .write(&[1], &mut [])
            .expect("should be able to write to the client endpoint");

        open_rx.await.unwrap();
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
        let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
        drop(client_end);
        assert_eq!(OPEN_COUNT.get(), 0);
    }
}
