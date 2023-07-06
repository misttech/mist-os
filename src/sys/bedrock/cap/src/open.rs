// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote},
    core::fmt,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::sync::Arc,
    vfs::{execution_scope::ExecutionScope, path::Path, remote},
};

pub type OpenFn = dyn Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) -> () + Send + Sync;

/// An [Open] capability lets the holder obtain other capabilities by pipelining
/// a [zx::Channel], usually treated as the server endpoint of some FIDL protocol.
///
/// [Open] implements [Clone] because logically one agent opening two capabilities
/// is equivalent to two agents each opening one capability through their respective
/// clones of the open capability.
#[derive(Clone)]
pub struct Open {
    open: Arc<OpenFn>,
    entry_type: fio::DirentType,
}

impl fmt::Debug for Open {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Open")
            .field("open", &"[open function]")
            .field("entry_type", &self.entry_type)
            .finish()
    }
}

impl Capability for Open {}

impl Open {
    pub fn new<T>(open: T, entry_type: fio::DirentType) -> Self
    where
        T: Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) -> () + Send + Sync + 'static,
    {
        Open { open: Arc::new(open), entry_type }
    }

    pub fn into_remote(self) -> Arc<remote::Remote> {
        let open = self.open;
        remote::remote_boxed_with_type(
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
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        // TODO: this should return a `fuchsia.io/Directory` handle with max rights.
        todo!("This should return a `fuchsia.io/Directory` handle with max rights.")
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::create_endpoints,
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
}
