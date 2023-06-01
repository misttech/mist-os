// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::base_packages::BasePackages,
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_hash::Hash,
    fuchsia_pkg::{PackageName, PackageVariant},
    fuchsia_zircon as zx,
    std::{
        collections::{BTreeMap, HashMap},
        str::FromStr,
        sync::Arc,
    },
    vfs::{
        directory::{
            dirents_sink,
            entry::{DirectoryEntry, EntryInfo},
            entry_container::{Directory, DirectoryWatcher},
            immutable::connection::io1::ImmutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        path::Path,
        ToObjectRequest,
    },
};

mod variants;
use variants::PkgfsPackagesVariants;

#[derive(Debug)]
pub struct PkgfsPackages {
    base_packages: Arc<BasePackages>,
    blobfs: blobfs::Client,
}

impl PkgfsPackages {
    pub fn new(base_packages: Arc<BasePackages>, blobfs: blobfs::Client) -> Self {
        Self { base_packages, blobfs }
    }

    async fn packages(&self) -> HashMap<PackageName, HashMap<PackageVariant, Hash>> {
        let paths_and_hashes = self.base_packages.root_paths_and_hashes();
        let mut res: HashMap<PackageName, HashMap<PackageVariant, Hash>> =
            HashMap::with_capacity(paths_and_hashes.len());
        for (path, hash) in paths_and_hashes {
            let name = path.name().to_owned();
            let variant = path.variant().to_owned();
            res.entry(name).or_default().insert(variant, *hash);
        }
        res
    }

    async fn package_variants(&self, name: &PackageName) -> Option<HashMap<PackageVariant, Hash>> {
        self.packages().await.remove(name)
    }

    async fn directory_entries(&self) -> BTreeMap<String, super::DirentType> {
        self.packages()
            .await
            .into_keys()
            .map(|k| (k.into(), super::DirentType::Directory))
            .collect()
    }
}

impl DirectoryEntry for PkgfsPackages {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        mut path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let flags = flags.difference(fio::OpenFlags::POSIX_WRITABLE);
        flags.to_object_request(server_end).handle(|object_request| {
            // This directory and all child nodes are read-only
            if flags.intersects(
                fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::CREATE
                    | fio::OpenFlags::CREATE_IF_ABSENT
                    | fio::OpenFlags::TRUNCATE
                    | fio::OpenFlags::APPEND,
            ) {
                return Err(zx::Status::NOT_SUPPORTED);
            }

            match path.next().map(PackageName::from_str) {
                None => {
                    object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
                }
                Some(Ok(package_name)) => {
                    let object_request = object_request.take();
                    scope.clone().spawn(async move {
                        match self.package_variants(&package_name).await {
                            Some(variants) => {
                                Arc::new(PkgfsPackagesVariants::new(variants, self.blobfs.clone()))
                                    .open(scope, flags, path, object_request.into_server_end());
                            }
                            None => object_request.shutdown(zx::Status::NOT_FOUND),
                        }
                    });
                    Ok(())
                }
                Some(Err(_)) => {
                    // Names that are not valid package names can't exist in this directory.
                    Err(zx::Status::NOT_FOUND)
                }
            }
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

#[async_trait]
impl Directory for PkgfsPackages {
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<(dyn dirents_sink::Sink + 'static)>,
    ) -> Result<(TraversalPosition, Box<(dyn dirents_sink::Sealed + 'static)>), zx::Status> {
        // If directory contents changes in between a client making paginated
        // fuchsia.io/Directory.ReadDirents calls, the client may not see a consistent snapshot
        // of the directory contents.
        super::read_dirents(&self.directory_entries().await, pos, sink).await
    }

    fn register_watcher(
        self: Arc<Self>,
        _: ExecutionScope,
        _: fio::WatchMask,
        _: DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    // `register_watcher` is unsupported so this is a no-op.
    fn unregister_watcher(self: Arc<Self>, _: usize) {}

    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_DIRECTORY,
            id: 1,
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    fn close(&self) -> Result<(), zx::Status> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::compat::pkgfs::testing::FakeSink,
        assert_matches::assert_matches,
        fuchsia_pkg::PackagePath,
        fuchsia_pkg_testing::{blobfs::Fake as FakeBlobfs, PackageBuilder},
        maplit::{convert_args, hashmap},
        std::collections::HashSet,
    };

    impl PkgfsPackages {
        pub fn new_test(base_packages: impl IntoIterator<Item = (PackagePath, Hash)>) -> Arc<Self> {
            Arc::new(PkgfsPackages::new(
                Arc::new(BasePackages::new_test_only(
                    // PkgfsPackages only uses the path-hash mapping, so tests do not need to
                    // populate the blob hashes.
                    HashSet::new(),
                    base_packages,
                )),
                blobfs::Client::new_mock().0,
            ))
        }

        fn proxy(self: &Arc<Self>, flags: fio::OpenFlags) -> fio::DirectoryProxy {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

            vfs::directory::entry::DirectoryEntry::open(
                Arc::clone(self),
                ExecutionScope::new(),
                flags,
                Path::dot(),
                server_end.into_channel().into(),
            );

            proxy
        }
    }

    macro_rules! package_name_hashmap {
        ($($inner:tt)*) => {
            convert_args!(
                keys = |s| PackageName::from_str(s).unwrap(),
                hashmap!($($inner)*)
            )
        };
    }

    macro_rules! package_variant_hashmap {
        ($($inner:tt)*) => {
            convert_args!(
                keys = |s| PackageVariant::from_str(s).unwrap(),
                hashmap!($($inner)*)
            )
        };
    }

    fn hash(n: u8) -> Hash {
        Hash::from([n; 32])
    }

    fn path(name: &str, variant: &str) -> PackagePath {
        PackagePath::from_name_and_variant(name.parse().unwrap(), variant.parse().unwrap())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn minimal_lifecycle() {
        let pkgfs_packages = PkgfsPackages::new_test([]);

        drop(pkgfs_packages);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn packages_listing() {
        let pkgfs_packages = PkgfsPackages::new_test([
            (path("static", "0"), hash(0)),
            (path("static", "1"), hash(2)),
        ]);

        assert_eq!(
            pkgfs_packages.packages().await,
            package_name_hashmap!(
                "static" => package_variant_hashmap!{
                    "0" => hash(0),
                    "1" => hash(2),
                },
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn readdir_empty() {
        let pkgfs_packages = PkgfsPackages::new_test([]);

        // Given adequate buffer space, the only entry is itself (".").
        let (pos, sealed) = Directory::read_dirents(
            &*pkgfs_packages,
            &TraversalPosition::Start,
            Box::new(FakeSink::new(100)),
        )
        .await
        .expect("read_dirents failed");

        assert_eq!(
            FakeSink::from_sealed(sealed).entries,
            vec![(".".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn readdir_nonempty() {
        let pkgfs_packages = PkgfsPackages::new_test([
            (path("allowed", "0"), hash(0)),
            (path("static", "0"), hash(1)),
            (path("static", "1"), hash(2)),
        ]);

        let (pos, sealed) = Directory::read_dirents(
            &*pkgfs_packages,
            &TraversalPosition::Start,
            Box::new(FakeSink::new(100)),
        )
        .await
        .expect("read_dirents failed");

        assert_eq!(
            FakeSink::from_sealed(sealed).entries,
            vec![
                (".".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                (
                    "allowed".to_owned(),
                    EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
                ),
                ("static".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
            ]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_rejects_invalid_name() {
        let pkgfs_packages = PkgfsPackages::new_test([]);

        let proxy = pkgfs_packages.proxy(fio::OpenFlags::RIGHT_READABLE);

        assert_matches!(
            fuchsia_fs::directory::open_directory(
                &proxy,
                "invalidname-!@#$%^&*()+=",
                fio::OpenFlags::RIGHT_READABLE
            )
            .await,
            Err(fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND))
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_rejects_missing_package() {
        let pkgfs_packages = PkgfsPackages::new_test([]);

        let proxy = pkgfs_packages.proxy(fio::OpenFlags::RIGHT_READABLE);

        assert_matches!(
            fuchsia_fs::directory::open_directory(
                &proxy,
                "missing",
                fio::OpenFlags::RIGHT_READABLE
            )
            .await,
            Err(fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND))
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_opens_static_package_variants() {
        let pkgfs_packages = PkgfsPackages::new_test([(path("static", "0"), hash(0))]);

        let proxy = pkgfs_packages.proxy(fio::OpenFlags::RIGHT_READABLE);

        assert_matches!(
            fuchsia_fs::directory::open_directory(&proxy, "static", fio::OpenFlags::RIGHT_READABLE)
                .await,
            Ok(_)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_opens_path_within_known_package_variant() {
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        let package = PackageBuilder::new("static")
            .add_resource_at("meta/message", &b"yes"[..])
            .build()
            .await
            .expect("created pkg");
        let (metafar_blob, _) = package.contents();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
        let pkgfs_packages = Arc::new(PkgfsPackages::new(
            Arc::new(BasePackages::new_test_only(
                HashSet::new(),
                [("static/0".parse().unwrap(), *package.meta_far_merkle_root())],
            )),
            blobfs_client,
        ));
        let proxy = pkgfs_packages.proxy(fio::OpenFlags::RIGHT_READABLE);

        let file = fuchsia_fs::directory::open_file(
            &proxy,
            "static/0/meta/message",
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await
        .unwrap();

        let message = fuchsia_fs::file::read_to_string(&file).await.unwrap();
        assert_eq!(message, "yes");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_unsets_posix_writable() {
        let pkgfs_packages = PkgfsPackages::new_test([]);

        let proxy =
            pkgfs_packages.proxy(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE);

        let (status, flags) = proxy.get_flags().await.unwrap();
        let () = zx::Status::ok(status).unwrap();
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE);
    }
}
