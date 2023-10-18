// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    fuchsia_inspect as finspect,
    fuchsia_merkle::Hash,
    futures::{future::BoxFuture, FutureExt as _, StreamExt as _, TryStreamExt as _},
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    },
};

/// A forest of packages and the blobs they require (including subpackages).
#[derive(Debug)]
pub struct FrozenIndex<Marker> {
    /// The meta.fars of the packages (including subpackages).
    packages: HashSet<Hash>,
    /// The meta.fars and content blobs of the packages (including subpackages).
    /// Equivalently, the contents of `packages` plus their corresponding content blobs.
    blobs: HashSet<Hash>,
    /// The package urls and hashes of the root packages (i.e. not including subpackages).
    root_package_urls_and_hashes: HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, Hash>,

    phantom: std::marker::PhantomData<Marker>,
}

/// Marker type for BasePackages.
#[derive(Debug)]
pub struct Base;
/// The system_image package, the packages in the static packages manifest, and the transitive
/// closure of their subpackages, or none if the system does not have a system_image package.
pub type BasePackages = FrozenIndex<Base>;

impl FrozenIndex<Base> {
    pub async fn new(
        blobfs: &blobfs::Client,
        system_image: &system_image::SystemImage,
    ) -> Result<Self, anyhow::Error> {
        let base_repo = fuchsia_url::RepositoryUrl::parse_host("fuchsia.com".into())
            .expect("valid repository hostname");
        let root_package_urls_and_hashes = system_image
            .static_packages()
            .await
            .context("failed to determine static packages")?
            .into_contents()
            .chain([(system_image::SystemImage::package_path(), *system_image.hash())])
            .map(|(path, hash)| {
                let (name, variant) = path.into_name_and_variant();
                // TODO(fxbug.dev/53911) Remove variant checks when variant concept is deleted.
                if !variant.is_zero() {
                    panic!("base package variants must be zero: {name} {variant}");
                }
                (fuchsia_url::UnpinnedAbsolutePackageUrl::new(base_repo.clone(), name, None), hash)
            })
            .collect::<HashMap<_, _>>();
        Self::from_urls(blobfs, root_package_urls_and_hashes).await
    }
}

impl<Marker: Send + Sync + 'static> FrozenIndex<Marker> {
    /// Create a `FrozenIndex` from a mapping of root package URLs to their hashes.
    /// Determines the content and subpackage blobs by reading the meta.fars from `blobfs`.
    async fn from_urls(
        blobfs: &blobfs::Client,
        root_package_urls_and_hashes: HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, Hash>,
    ) -> Result<Self, anyhow::Error> {
        let (packages, blobs) = Self::load_packages_and_blobs(
            blobfs,
            root_package_urls_and_hashes.iter().map(|(_, h)| *h),
        )
        .await
        .context("Error determining blobs")?;
        Ok(Self {
            packages,
            blobs,
            root_package_urls_and_hashes,
            phantom: std::marker::PhantomData,
        })
    }

    /// Takes `root_packages`, the hashes of the root packages.
    /// Returns the hashes of all packages including subpackages and the hashes of all blobs
    /// including subpackages.
    async fn load_packages_and_blobs(
        blobfs: &blobfs::Client,
        root_packages: impl Iterator<Item = Hash>,
    ) -> Result<(HashSet<Hash>, HashSet<Hash>), anyhow::Error> {
        let memoized_packages = async_lock::RwLock::new(HashMap::new());
        let mut futures = futures::stream::iter(
            root_packages.map(|p| Self::package_blobs(blobfs, p, &memoized_packages)),
        )
        .buffer_unordered(1000);

        let mut blobs = HashSet::new();
        while let Some(p) = futures.try_next().await? {
            blobs.extend(p);
        }
        drop(futures);

        Ok((memoized_packages.into_inner().into_keys().collect(), blobs))
    }

    // Returns all blobs of `package`: the meta.far, the content blobs, and the transitive
    // closure of subpackage blobs.
    async fn package_blobs(
        blobfs: &blobfs::Client,
        package: Hash,
        memoized_packages: &async_lock::RwLock<HashMap<Hash, HashSet<Hash>>>,
    ) -> Result<impl Iterator<Item = Hash>, anyhow::Error> {
        Ok(std::iter::once(package).chain(
            crate::required_blobs::find_required_blobs_recursive(
                blobfs,
                &package,
                memoized_packages,
                crate::required_blobs::ErrorStrategy::PropagateFailure,
            )
            .await
            .with_context(|| format!("determining required blobs for package {package}"))?,
        ))
    }

    /// Create an empty `FrozenIndex`, i.e. a `FrozenIndex` that does not have any packages (and
    /// therefore does not have any blobs). Useful for when there is no system_image package.
    pub fn empty() -> Self {
        Self {
            packages: HashSet::new(),
            blobs: HashSet::new(),
            root_package_urls_and_hashes: HashMap::new(),
            phantom: std::marker::PhantomData,
        }
    }

    /// The meta.fars and content blobs of the packages (including subpackages).
    pub fn list_blobs(&self) -> &HashSet<Hash> {
        &self.blobs
    }

    /// Returns `true` iff `pkg` is the hash of a package (including subpackages).
    pub fn is_package(&self, pkg: Hash) -> bool {
        self.packages.contains(&pkg)
    }

    /// Hashmap mapping the root (i.e not including subpackages) package urls to hashes.
    pub fn root_package_urls_and_hashes(
        &self,
    ) -> &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, Hash> {
        &self.root_package_urls_and_hashes
    }

    /// Returns a callback to be given to `finspect::Node::record_lazy_child`.
    pub fn record_lazy_inspect(
        self: &Arc<Self>,
    ) -> impl Fn() -> BoxFuture<'static, Result<finspect::Inspector, anyhow::Error>>
           + Send
           + Sync
           + 'static {
        let this = Arc::downgrade(self);
        move || {
            let this = this.clone();
            async move {
                let inspector = finspect::Inspector::default();
                if let Some(this) = this.upgrade() {
                    let root = inspector.root();
                    let () = this.root_package_urls_and_hashes.iter().for_each(|(path, hash)| {
                        // Packages are encoded as nodes instead of string properties because the
                        // privacy allowlist prefers to wildcard nodes instead of properties.
                        root.record_child(path.to_string(), |n| {
                            n.record_string("hash", hash.to_string())
                        })
                    });
                }
                Ok(inspector)
            }
            .boxed()
        }
    }

    /// Test-only constructor to allow testing with this type without constructing a blobfs.
    /// `packages` isn't populated, so `is_package` will always return false.
    #[cfg(test)]
    pub(crate) fn new_test_only(
        blobs: HashSet<Hash>,
        root_package_urls_and_hashes: impl IntoIterator<
            Item = (fuchsia_url::UnpinnedAbsolutePackageUrl, Hash),
        >,
    ) -> Self {
        Self {
            packages: HashSet::new(),
            blobs,
            root_package_urls_and_hashes: root_package_urls_and_hashes.into_iter().collect(),
            phantom: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, diagnostics_assertions::assert_data_tree, fuchsia_pkg_testing::PackageBuilder,
        std::iter::FromIterator as _,
    };

    struct TestEnv {
        _blobfs: blobfs_ramdisk::BlobfsRamdisk,
        system_image: fuchsia_pkg_testing::Package,
        inspector: finspect::types::Inspector,
    }

    impl TestEnv {
        async fn new_with_subpackages(
            static_packages: &[&fuchsia_pkg_testing::Package],
            subpackages: &[&fuchsia_pkg_testing::Package],
        ) -> (Self, FrozenIndex<Base>) {
            let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
            let blobfs_client = blobfs.client();

            for p in static_packages.iter().chain(subpackages) {
                p.write_to_blobfs(&blobfs).await;
            }

            let system_image = fuchsia_pkg_testing::SystemImageBuilder::new()
                .static_packages(static_packages)
                .build()
                .await;
            system_image.write_to_blobfs(&blobfs).await;

            let inspector = finspect::Inspector::default();

            let base_packages = FrozenIndex::new(
                &blobfs_client,
                &system_image::SystemImage::from_root_dir(
                    package_directory::RootDir::new(blobfs_client.clone(), *system_image.hash())
                        .await
                        .unwrap(),
                ),
            )
            .await
            .unwrap();

            (Self { _blobfs: blobfs, system_image, inspector }, base_packages)
        }

        async fn new(static_packages: &[&fuchsia_pkg_testing::Package]) -> (Self, BasePackages) {
            Self::new_with_subpackages(static_packages, &[]).await
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn identifies_all_blobs() {
        let base_subpackage = PackageBuilder::new("base-subpackage")
            .add_resource_at("base-subpackage-blob", &b"base-subpackage-blob-contents"[..])
            .build()
            .await
            .unwrap();
        let a_base_package = PackageBuilder::new("a-base-package")
            .add_resource_at("a-base-blob", &b"a-base-blob-contents"[..])
            .add_subpackage("my-subpackage", &base_subpackage)
            .build()
            .await
            .unwrap();
        let (env, base_packages) =
            TestEnv::new_with_subpackages(&[&a_base_package], &[&base_subpackage]).await;

        let expected_blobs = env
            .system_image
            .list_blobs()
            .unwrap()
            .into_iter()
            .chain(a_base_package.list_blobs().unwrap())
            .chain(base_subpackage.list_blobs().unwrap())
            .collect();
        assert_eq!(base_packages.list_blobs(), &expected_blobs);
        // Six expected blobs:
        //   system_image meta.far
        //   system_image content blob "data/static_packages"
        //   base-subpackage meta.far
        //   base-subpackage content blob "base-subpackage-blob"
        //   a-base-package meta.far
        //   a-base-package content blob "a-base-blob"
        assert_eq!(base_packages.list_blobs().len(), 6);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn correct_blob_count_shared_blob() {
        let a_base_package0 = PackageBuilder::new("a-base-package0")
            .add_resource_at("a-base-blob0", &b"duplicate-blob-contents"[..])
            .build()
            .await
            .unwrap();
        let a_base_package1 = PackageBuilder::new("a-base-package1")
            .add_resource_at("a-base-blob1", &b"duplicate-blob-contents"[..])
            .build()
            .await
            .unwrap();
        let (_env, base_packages) = TestEnv::new(&[&a_base_package0, &a_base_package1]).await;

        // Expect 5 blobs:
        //   * system_image meta.far
        //   * system_image data/static_packages
        //   * a-base-package0 meta.far
        //   * a-base-package0 a-base-blob0
        //   * a-base-package1 meta.far -> differs with a-base-package0 meta.far because
        //       meta/package and meta/contents differ
        //   * a-base-package1 a-base-blob1 -> duplicate of a-base-package0 a-base-blob0
        assert_eq!(base_packages.list_blobs().len(), 5);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn inspect_base_packages() {
        let base_subpackage = PackageBuilder::new("base-subpackage").build().await.unwrap();
        let a_base_package = PackageBuilder::new("a-base-package")
            .add_subpackage("my-subpackage", &base_subpackage)
            .build()
            .await
            .unwrap();
        let (env, base_packages) =
            TestEnv::new_with_subpackages(&[&a_base_package], &[&base_subpackage]).await;
        let base_packages = Arc::new(base_packages);

        env.inspector
            .root()
            .record_lazy_child("base-packages", base_packages.record_lazy_inspect());

        // Note base-subpackage is not present.
        assert_data_tree!(env.inspector, root: {
            "base-packages": {
                "fuchsia-pkg://fuchsia.com/a-base-package": {
                    "hash": a_base_package.hash().to_string(),
                },
                "fuchsia-pkg://fuchsia.com/system_image": {
                    "hash": env.system_image.hash().to_string(),
                }
            }
        });
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn paths_and_hashes_includes_system_image() {
        let a_base_package = PackageBuilder::new("a-base-package")
            .add_resource_at("a-base-blob", &b"a-base-blob-contents"[..])
            .build()
            .await
            .unwrap();
        let a_base_package_hash = *a_base_package.hash();
        let (env, base_packages) = TestEnv::new(&[&a_base_package]).await;

        assert_eq!(
            base_packages
                .root_package_urls_and_hashes()
                .iter()
                .map(|(p, h)| (p.clone(), *h))
                .collect::<HashSet<_>>(),
            HashSet::from_iter([
                (
                    "fuchsia-pkg://fuchsia.com/system_image".parse().unwrap(),
                    *env.system_image.hash()
                ),
                ("fuchsia-pkg://fuchsia.com/a-base-package".parse().unwrap(), a_base_package_hash),
            ])
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn paths_and_hashes_includes_system_image_even_if_no_static_packages() {
        let (env, base_packages) = TestEnv::new(&[]).await;

        assert_eq!(
            base_packages
                .root_package_urls_and_hashes()
                .iter()
                .map(|(p, h)| (p.clone(), *h))
                .collect::<HashSet<_>>(),
            HashSet::from_iter([(
                "fuchsia-pkg://fuchsia.com/system_image".parse().unwrap(),
                *env.system_image.hash()
            ),])
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn is_base_package_root_package() {
        let (env, base_packages) = TestEnv::new(&[]).await;
        let system_image = *env.system_image.hash();
        let mut not_system_image = Into::<[u8; 32]>::into(system_image);
        not_system_image[0] = !not_system_image[0];
        let not_system_image = Hash::from(not_system_image);

        assert!(base_packages.is_package(system_image));
        assert!(!base_packages.is_package(not_system_image));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn is_base_package_subpackage() {
        let subpackage = PackageBuilder::new("base-subpackage").build().await.unwrap();
        let superpackage = PackageBuilder::new("base-superpackage")
            .add_subpackage("my-subpackage", &subpackage)
            .build()
            .await
            .unwrap();
        let (_env, base_packages) =
            TestEnv::new_with_subpackages(&[&superpackage], &[&subpackage]).await;

        assert!(base_packages.is_package(*subpackage.hash()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn base_packages_fails_when_loading_fails() {
        let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
        let blobfs_client = blobfs.client();
        // system_image package has no data/static_packages file
        let system_image = PackageBuilder::new("system_image").build().await.unwrap();
        system_image.write_to_blobfs(&blobfs).await;

        let inspector = finspect::Inspector::default();

        let base_packages_res = FrozenIndex::new(
            &blobfs_client,
            &system_image::SystemImage::from_root_dir(
                package_directory::RootDir::new(blobfs_client.clone(), *system_image.hash())
                    .await
                    .unwrap(),
            ),
        )
        .await;

        assert!(base_packages_res.is_err());
        assert_data_tree!(inspector, root: {});
    }
}
