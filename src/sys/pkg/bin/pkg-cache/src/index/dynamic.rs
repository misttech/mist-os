// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::anyhow,
    fuchsia_inspect::{self as finspect},
    fuchsia_merkle::Hash,
    fuchsia_pkg::PackagePath,
    fuchsia_zircon as zx,
    futures::StreamExt as _,
    std::collections::{HashMap, HashSet},
    system_image::CachePackages,
    tracing::{error, warn},
};

/// Concurrency limit when checking which cache packages are resident on boot.
const CACHE_PACKAGE_LOADING_CONCURRENCY: usize = 100;
/// Warn if loading cache packages takes longer than this.
const CACHE_PACKAGE_LOADING_WARN_DURATION: zx::Duration = zx::Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct DynamicIndex {
    /// contains the active packages and packages still being cached that will become active when
    /// complete.
    packages: HashMap<Hash, Package>,
    /// map of package path to most recently activated package hash.
    active_packages: HashMap<PackagePath, Hash>,
    /// If dynamic protection is disabled, all write operations on the index become no-ops.
    /// Used while phasing out the dynamic index.
    protect_dynamic_packages: crate::DynamicProtection,
}

#[derive(thiserror::Error, Debug)]
#[error("the blob being fulfilled ({hash}) is not needed, dynamic index package state is {state}")]
pub struct FulfillNotNeededBlobError {
    pub hash: Hash,
    pub state: &'static str,
}

#[derive(thiserror::Error, Debug)]
pub enum CompleteInstallError {
    #[error("the package is in an unexpected state: {0:?}")]
    UnexpectedPackageState(Option<Package>),
}

#[derive(thiserror::Error, Debug)]
pub enum AddBlobsError {
    #[error("the package is not known to the dynamic index")]
    UnknownPackage,

    #[error("the package should be in state WithMetaFar but was in state {0}")]
    WrongState(&'static str),
}

impl DynamicIndex {
    pub fn from_config(protect_dynamic_packages: crate::DynamicProtection) -> Self {
        Self { packages: HashMap::new(), active_packages: HashMap::new(), protect_dynamic_packages }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::from_config(crate::DynamicProtection::Enabled)
    }

    #[cfg(test)]
    pub fn packages(&self) -> HashMap<Hash, Package> {
        self.packages.clone()
    }

    /// Returns a snapshot of all active packages and their hashes.
    #[cfg(test)]
    pub fn active_packages(&self) -> HashMap<PackagePath, Hash> {
        self.active_packages.clone()
    }

    /// True iff the package is active in the dynamic index.
    pub fn is_active(&self, hash: &Hash) -> bool {
        matches!(self.packages.get(hash), Some(Package::Active { .. }))
    }

    // Add the given package to the dynamic index.
    fn add_package(&mut self, hash: Hash, package: Package) {
        if self.is_disabled() {
            return;
        }
        match &package {
            Package::Pending => {}
            Package::WithMetaFar { .. } => {}
            Package::Active { path, .. } => {
                if let Some(previous_package) = self.active_packages.insert(path.clone(), hash) {
                    self.packages.remove(&previous_package);
                }
            }
        }
        self.packages.insert(hash, package);
    }

    /// Returns the content blobs associated with the given package hash, if known.
    pub fn lookup_content_blobs(&self, package_hash: &Hash) -> Option<&HashSet<Hash>> {
        self.packages.get(package_hash)?.content_blobs()
    }

    /// Returns all blobs protected by the dynamic index.
    pub fn all_blobs(&self) -> HashSet<Hash> {
        self.packages
            .iter()
            .flat_map(|(hash, package)| {
                let blobs = match &package {
                    Package::Pending => None,
                    Package::WithMetaFar { required_blobs, .. } => Some(required_blobs.iter()),
                    Package::Active { required_blobs, .. } => Some(required_blobs.iter()),
                };
                std::iter::once(hash).chain(blobs.into_iter().flatten())
            })
            .cloned()
            .collect()
    }

    /// Notifies dynamic index that the given package is going to be installed, to keep the meta
    /// far blob protected.
    pub fn start_install(&mut self, package_hash: Hash) {
        if self.is_disabled() {
            return;
        }
        self.packages.entry(package_hash).or_insert(Package::Pending);
    }

    /// Notifies dynamic index that the given package's meta far is now present in blobfs,
    /// providing the meta far's internal name. The content and subpackage blobs should be added by
    /// calls to `add_blobs`.
    pub fn fulfill_meta_far(
        &mut self,
        hash: Hash,
        path: PackagePath,
    ) -> Result<(), FulfillNotNeededBlobError> {
        if self.is_disabled() {
            return Ok(());
        }
        if let Some(wrong_state) = match &self.packages.get(&hash) {
            Some(Package::Pending) => None,
            None => Some("missing"),
            Some(Package::Active { .. }) => Some("Active"),
            Some(Package::WithMetaFar { .. }) => Some("WithMetaFar"),
        } {
            return Err(FulfillNotNeededBlobError { hash, state: wrong_state });
        }

        self.add_package(hash, Package::WithMetaFar { path, required_blobs: HashSet::new() });

        Ok(())
    }

    /// Associates additional blobs with a package that is in state `WithMetaFar`.
    pub fn add_blobs(
        &mut self,
        package_hash: Hash,
        additional_blobs: &HashSet<Hash>,
    ) -> Result<(), AddBlobsError> {
        if self.is_disabled() {
            return Ok(());
        }
        match self.packages.get_mut(&package_hash) {
            Some(Package::WithMetaFar { required_blobs, .. }) => {
                required_blobs.extend(additional_blobs);
                Ok(())
            }
            Some(package) => Err(AddBlobsError::WrongState(package.state_str())),
            None => Err(AddBlobsError::UnknownPackage),
        }
    }

    /// Notifies dynamic index that the given package has completed installation.
    pub fn complete_install(&mut self, package_hash: Hash) -> Result<(), CompleteInstallError> {
        if self.is_disabled() {
            return Ok(());
        }
        match self.packages.get_mut(&package_hash) {
            Some(package @ Package::WithMetaFar { .. }) => {
                let Package::WithMetaFar { path, required_blobs } =
                    std::mem::replace(package, Package::Pending)
                else {
                    unreachable!("outer match guarantees `package` is a WithMetaFar");
                };
                *package = Package::Active { path: path.clone(), required_blobs };
                if let Some(previous_package) = self.active_packages.insert(path, package_hash) {
                    self.packages.remove(&previous_package);
                }
                Ok(())
            }
            package => Err(CompleteInstallError::UnexpectedPackageState(package.cloned())),
        }
    }

    /// Notifies dynamic index that the given package installation has been canceled.
    pub fn cancel_install(&mut self, package_hash: &Hash) {
        if self.is_disabled() {
            return;
        }
        match self.packages.get(package_hash) {
            Some(Package::Pending) | Some(Package::WithMetaFar { .. }) => {
                self.packages.remove(package_hash);
            }
            Some(Package::Active { .. }) => {
                warn!("Unable to cancel install for active package {}", package_hash);
            }
            None => {
                error!("Unable to cancel install for unknown package {}", package_hash);
            }
        }
    }

    /// Records self to `node`.
    pub fn record_inspect(&self, node: &finspect::Node) {
        for (hash, state) in &self.packages {
            node.record_child(hash.to_string(), |n| {
                n.record_string("state", state.state_str());
                match state {
                    Package::Pending => (),
                    Package::WithMetaFar { path, required_blobs }
                    | Package::Active { path, required_blobs } => {
                        n.record_string("path", path.to_string());
                        n.record_uint("required_blobs", required_blobs.len() as u64);
                    }
                }
            })
        }
    }

    fn is_disabled(&self) -> bool {
        matches!(self.protect_dynamic_packages, crate::DynamicProtection::Disabled)
    }
}

/// For any package referenced by the cache packages manifest that has all of its blobs present in
/// blobfs, imports those packages into the provided dynamic index.
pub async fn load_cache_packages(
    index: &mut DynamicIndex,
    cache_packages: &CachePackages,
    blobfs: &blobfs::Client,
) {
    let start = zx::Time::get_monotonic();
    let () = load_cache_packages_impl(index, cache_packages, blobfs).await;
    let duration = zx::Time::get_monotonic() - start;
    if duration > CACHE_PACKAGE_LOADING_WARN_DURATION {
        warn!("loading cache packages is slow: {} ms", duration.into_millis())
    }
}

async fn load_cache_packages_impl(
    index: &mut DynamicIndex,
    cache_packages: &CachePackages,
    blobfs: &blobfs::Client,
) {
    // This function is called before anything writes to or deletes from blobfs, so if it needs
    // to be sped up, it might be possible to replace `filter_to_missing_blobs` with just a set
    // comparison to `all_known`.
    // This alternate approach requires that blobfs responds to `fuchsia.io/Directory.ReadDirents`
    // with *only* blobs that are readable, i.e. blobs for which the `USER_0` signal is set (which
    // is currently checked per-package per-blob by `blobfs.filter_to_missing_blobs()`). Would need
    // to confirm with the storage team that blobfs meets this requirement.
    // TODO(https://fxbug.dev/42172132): ensure non-fuchsia.com URLs are correctly handled in dynamic index.
    let memoized_packages = async_lock::RwLock::new(HashMap::new());
    let all_known = blobfs.list_known_blobs().await.ok();
    let all_known = all_known.as_ref();
    // A stream of resident cache packages and all their required blobs.
    let mut futures = futures::stream::iter(cache_packages.contents().map(|url| {
        let (blobfs, memoized_packages) = (&blobfs, &memoized_packages);
        async move {
            let hash = url.hash();
            // Find all required blobs (including subpackages).
            match crate::required_blobs::find_required_blobs_recursive(
                blobfs,
                &hash,
                &memoized_packages,
                crate::required_blobs::ErrorStrategy::PropagateFailure,
            )
            .await
            {
                Ok(blobs) => {
                    // If no required blobs are missing, yield this package for later activation.
                    if blobfs.filter_to_missing_blobs(&blobs, all_known).await.is_empty() {
                        let path = PackagePath::from_name_and_variant(
                            url.name().clone(),
                            url.variant().unwrap_or(&fuchsia_pkg::PackageVariant::zero()).clone(),
                        );
                        Some((hash, path, blobs))
                    } else {
                        None
                    }
                }
                Err(crate::required_blobs::FindRequiredBlobsError::CreateRootDir {
                    source: package_directory::Error::MissingMetaFar,
                    ..
                }) => None,
                Err(e) => {
                    error!(
                        %url, "load_cache_packages: finding required blobs: {:#}", anyhow!(e)
                    );
                    None
                }
            }
        }
    }))
    .buffer_unordered(CACHE_PACKAGE_LOADING_CONCURRENCY);

    while let Some(cache_package) = futures.next().await {
        if let Some((hash, path, blobs)) = cache_package {
            let () = index.start_install(hash);
            if let Err(e) = index.fulfill_meta_far(hash, path) {
                error!(
                    "load_cache_packages: fulfill_meta_far of {} failed: {:#}",
                    hash,
                    anyhow!(e)
                );
                let () = index.cancel_install(&hash);
                continue;
            }
            if let Err(e) = index.add_blobs(hash, &blobs) {
                error!("load_cache_packages: add_blobs of {} failed: {:#}", hash, anyhow!(e));
                let () = index.cancel_install(&hash);
                continue;
            }
            if let Err(e) = index.complete_install(hash) {
                error!(
                    "load_cache_packages: complete_install of {} failed: {:#}",
                    hash,
                    anyhow!(e)
                );
                let () = index.cancel_install(&hash);
                continue;
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Package {
    /// Only the package hash is known, meta.far isn't available yet.
    Pending,
    /// We have the meta.far
    WithMetaFar {
        /// The name and variant of the package.
        path: PackagePath,
        /// Set of blobs this package depends on, does not include meta.far blob itself.
        required_blobs: HashSet<Hash>,
    },
    /// All blobs are present and the package is activated.
    Active {
        /// The name and variant of the package.
        path: PackagePath,
        /// Set of blobs this package depends on, does not include meta.far blob itself.
        required_blobs: HashSet<Hash>,
    },
}

impl Package {
    fn content_blobs(&self) -> Option<&HashSet<Hash>> {
        match self {
            Package::Pending => None,
            Package::WithMetaFar { required_blobs, .. } => Some(required_blobs),
            Package::Active { required_blobs, .. } => Some(required_blobs),
        }
    }

    fn state_str(&self) -> &'static str {
        match self {
            Package::Pending => "Pending",
            Package::WithMetaFar { .. } => "WithMetaFar",
            Package::Active { .. } => "Active",
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fuchsia_async as fasync,
        fuchsia_pkg_testing::PackageBuilder,
        fuchsia_url::PinnedAbsolutePackageUrl,
        maplit::{hashmap, hashset},
    };

    #[test]
    fn test_is_active() {
        let mut dynamic_index = DynamicIndex::new();
        dynamic_index.add_package(Hash::from([1; 32]), Package::Pending);
        dynamic_index.add_package(
            Hash::from([2; 32]),
            Package::WithMetaFar {
                path: PackagePath::from_name_and_variant(
                    "fake-package".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
            },
        );
        dynamic_index.add_package(
            Hash::from([3; 32]),
            Package::Active {
                path: PackagePath::from_name_and_variant(
                    "fake-package-2".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([6; 32]) },
            },
        );

        assert!(!dynamic_index.is_active(&Hash::from([1; 32])));
        assert!(!dynamic_index.is_active(&Hash::from([2; 32])));
        assert!(dynamic_index.is_active(&Hash::from([3; 32])));
    }

    #[test]
    fn test_all_blobs() {
        let mut dynamic_index = DynamicIndex::new();
        assert_eq!(dynamic_index.all_blobs(), HashSet::new());

        dynamic_index.add_package(Hash::from([1; 32]), Package::Pending);
        dynamic_index.add_package(
            Hash::from([2; 32]),
            Package::WithMetaFar {
                path: PackagePath::from_name_and_variant(
                    "fake-package".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
            },
        );
        dynamic_index.add_package(
            Hash::from([5; 32]),
            Package::Active {
                path: PackagePath::from_name_and_variant(
                    "fake-package-2".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([6; 32]) },
            },
        );
        assert_eq!(
            dynamic_index.all_blobs(),
            hashset! {
                Hash::from([1; 32]),
                Hash::from([2; 32]),
                Hash::from([3; 32]),
                Hash::from([4; 32]),
                Hash::from([5; 32]),
                Hash::from([6; 32]),
            }
        );
    }

    #[test]
    fn lookup_content_blobs_handles_withmetafar_and_active_states() {
        let mut dynamic_index = DynamicIndex::new();

        dynamic_index.add_package(Hash::from([1; 32]), Package::Pending);
        dynamic_index.add_package(
            Hash::from([2; 32]),
            Package::WithMetaFar {
                path: PackagePath::from_name_and_variant(
                    "fake-package".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
            },
        );
        dynamic_index.add_package(
            Hash::from([5; 32]),
            Package::Active {
                path: PackagePath::from_name_and_variant(
                    "fake-package-2".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([6; 32]) },
            },
        );

        assert_eq!(dynamic_index.lookup_content_blobs(&Hash::from([0; 32])), None);
        assert_eq!(dynamic_index.lookup_content_blobs(&Hash::from([1; 32])), None);
        assert_eq!(
            dynamic_index.lookup_content_blobs(&Hash::from([2; 32])),
            Some(&hashset! { Hash::from([3; 32]), Hash::from([4; 32]) })
        );
        assert_eq!(
            dynamic_index.lookup_content_blobs(&Hash::from([5; 32])),
            Some(&hashset! { Hash::from([6; 32]) })
        );
    }

    #[test]
    fn test_complete_install() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let path = PackagePath::from_name_and_variant(
            "fake-package".parse().unwrap(),
            "0".parse().unwrap(),
        );
        let required_blobs =
            hashset! { Hash::from([3; 32]), Hash::from([4; 32]), Hash::from([5; 32]) };
        let previous_hash = Hash::from([6; 32]);
        dynamic_index.add_package(
            previous_hash,
            Package::Active {
                path: path.clone(),
                required_blobs: hashset! { Hash::from([7; 32]) },
            },
        );
        dynamic_index.add_package(
            hash,
            Package::WithMetaFar { path: path.clone(), required_blobs: required_blobs.clone() },
        );

        let () = dynamic_index.complete_install(hash).unwrap();
        assert_eq!(
            dynamic_index.packages(),
            hashmap! {
                hash => Package::Active {
                    path: path.clone(),
                    required_blobs,
                },
            }
        );
        assert_eq!(dynamic_index.active_packages(), hashmap! { path => hash });
    }

    #[test]
    fn complete_install_unknown_package() {
        let mut dynamic_index = DynamicIndex::new();

        assert_matches!(
            dynamic_index.complete_install(Hash::from([2; 32])),
            Err(CompleteInstallError::UnexpectedPackageState(None))
        );
    }

    #[test]
    fn complete_install_pending_package() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        dynamic_index.add_package(hash, Package::Pending);
        assert_matches!(
            dynamic_index.complete_install(hash),
            Err(CompleteInstallError::UnexpectedPackageState(Some(Package::Pending)))
        );
    }

    #[test]
    fn complete_install_active_package() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let package = Package::Active {
            path: PackagePath::from_name_and_variant(
                "fake-package".parse().unwrap(),
                "0".parse().unwrap(),
            ),
            required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
        };
        dynamic_index.add_package(hash, package.clone());
        assert_matches!(
            dynamic_index.complete_install(hash),
            Err(CompleteInstallError::UnexpectedPackageState(Some(p))) if p == package
        );
    }

    #[test]
    fn start_install() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        dynamic_index.start_install(hash);

        assert_eq!(dynamic_index.packages(), hashmap! { hash => Package::Pending {  } });
    }

    #[test]
    fn start_install_do_not_overwrite() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let package = Package::WithMetaFar {
            path: PackagePath::from_name_and_variant(
                "fake-package".parse().unwrap(),
                "0".parse().unwrap(),
            ),
            required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
        };
        dynamic_index.add_package(hash, package.clone());

        dynamic_index.start_install(hash);

        assert_eq!(dynamic_index.packages(), hashmap! { hash => package });
    }

    #[test]
    fn cancel_install() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        dynamic_index.start_install(hash);
        assert_eq!(dynamic_index.packages(), hashmap! { hash => Package::Pending });
        dynamic_index.cancel_install(&hash);
        assert_eq!(dynamic_index.packages, hashmap! {});
    }

    #[test]
    fn cancel_install_with_meta_far() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        dynamic_index.add_package(
            hash,
            Package::WithMetaFar {
                path: PackagePath::from_name_and_variant(
                    "fake-package".parse().unwrap(),
                    "0".parse().unwrap(),
                ),
                required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
            },
        );
        dynamic_index.cancel_install(&hash);
        assert_eq!(dynamic_index.packages(), hashmap! {});
    }

    #[test]
    fn cancel_install_active() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let path = PackagePath::from_name_and_variant(
            "fake-package".parse().unwrap(),
            "0".parse().unwrap(),
        );
        let package = Package::Active {
            path: path.clone(),
            required_blobs: hashset! { Hash::from([3; 32]), Hash::from([4; 32]) },
        };
        dynamic_index.add_package(hash, package.clone());
        dynamic_index.cancel_install(&hash);
        assert_eq!(dynamic_index.packages(), hashmap! { hash => package });
        assert_eq!(dynamic_index.active_packages(), hashmap! { path => hash });
    }

    #[test]
    fn cancel_install_unknown() {
        let mut dynamic_index = DynamicIndex::new();

        dynamic_index.start_install(Hash::from([2; 32]));
        dynamic_index.cancel_install(&Hash::from([4; 32]));
        assert_eq!(dynamic_index.packages(), hashmap! { Hash::from([2; 32]) => Package::Pending });
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_load_cache_packages() {
        let present_package0 = PackageBuilder::new("present0")
            .add_resource_at("present-blob0", &b"contents0"[..])
            .build()
            .await
            .unwrap();
        let missing_content_blob = PackageBuilder::new("missing-content-blob")
            .add_resource_at("missing-blob", &b"missing-contents"[..])
            .build()
            .await
            .unwrap();
        let missing_meta_far = PackageBuilder::new("missing-meta-far")
            .add_resource_at("other-present-blob", &b"other-present-contents"[..])
            .build()
            .await
            .unwrap();
        let present_package1 = PackageBuilder::new("present1")
            .add_resource_at("present-blob1", &b"contents1"[..])
            .build()
            .await
            .unwrap();
        let present_subpackage = PackageBuilder::new("present-sub")
            .add_resource_at("present-sub-blob", &b"sub-contents-0"[..])
            .build()
            .await
            .unwrap();
        let present_superpackage = PackageBuilder::new("present-super")
            .add_subpackage("present-child", &present_subpackage)
            .add_resource_at("present-blob2", &b"contents2"[..])
            .build()
            .await
            .unwrap();
        let missing_subpackage = PackageBuilder::new("missing-sub").build().await.unwrap();
        let has_missing_subpackage = PackageBuilder::new("has-missing-subpackage")
            .add_subpackage("missing-child", &missing_subpackage)
            .build()
            .await
            .unwrap();

        let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
        let blobfs_dir = blobfs.root_dir().unwrap();

        present_package0.write_to_blobfs(&blobfs).await;
        missing_content_blob.write_to_blobfs(&blobfs).await;
        missing_meta_far.write_to_blobfs(&blobfs).await;
        present_package1.write_to_blobfs(&blobfs).await;
        present_superpackage.write_to_blobfs(&blobfs).await;
        has_missing_subpackage.write_to_blobfs(&blobfs).await;

        for (hash, _) in missing_content_blob.contents().1 {
            blobfs_dir.remove_file(hash.to_string()).unwrap();
        }
        blobfs_dir.remove_file(missing_meta_far.contents().0.merkle.to_string()).unwrap();
        blobfs_dir.remove_file(missing_subpackage.contents().0.merkle.to_string()).unwrap();

        let cache_packages = CachePackages::from_entries(vec![
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "present0".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                *present_package0.hash(),
            ),
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "missing-content-blob".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                *missing_content_blob.hash(),
            ),
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "missing-meta-far".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                *missing_meta_far.hash(),
            ),
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "present1".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                *present_package1.hash(),
            ),
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "present-super".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                *present_superpackage.hash(),
            ),
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "has-missing-subpackage".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                *has_missing_subpackage.hash(),
            ),
        ]);

        let mut dynamic_index = DynamicIndex::new();

        let () = load_cache_packages(&mut dynamic_index, &cache_packages, &blobfs.client()).await;

        let present0 = Package::Active {
            path: "present0/0".parse().unwrap(),
            required_blobs: present_package0.contents().1.into_keys().collect(),
        };
        let present1 = Package::Active {
            path: "present1/0".parse().unwrap(),
            required_blobs: present_package1.contents().1.into_keys().collect(),
        };
        let present_super = Package::Active {
            path: "present-super/0".parse().unwrap(),
            required_blobs: present_superpackage
                .content_and_subpackage_blobs()
                .unwrap()
                .into_keys()
                .collect(),
        };

        assert_eq!(
            dynamic_index.packages(),
            hashmap! {
                *present_package0.hash() => present0,
                *present_package1.hash() => present1,
                *present_superpackage.hash() => present_super,
            }
        );
        assert_eq!(
            dynamic_index.active_packages(),
            hashmap! {
                "present0/0".parse().unwrap() => *present_package0.hash(),
                "present1/0".parse().unwrap() => *present_package1.hash(),
                "present-super/0".parse().unwrap() => *present_superpackage.hash(),
            }
        );
        assert_eq!(
            dynamic_index.all_blobs(),
            present_package0
                .list_blobs()
                .into_iter()
                .chain(present_package1.list_blobs())
                .chain(present_superpackage.list_blobs())
                .collect()
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn fulfill_meta_far_transitions_package_from_pending_to_with_meta_far() {
        let mut dynamic_index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let path = PackagePath::from_name_and_variant(
            "fake-package".parse().unwrap(),
            "0".parse().unwrap(),
        );
        dynamic_index.start_install(hash);

        let () = dynamic_index.fulfill_meta_far(hash, path.clone()).unwrap();

        assert_eq!(
            dynamic_index.packages(),
            hashmap! {
                hash => Package::WithMetaFar {
                    path,
                    required_blobs: HashSet::new(),
                }
            }
        );
        assert_eq!(dynamic_index.active_packages(), hashmap! {});
    }

    #[fasync::run_singlethreaded(test)]
    async fn fulfill_meta_far_fails_on_unknown_package() {
        let mut index = DynamicIndex::new();

        assert_matches!(
            index
                .fulfill_meta_far(
                    Hash::from([2; 32]),
                    PackagePath::from_name_and_variant(
                        "unknown".parse().unwrap(),
                        "0".parse().unwrap()
                    )
                ),
            Err(FulfillNotNeededBlobError{hash, state})
                if hash == Hash::from([2; 32]) && state == "missing"
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn fulfill_meta_far_fails_on_package_with_meta_far() {
        let mut index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let path = PackagePath::from_name_and_variant(
            "with-meta-far-pkg".parse().unwrap(),
            "0".parse().unwrap(),
        );
        let package = Package::WithMetaFar { path: path.clone(), required_blobs: HashSet::new() };
        index.add_package(hash, package);

        assert_matches!(
            index.fulfill_meta_far(hash, path),
            Err(FulfillNotNeededBlobError{hash, state}) if hash == Hash::from([2; 32]) && state == "WithMetaFar"
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn fulfill_meta_far_fails_on_active_package() {
        let mut index = DynamicIndex::new();

        let hash = Hash::from([2; 32]);
        let path =
            PackagePath::from_name_and_variant("active".parse().unwrap(), "0".parse().unwrap());
        let package = Package::Active { path: path.clone(), required_blobs: HashSet::new() };
        index.add_package(hash, package);

        assert_matches!(
            index.fulfill_meta_far(hash, path),
            Err(FulfillNotNeededBlobError{hash, state})
                if hash == Hash::from([2; 32]) && state == "Active"
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn add_blobs_adds_blobs() {
        let mut dynamic_index = DynamicIndex::new();
        let hash = Hash::from([0; 32]);
        let path = PackagePath::from_name_and_variant(
            "fake-package".parse().unwrap(),
            "0".parse().unwrap(),
        );
        dynamic_index.start_install(hash);
        let () = dynamic_index.fulfill_meta_far(hash, path.clone()).unwrap();

        let () = dynamic_index.add_blobs(hash, &HashSet::from([Hash::from([2; 32])])).unwrap();

        assert_eq!(
            dynamic_index.packages(),
            hashmap! {
                hash => Package::WithMetaFar {
                    path,
                    required_blobs: HashSet::from([Hash::from([2; 32])]),
                }
            }
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn add_blobs_errors_on_unknown_package() {
        let mut dynamic_index = DynamicIndex::new();

        assert_matches!(
            dynamic_index.add_blobs(Hash::from([0; 32]), &hashset! { Hash::from([1; 32])}),
            Err(AddBlobsError::UnknownPackage)
        );
    }
}
