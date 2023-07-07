// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    fuchsia_inspect as finspect,
    fuchsia_merkle::Hash,
    fuchsia_pkg::PackagePath,
    futures::{future::BoxFuture, FutureExt as _, StreamExt as _, TryStreamExt as _},
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    },
};

/// The system_image package, the packages in the static packages manifest, and the transitive
/// closure of their subpackages, or none if the system does not have a system_image package.
#[derive(Debug)]
pub struct BasePackages {
    /// The meta.fars of the base packages (including subpackages).
    base_packages: HashSet<Hash>,
    /// The meta.fars and content blobs of the base packages (including subpackages).
    /// Equivalently, the contents of `base_packages` plus the content blobs.
    base_blobs: HashSet<Hash>,
    /// The paths and hashes of the root base packages (i.e. not including subpackages).
    root_paths_and_hashes: Vec<(PackagePath, Hash)>,
}

impl BasePackages {
    pub async fn new(
        blobfs: &blobfs::Client,
        system_image: &system_image::SystemImage,
    ) -> Result<Self, anyhow::Error> {
        let root_paths_and_hashes = system_image
            .static_packages()
            .await
            .context("failed to load static packages from system image")?
            .into_contents()
            .chain(std::iter::once((
                system_image::SystemImage::package_path(),
                *system_image.hash(),
            )))
            .collect::<Vec<_>>();

        let (base_packages, base_blobs) =
            Self::load_base_blobs(blobfs, root_paths_and_hashes.iter().map(|(_, h)| *h))
                .await
                .context("Error determining base blobs")?;
        Ok(Self { base_packages, base_blobs, root_paths_and_hashes })
    }

    /// Returns the base packages and base blobs (including the transitive closure of subpackages).
    async fn load_base_blobs(
        blobfs: &blobfs::Client,
        root_base_packages: impl Iterator<Item = Hash>,
    ) -> Result<(HashSet<Hash>, HashSet<Hash>), anyhow::Error> {
        let memoized_packages = async_lock::RwLock::new(HashMap::new());
        let mut futures = futures::stream::iter(
            root_base_packages.map(|p| Self::package_blobs(blobfs, p, &memoized_packages)),
        )
        .buffer_unordered(1000);

        let mut base_blobs = HashSet::new();
        while let Some(p) = futures.try_next().await? {
            base_blobs.extend(p);
        }
        drop(futures);

        Ok((memoized_packages.into_inner().into_keys().collect(), base_blobs))
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
            .with_context(|| format!("determining required blobs for base package {package}"))?,
        ))
    }

    /// Create an empty `BasePackages`, i.e. a `BasePackages` that does not have any packages (and
    /// therefore does not have any blobs). Useful for when there is no system_image package.
    pub fn empty() -> Self {
        Self {
            base_packages: HashSet::new(),
            base_blobs: HashSet::new(),
            root_paths_and_hashes: Vec::new(),
        }
    }

    /// The meta.fars and content blobs of the base packages (including subpackages).
    pub fn list_blobs(&self) -> &HashSet<Hash> {
        &self.base_blobs
    }

    /// Returns `true` iff `pkg` is the hash of a base package (including subpackages).
    pub fn is_base_package(&self, pkg: Hash) -> bool {
        self.base_packages.contains(&pkg)
    }

    /// Iterator over the root (i.e not including subpackages) base package paths and hashes.
    pub fn root_paths_and_hashes(&self) -> impl ExactSizeIterator<Item = &(PackagePath, Hash)> {
        self.root_paths_and_hashes.iter()
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
                    let () = this.root_paths_and_hashes.iter().for_each(|(path, hash)| {
                        root.record_string(path.to_string(), hash.to_string())
                    });
                }
                Ok(inspector)
            }
            .boxed()
        }
    }

    /// Test-only constructor to allow testing with this type without constructing a blobfs.
    /// base_packages isn't populated, so is_base_package will always return false.
    #[cfg(test)]
    pub(crate) fn new_test_only(
        base_blobs: HashSet<Hash>,
        root_paths_and_hashes: impl IntoIterator<Item = (PackagePath, Hash)>,
    ) -> Self {
        Self {
            base_packages: HashSet::new(),
            base_blobs,
            root_paths_and_hashes: root_paths_and_hashes.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, fuchsia_inspect::assert_data_tree, fuchsia_pkg_testing::PackageBuilder,
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
        ) -> (Self, BasePackages) {
            let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
            let blobfs_client = blobfs.client();

            let blobfs_dir = blobfs.root_dir().unwrap();
            for p in static_packages.iter().chain(subpackages) {
                p.write_to_blobfs_dir(&blobfs_dir);
            }

            let system_image = fuchsia_pkg_testing::SystemImageBuilder::new()
                .static_packages(static_packages)
                .build()
                .await;
            system_image.write_to_blobfs_dir(&blobfs_dir);

            let inspector = finspect::Inspector::default();

            let base_packages = BasePackages::new(
                &blobfs_client,
                &system_image::SystemImage::from_root_dir(
                    package_directory::RootDir::new(
                        blobfs_client.clone(),
                        *system_image.meta_far_merkle_root(),
                    )
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
                "a-base-package/0": a_base_package.meta_far_merkle_root().to_string(),
                "system_image/0": env.system_image.meta_far_merkle_root().to_string(),
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
        let a_base_package_hash = *a_base_package.meta_far_merkle_root();
        let (env, base_packages) = TestEnv::new(&[&a_base_package]).await;

        assert_eq!(
            base_packages
                .root_paths_and_hashes()
                .map(|(p, h)| (p.clone(), *h))
                .collect::<HashSet<_>>(),
            HashSet::from_iter([
                ("system_image/0".parse().unwrap(), *env.system_image.meta_far_merkle_root()),
                ("a-base-package/0".parse().unwrap(), a_base_package_hash),
            ])
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn paths_and_hashes_includes_system_image_even_if_no_static_packages() {
        let (env, base_packages) = TestEnv::new(&[]).await;

        assert_eq!(
            base_packages
                .root_paths_and_hashes()
                .map(|(p, h)| (p.clone(), *h))
                .collect::<HashSet<_>>(),
            HashSet::from_iter([(
                "system_image/0".parse().unwrap(),
                *env.system_image.meta_far_merkle_root()
            ),])
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn is_base_package_root_package() {
        let (env, base_packages) = TestEnv::new(&[]).await;
        let system_image = *env.system_image.meta_far_merkle_root();
        let mut not_system_image = Into::<[u8; 32]>::into(system_image);
        not_system_image[0] = !not_system_image[0];
        let not_system_image = Hash::from(not_system_image);

        assert!(base_packages.is_base_package(system_image));
        assert!(!base_packages.is_base_package(not_system_image));
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

        assert!(base_packages.is_base_package(*subpackage.meta_far_merkle_root()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn base_packages_fails_when_loading_fails() {
        let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
        let blobfs_client = blobfs.client();
        let blobfs_dir = blobfs.root_dir().unwrap();
        // system_image package has no data/static_packages file
        let system_image = PackageBuilder::new("system_image").build().await.unwrap();
        system_image.write_to_blobfs_dir(&blobfs_dir);

        let inspector = finspect::Inspector::default();

        let base_packages_res = BasePackages::new(
            &blobfs_client,
            &system_image::SystemImage::from_root_dir(
                package_directory::RootDir::new(
                    blobfs_client.clone(),
                    *system_image.meta_far_merkle_root(),
                )
                .await
                .unwrap(),
            ),
        )
        .await;

        assert!(base_packages_res.is_err());
        assert_data_tree!(inspector, root: {});
    }
}
