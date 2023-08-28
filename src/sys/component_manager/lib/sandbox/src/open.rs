// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, Remote},
    core::fmt,
    fidl::endpoints::create_endpoints,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::future::BoxFuture,
    futures::FutureExt,
    std::sync::Arc,
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path},
};

/// An [Open] capability lets the holder obtain other capabilities by pipelining
/// a [zx::Channel], usually treated as the server endpoint of some FIDL protocol.
/// We call this operation opening the capability.
///
/// ## Open via remoting
///
/// The most straightforward way to open the capability is via the [Remote] trait.
/// [Remote::to_zx_handle] will:
///
/// * Return a client endpoint.
/// * Wait for someone to use the client endpoint.
/// * Open the capability with the "." (dot) path, `open_flags`, and the corresponding
///   server endpoint.
/// * You may then serve a desired FIDL protocol on the server endpoint.
///
/// Intuitively this is opening a new connection to the current object.
///
/// ## Open via `Dict` integration
///
/// When converting a `Dict` capability to [Open], all the dictionary entries will be
/// recursively converted to [Open] capabilities. An open capability within the `Dict`
/// functions similarly to a directory entry:
///
/// * Remoting the [Open] from the `Dict` gives access to a `fuchsia.io/Directory`.
/// * Within this directory, each member [Open] will show up as a directory entry, whose
///   type is `entry_type`.
/// * When a `fuchsia.io/Directory.Open` request hits the directory, the [OpenFn] of the
///   entry matching the first path segment of the open request will be invoked, passing
///   the remaining relative path if any, or "." if the path terminates at that entry.
///
/// Intuitively this is akin to mounting a remote VFS node in a directory.
///
/// ## Cloning
///
/// [Open] implements [Clone] because logically one agent opening two capabilities
/// is equivalent to two agents each opening one capability through their respective
/// clones of the open capability.
#[derive(Capability, Clone)]
#[capability(try_clone = "clone", convert = "to_self_only")]
pub struct Open {
    open: Arc<OpenFn>,
    entry_type: fio::DirentType,
    open_flags: fio::OpenFlags,
}

/// The function that will be called when this capability is opened.
pub type OpenFn = dyn Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) -> () + Send + Sync;

impl fmt::Debug for Open {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Open")
            .field("open", &"[open function]")
            .field("entry_type", &self.entry_type)
            .field("open_flags", &self.open_flags)
            .finish()
    }
}

impl Open {
    /// Creates an [Open] capability.
    ///
    /// Arguments:
    ///
    /// * `open` - The function that will be called when this capability is opened.
    ///
    /// * `entry_type` - The type of the node that will be returned when the [Open] is placed
    ///   within a `Dict` and the user enumerates the `fuchsia.io/Directory` representation
    ///   of the dictionary.
    ///
    /// * `open_flags` - The flags that will be used to open a new connection to this capability
    ///   during [Remote].
    ///
    pub fn new<F>(open: F, entry_type: fio::DirentType, open_flags: fio::OpenFlags) -> Self
    where
        F: Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) -> () + Send + Sync + 'static,
    {
        Open { open: Arc::new(open), entry_type, open_flags }
    }

    /// Turn the [Open] into a remote VFS node.
    ///
    /// Both `into_remote` and [Remote::to_zx_handle] will let us open the capability:
    ///
    /// * `into_remote` returns a [vfs::remote::Remote] that supports
    ///   [DirectoryEntry::open].
    /// * [Remote::to_zx_handle] returns a client endpoint and calls
    ///   [DirectoryEntry::open] with the server endpoint when the client is used.
    ///
    /// `into_remote` avoids a round trip through FIDL and channels, and is used as an
    /// internal performance optimization by `Dict` when building a directory tree.
    pub(crate) fn into_remote(self) -> Arc<vfs::remote::Remote> {
        let open = self.open;
        vfs::remote::remote_boxed_with_type(
            Box::new(
                move |scope: ExecutionScope,
                      flags: fio::OpenFlags,
                      relative_path: Path,
                      server_end: ServerEnd<fio::NodeMarker>| {
                    open(scope, flags, relative_path, server_end.into_channel().into())
                },
            ),
            self.entry_type,
        )
    }
}

impl Remote for Open {
    /// Opens the capability with "." path when a request comes from the returned client endpoint.
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let open_flags = self.open_flags;
        let remote = self.into_remote();
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
                remote.clone().open(
                    scope.clone(),
                    open_flags,
                    vfs::path::Path::dot(),
                    server_end.into_zx_channel().into(),
                );
            }
            scope.wait().await;
        }
        .boxed();

        (client_end.into_handle(), Some(fut))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fuchsia_async as fasync, fuchsia_zircon as zx,
        lazy_static::lazy_static,
        test_util::Counter,
        vfs::{
            directory::{entry::DirectoryEntry, immutable::simple as pfs},
            name::Name,
        },
    };

    #[fuchsia::test]
    async fn test_into_remote() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "bar");
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
            fio::OpenFlags::DIRECTORY,
        );
        let remote = open.into_remote();
        let dir = pfs::simple();
        dir.get_or_insert(Name::from("foo").unwrap(), || remote);

        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        dir.clone().open(
            scope.clone(),
            fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, server_end) = zx::Channel::create();
        let dir = dir_client_end.channel();
        fdio::service_connect_at(dir, "foo/bar", server_end).unwrap();
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    #[fuchsia::test]
    async fn test_remote() {
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
            fio::OpenFlags::DIRECTORY,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, fut) = open.to_zx_handle();
        zx::Channel::from(client_end)
            .write(&[1], &mut [])
            .expect("should be able to write to the client endpoint");
        fut.unwrap().await;
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    #[fuchsia::test]
    async fn test_remote_not_used_if_not_written_to() {
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
            fio::OpenFlags::DIRECTORY,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, fut) = open.to_zx_handle();
        drop(client_end);
        fut.unwrap().await;
        assert_eq!(OPEN_COUNT.get(), 0);
    }
}
