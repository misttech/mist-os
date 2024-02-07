// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_io as fio, fuchsia_inspect as finspect,
    futures::{future::BoxFuture, FutureExt as _, SinkExt as _, StreamExt as _},
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    },
    vfs::directory::entry::DirectoryEntry as _,
};

/// `CachingPackageServer` uses `package_directory::RootDir` to serve package directory connections
/// while deduplicating the `Arc<package_directory::RootDir>`s backing those connections and
/// exposing which of the packages have open connections (useful when determining which packages
/// can be garbage collected).
///
/// Because of how RootDir is implemented, (it uses the Rust VFS to serve most of the fuchsia.io
/// connections itself and registers child connections on the parent connection's
/// `vfs::ExecutionScope`) CachingPackageServer will consider a package directory to have open
/// connections if there are:
///   1. fuchsia.io.Directory connections to the package's root directory or any sub directory.
///   2. fuchsia.io.File connections to the package's files under meta/.
///   3. fuchsia.io.File connections to the package's content blobs (files not under meta/) *iff*
///      the `package_directory::NonMetaStorage` impl provided to `new` registers those connections
///      on the provided `scope`.
///
/// The `NonMetaStorage` impl we use for Fxblob does this.
/// The impl we use for Blobfs does not do this, but Blobfs will wait to delete blobs that have
/// open connections until the last connection closes.
/// Similarly, both Blobfs and Fxblob will wait to delete blobs until the last VMO is closed (VMOs
/// obtained from fuchsia.io.File.GetBackingMemory will not keep a package alive), so it is safe to
/// delete packages that CachingPackageServer says are not open.
///
/// Clients close connections to packages by closing their end of the Zircon channel over which the
/// fuchsia.io.[File|Directory] messages were being sent. Some time after the client end of the
/// channel is closed, the server (usually in a different process) will be notified by the kernel,
/// and the server will then remove the task serving that connection from the corresponding
/// `vfs::ExecutionScope`. When the last task is removed from the scope, the cleanup future
/// returned by `new` will be notified and then (assuming the cleanup future is being polled)
/// remove the package from CachingPackageServer's cache, at which point `Self::open_packages` will
/// no longer mention the package.
/// All this is to say that there will be some delay between a package no longer being in use and
/// clients of CachingPackageServer finding out about that.
#[derive(Debug, Clone)]
pub struct CachingPackageServer<S> {
    non_meta_storage: S,
    open_packages: Arc<
        std::sync::Mutex<
            HashMap<
                fuchsia_hash::Hash,
                (vfs::execution_scope::ExecutionScope, Arc<package_directory::RootDir<S>>),
            >,
        >,
    >,
    // Sends the scope and hash for newly added packages to the cleanup future that removes
    // packages from `open_packages` when their last connection closes.
    cleanup_sender:
        futures::channel::mpsc::Sender<(vfs::execution_scope::ExecutionScope, fuchsia_hash::Hash)>,
}

impl<S: package_directory::NonMetaStorage + Clone> CachingPackageServer<S> {
    /// Creates a `CachingPackageServer` that uses `non_meta_storage` as the backing for the
    /// internally managed `package_directory::RootDir`s.
    /// Returns a `Future` that removes packages with no outstanding connections from the
    /// cache. This `Future` must be run for calls to `CachingPackageServer::serve` to complete
    /// and will itself complete when the `CachingPackageServer` is dropped and all outstanding
    /// package directory connections are closed.
    pub fn new(non_meta_storage: S) -> (Self, impl futures::Future<Output = ()>) {
        let (cleanup_sender, recv) = futures::channel::mpsc::channel::<(
            vfs::execution_scope::ExecutionScope,
            fuchsia_hash::Hash,
        )>(0);
        let open_packages = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let gc_fut = gc_closed_packages(Arc::clone(&open_packages), recv);
        (Self { non_meta_storage, open_packages, cleanup_sender }, gc_fut)
    }

    /// Uses the `non_meta_storage` provided to `Self::new` to serve the package indicated by
    /// `hash` on `server_end`.
    /// The cleanup future returned by `Self::new` must be polled for this function to complete.
    /// Panics if the cleanup future returned by `Self::new` has been dropped.
    /// If provided, `root_dir` must be backed by the same `NonMetaStorage` that `Self::new` was
    /// called with.
    pub async fn serve(
        &self,
        hash: fuchsia_hash::Hash,
        flags: fio::OpenFlags,
        server_end: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
        root_dir: Option<Arc<package_directory::RootDir<S>>>,
    ) -> Result<(), package_directory::Error> {
        {
            let packages = self.open_packages.lock().expect("poisoned mutex");
            if let Some((scope, root_dir)) = packages.get(&hash) {
                let () = Arc::clone(&root_dir).open(
                    scope.clone(),
                    flags,
                    vfs::path::Path::dot(),
                    server_end.into_channel().into(),
                );
                return Ok(());
            }
        }

        let root_dir = if let Some(root_dir) = root_dir {
            root_dir
        } else {
            // Making a RootDir takes ~100 μs (reading the meta.far), so do this without the lock.
            match package_directory::RootDir::new(self.non_meta_storage.clone(), hash).await {
                Ok(root_dir) => root_dir,
                Err(e) => {
                    let () = vfs::common::send_on_open_with_error(
                        flags.contains(fio::OpenFlags::DESCRIBE),
                        server_end.into_channel().into(),
                        (&e).into(),
                    );
                    return Err(e);
                }
            }
        };
        let scope_for_new_entry = {
            let mut packages = self.open_packages.lock().expect("poisoned mutex");
            let mut occupied;
            use std::collections::hash_map::Entry::*;
            let ((scope, root_dir), is_new) = match packages.entry(hash) {
                Occupied(o) => {
                    occupied = o;
                    (occupied.get_mut(), false)
                }
                Vacant(v) => {
                    (v.insert((vfs::execution_scope::ExecutionScope::new(), root_dir)), true)
                }
            };
            // Create the connection while the lock is held so that the cleanup future doesn't busy
            // loop if the last outstanding connection happens to close while this one is made.
            let () = root_dir.clone().open(
                scope.clone(),
                flags,
                vfs::path::Path::dot(),
                server_end.into_channel().into(),
            );
            is_new.then(|| scope.clone())
        };
        if let Some(scope) = scope_for_new_entry {
            let () = self
                .cleanup_sender
                .clone()
                .send((scope, hash))
                .await
                .expect("cache outlived cleanup future");
        }
        Ok(())
    }

    /// Packages with open fuchsia.io connections.
    pub fn open_packages(&self) -> HashSet<fuchsia_hash::Hash> {
        self.open_packages.lock().expect("poisoned mutex").keys().copied().collect::<HashSet<_>>()
    }

    /// Returns a callback to be given to `finspect::Node::record_lazy_child`.
    /// Records the package hashes and their corresponding number of open non-blob connections.
    pub fn record_lazy_inspect(
        &self,
    ) -> impl Fn() -> BoxFuture<'static, Result<finspect::Inspector, anyhow::Error>>
           + Send
           + Sync
           + 'static {
        let open_packages = Arc::downgrade(&self.open_packages);
        move || {
            let open_packages = open_packages.clone();
            async move {
                let inspector = finspect::Inspector::default();
                if let Some(open_packages) = open_packages.upgrade() {
                    let package_counts: HashMap<_, _> = {
                        let open_packages = open_packages.lock().expect("poisoned mutex");
                        open_packages
                            .iter()
                            .map(|(k, v)| (*k, Arc::strong_count(&v.1) as i64 - 1))
                            .collect()
                    };
                    let root = inspector.root();
                    let () = package_counts.into_iter().for_each(|(pkg, count)| {
                        root.record_child(pkg.to_string(), |n| {
                            n.record_int("non-blob connections", count)
                        })
                    });
                }
                Ok(inspector)
            }
            .boxed()
        }
    }
}

/// Receives the (scope, hash) of packages when they are first added to the cache.
/// Waits for the each scope to finish (i.e. all its connections to close) and then removes the
/// corresponding package from the cache.
async fn gc_closed_packages<S: package_directory::NonMetaStorage + Clone>(
    open_packages: Arc<
        std::sync::Mutex<
            HashMap<
                fuchsia_hash::Hash,
                (vfs::execution_scope::ExecutionScope, Arc<package_directory::RootDir<S>>),
            >,
        >,
    >,
    mut newly_opened_packages: futures::channel::mpsc::Receiver<(
        vfs::execution_scope::ExecutionScope,
        fuchsia_hash::Hash,
    )>,
) {
    async fn wait_for_close(
        scope: vfs::execution_scope::ExecutionScope,
        hash: fuchsia_hash::Hash,
    ) -> fuchsia_hash::Hash {
        let () = scope.wait().await;
        hash
    }

    let mut close_events = futures::stream::FuturesUnordered::new();
    loop {
        futures::select! {
            (scope, hash) = newly_opened_packages.select_next_some() => {
                let () = close_events.push(wait_for_close(scope, hash));
            }
            hash = close_events.select_next_some() => {
                let mut packages = open_packages.lock().expect("poisoned mutex");
                let (scope, root_dir) = packages.get(&hash).expect(
                    "a close event is created either when a package is added to the cache or \
                     by this function when a close event is observed for a package that has \
                     outstanding references and packages are only removed from the cache when \
                     their close event occurs"
                );
                if Arc::strong_count(&root_dir) == 1 {
                    assert!(packages.remove(&hash).is_some(), "get just succeeded with lock held");
                } else {
                    // A new connection was created in between the last outstanding connection
                    // closing and this cleanup code running. Register another cleanup because the
                    // new connection (to the RootDir already in the cache) did not.
                    let () = close_events.push(wait_for_close(scope.clone(), hash));
                }
            }
            complete => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        diagnostics_assertions::assert_data_tree,
        fuchsia_pkg_testing::{blobfs::Fake as FakeBlobfs, PackageBuilder},
        fuchsia_zircon as zx,
    };

    #[fuchsia::test]
    async fn serve() {
        let pkg = PackageBuilder::new("pkg-name").build().await.unwrap();
        let (metafar_blob, _) = pkg.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let (server, cleanup_fut) = CachingPackageServer::new(blobfs_client);
        let cleanup_fut = fuchsia_async::Task::spawn(cleanup_fut);

        let (proxy, server_end) = fidl::endpoints::create_proxy().unwrap();
        let () = server
            .serve(metafar_blob.merkle, fio::OpenFlags::RIGHT_READABLE, server_end, None)
            .await
            .unwrap();

        let _: fio::ConnectionInfo =
            proxy.get_connection_info().await.expect("directory succesfully handling requests");
        assert_eq!(server.open_packages().len(), 1);

        // The detached cleanup_fut removes the now-closed package asynchronously.
        drop(proxy);
        while server.open_packages().len() != 0 {
            let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        }

        drop(server);
        let () = cleanup_fut.await;
    }

    #[fuchsia::test]
    async fn serve_deduplicates() {
        let pkg = PackageBuilder::new("pkg-name").build().await.unwrap();
        let (metafar_blob, _) = pkg.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let (server, cleanup_fut) = CachingPackageServer::new(blobfs_client);
        let cleanup_fut = fuchsia_async::Task::spawn(cleanup_fut);

        let (proxy0, server_end) = fidl::endpoints::create_proxy().unwrap();
        let () = server
            .serve(metafar_blob.merkle, fio::OpenFlags::RIGHT_READABLE, server_end, None)
            .await
            .unwrap();
        // One Arc in the cache and one Arc in the VFS connection.
        assert_eq!(
            Arc::strong_count(
                &server.open_packages.lock().unwrap().get(&metafar_blob.merkle).unwrap().1
            ),
            2
        );

        let (proxy1, server_end) = fidl::endpoints::create_proxy().unwrap();
        let () = server
            .serve(metafar_blob.merkle, fio::OpenFlags::RIGHT_READABLE, server_end, None)
            .await
            .unwrap();
        // One Arc in the cache and two Arcs in the VFS connections.
        assert_eq!(
            Arc::strong_count(
                &server.open_packages.lock().unwrap().get(&metafar_blob.merkle).unwrap().1
            ),
            3
        );

        // Dropping the proxy causes the async task spawned by the VFS library that manages the
        // connection to asynchronously drop its instance of the Arc.
        drop(proxy1);
        while Arc::strong_count(
            &server.open_packages.lock().unwrap().get(&metafar_blob.merkle).unwrap().1,
        ) != 2
        {
            let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        }
        // The still open connection to the root dir should keep the package in the cache, even
        // if we sleep to give the cleanup_fut a chance to run.
        let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        assert_eq!(server.open_packages().len(), 1);

        // Closing the final connection should result in the cleanup_fut asynchronously removing
        // the package from the cache.
        drop(proxy0);
        while server.open_packages().len() != 0 {
            let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        }

        drop(server);
        let () = cleanup_fut.await;
    }

    #[fuchsia::test]
    async fn tracks_subdirectory_connections() {
        let pkg = PackageBuilder::new("pkg-name")
            .add_resource_at("content-dir/a-blob", &b""[..])
            .build()
            .await
            .unwrap();
        let (metafar_blob, _) = pkg.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let (server, cleanup_fut) = CachingPackageServer::new(blobfs_client);
        let cleanup_fut = fuchsia_async::Task::spawn(cleanup_fut);

        let (proxy, server_end) = fidl::endpoints::create_proxy().unwrap();
        let () = server
            .serve(metafar_blob.merkle, fio::OpenFlags::RIGHT_READABLE, server_end, None)
            .await
            .unwrap();
        // One Arc in the cache and one Arc in the VFS connection.
        assert_eq!(
            Arc::strong_count(
                &server.open_packages.lock().unwrap().get(&metafar_blob.merkle).unwrap().1
            ),
            2
        );

        let content_dir = fuchsia_fs::directory::open_directory(
            &proxy,
            "content-dir",
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await
        .unwrap();
        // A third Arc in the VFS connection to the subdir.
        assert_eq!(
            Arc::strong_count(
                &server.open_packages.lock().unwrap().get(&metafar_blob.merkle).unwrap().1
            ),
            3
        );

        // Dropping the root dir proxy causes the async task spawned by the VFS library that
        // manages the connection to asynchronously drop its instance of the Arc.
        drop(proxy);
        while Arc::strong_count(
            &server.open_packages.lock().unwrap().get(&metafar_blob.merkle).unwrap().1,
        ) != 2
        {
            let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        }
        // The still open connection to the subdirectory should keep the package in the cache, even
        // if we sleep to give the cleanup_fut a chance to run.
        let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        assert_eq!(server.open_packages().len(), 1);

        // Closing the final connection should result in the cleanup_fut asynchronously removing
        // the package from the cache.
        drop(content_dir);
        while server.open_packages().len() != 0 {
            let () = fuchsia_async::Timer::new(std::time::Duration::from_millis(5)).await;
        }

        drop(server);
        let () = cleanup_fut.await;
    }

    #[fuchsia::test]
    async fn root_dir_creation_error_sends_on_open_event() {
        let (_blobfs_fake, blobfs_client) = FakeBlobfs::new();
        let (server, cleanup_fut) = CachingPackageServer::new(blobfs_client);
        let cleanup_fut = fuchsia_async::Task::spawn(cleanup_fut);

        let (proxy, server_end) = fidl::endpoints::create_proxy().unwrap();
        assert_matches!(
            server.serve([0; 32].into(), fio::OpenFlags::RIGHT_READABLE, server_end, None).await,
            Err(package_directory::Error::MissingMetaFar)
        );

        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FOUND, .. }))
        );
        assert_eq!(server.open_packages().len(), 0);

        drop(server);
        let () = cleanup_fut.await;
    }

    #[fuchsia::test]
    async fn inspect() {
        let pkg = PackageBuilder::new("pkg-name").build().await.unwrap();
        let (metafar_blob, _) = pkg.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let (server, cleanup_fut) = CachingPackageServer::new(blobfs_client);
        let cleanup_fut = fuchsia_async::Task::spawn(cleanup_fut);

        let (proxy, server_end) = fidl::endpoints::create_proxy().unwrap();
        let () = server
            .serve(metafar_blob.merkle, fio::OpenFlags::RIGHT_READABLE, server_end, None)
            .await
            .unwrap();

        let inspector = finspect::Inspector::default();
        inspector.root().record_lazy_child("open-packages", server.record_lazy_inspect());

        assert_data_tree!(inspector, root: {
            "open-packages": {
                pkg.hash().to_string() => {
                    "non-blob connections": 1i64,
                },
            }
        });

        drop(proxy);
        drop(server);
        let () = cleanup_fut.await;
    }
}
