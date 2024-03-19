// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::anyhow,
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_hash::Hash,
    fuchsia_pkg::PackageVariant,
    fuchsia_zircon as zx,
    std::{
        collections::{BTreeSet, HashMap},
        sync::Arc,
    },
    tracing::error,
    vfs::{
        directory::{
            dirents_sink,
            entry::EntryInfo,
            entry_container::{Directory, DirectoryWatcher},
            immutable::connection::ImmutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        immutable_attributes,
        path::Path,
        ToObjectRequest,
    },
};

#[cfg(feature = "supports_open2")]
use vfs::{ObjectRequestRef, ProtocolsExt as _};

#[derive(Debug)]
pub struct PkgfsPackagesVariants {
    contents: HashMap<PackageVariant, Hash>,
    blobfs: blobfs::Client,
}

impl PkgfsPackagesVariants {
    pub(super) fn new(
        contents: HashMap<PackageVariant, Hash>,
        blobfs: blobfs::Client,
    ) -> Arc<Self> {
        Arc::new(Self { contents, blobfs })
    }

    fn variants(&self) -> BTreeSet<PackageVariant> {
        self.contents.keys().cloned().collect()
    }

    fn variant(&self, name: &str) -> Option<Hash> {
        self.contents.get(&name.parse().ok()?).cloned()
    }
}

impl vfs::node::IsDirectory for PkgfsPackagesVariants {}

#[async_trait]
impl vfs::node::Node for PkgfsPackagesVariants {
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

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE,
                content_size: 0,
                storage_size: 0,
                link_count: 1,
                id: 1,
            }
        ))
    }
}

#[async_trait]
impl Directory for PkgfsPackagesVariants {
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

            match path.next().map(|variant| self.variant(variant)) {
                None => {
                    object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
                }
                Some(Some(hash)) => {
                    let blobfs = self.blobfs.clone();
                    let server_end = object_request.take().into_server_end();
                    scope.clone().spawn(async move {
                        if let Err(e) = package_directory::serve_path(
                            scope, blobfs, hash, flags, path, server_end,
                        )
                        .await
                        {
                            error!(
                                "Failed to open package directory for {}: {:#}",
                                hash,
                                anyhow!(e)
                            );
                        }
                    });
                    Ok(())
                }
                Some(None) => Err(zx::Status::NOT_FOUND),
            }
        });
    }

    #[cfg(feature = "supports_open2")]
    fn open2(
        self: Arc<Self>,
        scope: ExecutionScope,
        mut path: Path,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if protocols.creation_mode() != vfs::CreationMode::Never {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        // This directory and all child nodes are read-only
        if let Some(rights) = protocols.rights() {
            if rights.intersects(fio::Operations::WRITE_BYTES) {
                return Err(zx::Status::NOT_SUPPORTED);
            }
        }
        // If file options was specified, `APPEND` or `TRUNCATE` modes are not support
        if protocols.is_append() || object_request.truncate {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        match path.next().map(|variant| self.variant(variant)) {
            None => {
                object_request.spawn_connection(scope, self, protocols, ImmutableConnection::create)
            }
            Some(Some(hash)) => {
                let blobfs = self.blobfs.clone();
                let object_request = object_request.take();
                scope.clone().spawn(async move {
                    match package_directory::RootDir::new(blobfs, hash)
                        .await
                        .map_err(|e| (&e).into())
                    {
                        Ok(root_dir) => {
                            object_request.handle(|object_request| {
                                root_dir.open2(scope, path, protocols, object_request)
                            });
                        }
                        Err(e) => object_request.shutdown(e),
                    };
                });
                Ok(())
            }
            Some(None) => Err(zx::Status::NOT_FOUND),
        }
    }

    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        mut sink: Box<(dyn dirents_sink::Sink + 'static)>,
    ) -> Result<(TraversalPosition, Box<(dyn dirents_sink::Sealed + 'static)>), zx::Status> {
        use dirents_sink::AppendResult;

        let entries = self.variants();

        let remaining = match pos {
            TraversalPosition::Start => {
                // Yield "." first. If even that can't fit in the response, return the same
                // traversal position so we try again next time (where the client hopefully
                // provides a bigger buffer).
                match sink
                    .append(&EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory), ".")
                {
                    AppendResult::Ok(new_sink) => sink = new_sink,
                    AppendResult::Sealed(sealed) => return Ok((TraversalPosition::Start, sealed)),
                }

                entries.range::<PackageVariant, _>(..)
            }
            TraversalPosition::Name(next) => {
                // This function only returns valid package variants, so it will always be provided
                // a valid package variant.
                let next: PackageVariant = next.parse().unwrap();

                // `next` is the name of the next item that still needs to be provided, so start
                // there.
                entries.range(next..)
            }
            TraversalPosition::Index(_) => {
                // This directory uses names for iteration because I copy/pasted this from super.
                // Since this directory is immutable, index-based enumeration would be more
                // efficient.
                unreachable!()
            }
            TraversalPosition::End => return Ok((TraversalPosition::End, sink.seal())),
        };

        for variant in remaining {
            let variant = variant.to_string();
            match sink
                .append(&EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory), &variant)
            {
                AppendResult::Ok(new_sink) => sink = new_sink,
                AppendResult::Sealed(sealed) => {
                    // Ran out of response buffer space. Pick up on this item next time.
                    return Ok((TraversalPosition::Name(variant), sealed));
                }
            }
        }

        Ok((TraversalPosition::End, sink.seal()))
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
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::compat::pkgfs::testing::FakeSink,
        assert_matches::assert_matches,
        fuchsia_pkg_testing::{blobfs::Fake as FakeBlobfs, PackageBuilder},
        maplit::{btreeset, convert_args, hashmap},
        std::str::FromStr,
    };

    impl PkgfsPackagesVariants {
        pub fn new_test(contents: HashMap<PackageVariant, Hash>) -> Arc<Self> {
            let (blobfs, _) = blobfs::Client::new_mock();

            PkgfsPackagesVariants::new(contents, blobfs)
        }

        fn proxy(self: &Arc<Self>, flags: fio::OpenFlags) -> fio::DirectoryProxy {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

            Arc::clone(self).open(
                ExecutionScope::new(),
                flags,
                Path::dot(),
                server_end.into_channel().into(),
            );

            proxy
        }

        // Returns directory proxy from Open2
        #[cfg(feature = "supports_open2")]
        fn proxy2(self: &Arc<Self>, protocols: fio::ConnectionProtocols) -> fio::DirectoryProxy {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

            protocols.to_object_request(server_end).handle(|req| {
                Arc::clone(self).open2(ExecutionScope::new(), Path::dot(), protocols, req)
            });

            proxy
        }
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

    #[fuchsia_async::run_singlethreaded(test)]
    async fn minimal_lifecycle() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(hashmap! {});

        drop(pkgfs_packages_variants);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn variants_listing_is_contents_keys() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {
            "0" => hash(0x0000),
            "zero" => hash(0),
            "nil" => hash(0),
            "not-0" => hash(42),
        });

        assert_eq!(
            pkgfs_packages_variants.variants(),
            convert_args!(
                keys = |s| PackageVariant::from_str(s).unwrap(),
                btreeset!("0", "zero", "nil", "not-0")
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn variant_getter_is_contents_lookup() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {
            "0" => hash(0),
            "1" => hash(1),
            "answer" => hash(42),
        });

        assert_eq!(pkgfs_packages_variants.variant("0"), Some(hash(0)));
        assert_eq!(pkgfs_packages_variants.variant("1"), Some(hash(1)));
        assert_eq!(pkgfs_packages_variants.variant("answer"), Some(hash(42)));

        assert_eq!(pkgfs_packages_variants.variant("unknown"), None);
        assert_eq!(pkgfs_packages_variants.variant("invalid-chars:!@#$%^&*()"), None);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn readdir_empty() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {});

        // An empty readdir buffer can't hold any elements, so it returns nothing and indicates we
        // are still at the start.
        let (pos, sealed) = Directory::read_dirents(
            &*pkgfs_packages_variants,
            &TraversalPosition::Start,
            Box::new(FakeSink::new(0)),
        )
        .await
        .expect("read_dirents failed");

        assert_eq!(FakeSink::from_sealed(sealed).entries, vec![]);
        assert_eq!(pos, TraversalPosition::Start);

        // Given adequate buffer space, the only entry is itself (".").
        let (pos, sealed) = Directory::read_dirents(
            &*pkgfs_packages_variants,
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
    async fn readdir_enumerates_all_variants() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {
            "0" => hash(0),
            "1" => hash(1),
            "two" => hash(2),
        });

        let (pos, sealed) = Directory::read_dirents(
            &*pkgfs_packages_variants,
            &TraversalPosition::Start,
            Box::new(FakeSink::new(100)),
        )
        .await
        .expect("read_dirents failed");

        assert_eq!(
            FakeSink::from_sealed(sealed).entries,
            vec![
                (".".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("0".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("1".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("two".to_owned(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
            ]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn readdir_one_entry_at_a_time_yields_expected_entries() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {
            "0" => hash(0),
            "1" => hash(1),
            "two" => hash(2),
        });

        let expected_entries = vec![
            (
                ".".to_owned(),
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory),
                TraversalPosition::Name("0".to_owned()),
            ),
            (
                "0".to_owned(),
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory),
                TraversalPosition::Name("1".to_owned()),
            ),
            (
                "1".to_owned(),
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory),
                TraversalPosition::Name("two".to_owned()),
            ),
            (
                "two".to_owned(),
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory),
                TraversalPosition::End,
            ),
        ];

        let mut pos = TraversalPosition::Start;

        for (name, info, expected_pos) in expected_entries {
            let (newpos, sealed) = Directory::read_dirents(
                &*pkgfs_packages_variants,
                &pos,
                Box::new(FakeSink::new(1)),
            )
            .await
            .expect("read_dirents failed");

            assert_eq!(FakeSink::from_sealed(sealed).entries, vec![(name, info)]);
            assert_eq!(newpos, expected_pos);

            pos = newpos;
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_rejects_invalid_name() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {});

        let proxy = pkgfs_packages_variants.proxy(fio::OpenFlags::RIGHT_READABLE);

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
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {});

        let proxy = pkgfs_packages_variants.proxy(fio::OpenFlags::RIGHT_READABLE);

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
    async fn open_unsets_posix_writable() {
        let package = PackageBuilder::new("just-meta-far").build().await.expect("created pkg");
        let (metafar_blob, _) = package.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let pkgfs_packages_variants = PkgfsPackagesVariants::new(
            package_variant_hashmap! {
                "0" => metafar_blob.merkle,
            },
            blobfs_client,
        );

        let proxy = pkgfs_packages_variants
            .proxy(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE);

        let (status, flags) = proxy.get_flags().await.unwrap();
        let () = zx::Status::ok(status).unwrap();
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_opens_known_package_variant() {
        let package = PackageBuilder::new("just-meta-far").build().await.expect("created pkg");
        let (metafar_blob, _) = package.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let pkgfs_packages_variants = PkgfsPackagesVariants::new(
            package_variant_hashmap! {
                "0" => metafar_blob.merkle,
            },
            blobfs_client,
        );

        let proxy = pkgfs_packages_variants.proxy(fio::OpenFlags::RIGHT_READABLE);

        let dir =
            fuchsia_fs::directory::open_directory(&proxy, "0", fio::OpenFlags::RIGHT_READABLE)
                .await
                .unwrap();
        let () = package.verify_contents(&dir).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_opens_path_within_known_package_variant() {
        let package = PackageBuilder::new("just-meta-far")
            .add_resource_at("meta/message", &b"Hello World!"[..])
            .build()
            .await
            .expect("created pkg");
        let (metafar_blob, _) = package.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let pkgfs_packages_variants = PkgfsPackagesVariants::new(
            package_variant_hashmap! {
                "0" => metafar_blob.merkle,
            },
            blobfs_client,
        );

        let proxy = pkgfs_packages_variants.proxy(fio::OpenFlags::RIGHT_READABLE);

        let file = fuchsia_fs::directory::open_file(
            &proxy,
            "0/meta/message",
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await
        .unwrap();
        let message = fuchsia_fs::file::read_to_string(&file).await.unwrap();
        assert_eq!(message, "Hello World!");
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn open2_rejects_invalid_name() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {});

        let proxy =
            pkgfs_packages_variants.proxy2(fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            }));

        assert_matches!(
            fuchsia_fs::directory::open2_directory(
                &proxy,
                "invalidname-!@#$%^&*()+=",
                fio::ConnectionProtocols::Node(fio::NodeOptions {
                    rights: Some(fio::Operations::READ_BYTES),
                    ..Default::default()
                }),
            )
            .await,
            Err(fuchsia_fs::node::OpenError::OnOpenDecode(fidl::Error::ClientChannelClosed {
                status: zx::Status::NOT_FOUND,
                ..
            }))
        );
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn open2_rejects_missing_package() {
        let pkgfs_packages_variants = PkgfsPackagesVariants::new_test(package_variant_hashmap! {});

        let proxy =
            pkgfs_packages_variants.proxy2(fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            }));

        assert_matches!(
            fuchsia_fs::directory::open2_directory(
                &proxy,
                "missing",
                fio::ConnectionProtocols::Node(fio::NodeOptions {
                    rights: Some(fio::Operations::READ_BYTES),
                    ..Default::default()
                }),
            )
            .await,
            Err(fuchsia_fs::node::OpenError::OnOpenDecode(fidl::Error::ClientChannelClosed {
                status: zx::Status::NOT_FOUND,
                ..
            }))
        );
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn open2_opens_known_package_variant() {
        let package = PackageBuilder::new("just-meta-far").build().await.expect("created pkg");
        let (metafar_blob, _) = package.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let pkgfs_packages_variants = PkgfsPackagesVariants::new(
            package_variant_hashmap! {
                "0" => metafar_blob.merkle,
            },
            blobfs_client,
        );

        let proxy =
            pkgfs_packages_variants.proxy2(fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::R_STAR_DIR),
                ..Default::default()
            }));

        let dir = fuchsia_fs::directory::open2_directory(
            &proxy,
            "0",
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::R_STAR_DIR),
                ..Default::default()
            }),
        )
        .await
        .expect("open2 failed");

        // `verify_contents(..)` checks the contents of each file in `dir`, and requires the files
        // to be opened with `R_STAR_DIR` rights.
        let () = package.verify_contents(&dir).await.expect("verify_contents failed");
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn open2_opens_path_within_known_package_variant() {
        let package = PackageBuilder::new("just-meta-far")
            .add_resource_at("meta/message", &b"Hello World!"[..])
            .build()
            .await
            .expect("created pkg");
        let (metafar_blob, _) = package.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);

        let pkgfs_packages_variants = PkgfsPackagesVariants::new(
            package_variant_hashmap! {
                "0" => metafar_blob.merkle,
            },
            blobfs_client,
        );

        let proxy =
            pkgfs_packages_variants.proxy2(fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            }));

        let file = fuchsia_fs::directory::open2_file(
            &proxy,
            "0/meta/message",
            fio::FileProtocolFlags::default(),
            Some(fio::Operations::READ_BYTES),
        )
        .await
        .expect("open2 failed");
        let message = fuchsia_fs::file::read_to_string(&file).await.expect("read_to_string failed");
        assert_eq!(message, "Hello World!");
    }
}
