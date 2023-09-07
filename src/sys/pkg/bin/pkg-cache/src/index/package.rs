// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use crate::index::dynamic::{AddBlobsError, CompleteInstallError};
use {
    crate::index::{
        dynamic::{DynamicIndex, FulfillNotNeededBlobError},
        retained::RetainedIndex,
    },
    fuchsia_hash::Hash,
    fuchsia_inspect as finspect,
    fuchsia_pkg::PackagePath,
    std::{collections::HashSet, sync::Arc},
};

/// The index of packages known to pkg-cache.
#[derive(Debug)]
pub struct PackageIndex {
    dynamic: DynamicIndex,
    retained: RetainedIndex,
    #[allow(unused)]
    node: finspect::Node,
}

#[derive(thiserror::Error, Debug)]
pub enum FulfillMetaFarError {
    #[error("the blob is not needed by any index")]
    FulfillNotNeededBlob(#[from] FulfillNotNeededBlobError),

    #[error("creating RootDir for meta.far")]
    CreateRootDir(#[from] package_directory::Error),

    #[error("obtaining package path from meta.far")]
    PackagePath(#[from] package_directory::PathError),
}

impl PackageIndex {
    /// Creates a new, empty instance of the PackageIndex.
    pub fn new(node: finspect::Node) -> Self {
        Self {
            dynamic: DynamicIndex::new(node.create_child("dynamic")),
            retained: RetainedIndex::new(node.create_child("retained")),
            node,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_test() -> Self {
        let inspector = finspect::Inspector::default();
        Self::new(inspector.root().create_child("index"))
    }

    /// Notifies the appropriate indices that the package with the given hash is going to be
    /// installed, ensuring the meta far blob is protected by the index.
    pub fn start_install(&mut self, package_hash: Hash) {
        if self.retained.contains_package(&package_hash) {
            // The retained index intends to track this package, but the first event it wants to
            // take action on is when the meta far is readable. Nothing to do now.

            // This package wasn't already in the dynamic index, so don't add it to it.
            return;
        }

        self.dynamic.start_install(package_hash);
    }

    fn fulfill_meta_far(
        &mut self,
        package_hash: Hash,
        package_path: PackagePath,
    ) -> Result<(), FulfillMetaFarError> {
        let is_retained = self.retained.contains_package(&package_hash);
        // Transition the dynamic index state if it is interested in this package.  Report an error
        // if the package is not tracked by any index.
        let () = match self.dynamic.fulfill_meta_far(package_hash, package_path) {
            Err(crate::index::dynamic::FulfillNotNeededBlobError { .. }) if is_retained => Ok(()),
            Err(e @ crate::index::dynamic::FulfillNotNeededBlobError { .. }) => Err(e),
            Ok(()) => Ok(()),
        }?;

        Ok(())
    }

    /// Associate additional blobs (e.g. subpackage meta.fars and content blobs) with a package
    /// that is being cached.
    pub fn add_blobs(
        &mut self,
        package_hash: Hash,
        additional_blobs: HashSet<Hash>,
    ) -> Result<(), AddBlobsError> {
        // Notify the retained index if it is interested in this package.
        let is_retained = self.retained.add_blobs(&package_hash, &additional_blobs);

        let () = match self.dynamic.add_blobs(package_hash, &additional_blobs) {
            Ok(()) => (),
            Err(crate::index::dynamic::AddBlobsError::UnknownPackage) if is_retained => (),
            Err(e) => return Err(e),
        };
        Ok(())
    }

    /// Notifies the appropriate indices that the package with the given hash has completed
    /// installation.
    pub(crate) fn complete_install(
        &mut self,
        package_hash: Hash,
    ) -> Result<crate::cache_service::PackageStatus, CompleteInstallError> {
        let is_retained = self.retained.contains_package(&package_hash);

        match self.dynamic.complete_install(package_hash) {
            Err(_) if is_retained => Ok(crate::cache_service::PackageStatus::Other),
            Err(e) => Err(e),
            Ok(()) => Ok(crate::cache_service::PackageStatus::Active),
        }
    }

    /// Notifies the appropriate indices that the installation for the package with the given hash
    /// has been cancelled.
    pub fn cancel_install(&mut self, package_hash: &Hash) {
        self.dynamic.cancel_install(package_hash)
    }

    fn set_retained_index(&mut self, mut index: RetainedIndex) {
        // Populate the index with the blobs from the dynamic index.
        let retained_packages = index.retained_packages().copied().collect::<Vec<_>>();
        for hash in retained_packages {
            if let Some(blobs) = self.dynamic.lookup_content_blobs(&hash) {
                // TODO(fxbug.dev/112769) Consider replacing this panic with an error, or e.g.
                // adding a method to the retained index that makes both unnecessary.
                assert!(index.add_blobs(&hash, blobs));
            }
        }

        // Replace our retained index with the new one, which will populate content blobs from the
        // old retained index.
        self.retained.replace(index);
    }

    /// Returns if the package is active in the dynamic index.
    pub fn is_active(&self, hash: &Hash) -> bool {
        self.dynamic.is_active(hash)
    }

    /// Returns all blobs protected by the dynamic and retained indices.
    pub fn all_blobs(&self) -> HashSet<Hash> {
        let mut all = self.dynamic.all_blobs();
        all.extend(self.retained.all_blobs());
        all
    }
}

/// Load present cache packages into the dynamic index.
pub async fn load_cache_packages(
    index: &mut PackageIndex,
    cache_packages: &system_image::CachePackages,
    blobfs: &blobfs::Client,
) {
    crate::index::dynamic::load_cache_packages(&mut index.dynamic, cache_packages, blobfs).await
}

/// Notifies the appropriate inner indices that the given meta far blob is now available in blobfs.
/// Do not use this for regular blob (unless it's also a meta far).
pub async fn fulfill_meta_far_blob(
    index: &async_lock::RwLock<PackageIndex>,
    blobfs: &blobfs::Client,
    meta_hash: Hash,
) -> Result<package_directory::RootDir<blobfs::Client>, FulfillMetaFarError> {
    let root_dir = package_directory::RootDir::new(blobfs.clone(), meta_hash).await?;
    let () = index.write().await.fulfill_meta_far(meta_hash, root_dir.path().await?)?;
    Ok(root_dir)
}

/// Replaces the retained index with one that tracks the given meta far hashes.
pub async fn set_retained_index(
    index: &Arc<async_lock::RwLock<PackageIndex>>,
    blobfs: &blobfs::Client,
    meta_hashes: &[Hash],
) {
    // To avoid having to hold a lock while reading/parsing meta fars, first produce a fresh
    // retained index from meta fars available in blobfs.
    let new_retained = crate::index::retained::populate_retained_index(blobfs, meta_hashes).await;

    // Then, atomically, merge in available data from the dynamic/retained indices and swap in
    // the new retained index.
    index.write().await.set_retained_index(new_retained);
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            cache_service::PackageStatus, index::dynamic::Package,
            test_utils::add_meta_far_to_blobfs,
        },
        assert_matches::assert_matches,
        maplit::{hashmap, hashset},
    };

    fn hash(n: u8) -> Hash {
        Hash::from([n; 32])
    }
    fn path(s: &str) -> PackagePath {
        PackagePath::from_name_and_variant(s.parse().unwrap(), "0".parse().unwrap())
    }

    #[test]
    fn set_retained_index_with_dynamic_package_in_pending_state_puts_package_in_both() {
        let mut index = PackageIndex::new_test();

        index.start_install(hash(0));

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
        }));

        index.fulfill_meta_far(hash(0), path("pending")).unwrap();
        index.add_blobs(hash(0), HashSet::from([hash(1)])).unwrap();
        assert_eq!(index.complete_install(hash(0)).unwrap(), PackageStatus::Active);

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(1)])),
            }
        );
        assert_eq!(
            index.dynamic.active_packages(),
            hashmap! {
                path("pending") => hash(0),
            }
        );
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(0) => Package::Active {
                    path: path("pending"),
                    required_blobs: HashSet::from([hash(1)]),
                },
            }
        );
    }

    #[test]
    fn set_retained_index_with_dynamic_package_in_withmetafar_state_puts_package_in_both() {
        let mut index = PackageIndex::new_test();

        index.start_install(hash(0));
        index.start_install(hash(1));

        index.fulfill_meta_far(hash(0), path("withmetafar1")).unwrap();
        index.add_blobs(hash(0), HashSet::from([hash(10)])).unwrap();
        index.fulfill_meta_far(hash(1), path("withmetafar2")).unwrap();
        index.add_blobs(hash(1), HashSet::from([hash(11)])).unwrap();

        // Constructing a new RetainedIndex may race with a package install to the dynamic index.
        // Ensure index.set_retained_index handles both cases.
        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(10)])),
            hash(1) => None,
        }));

        assert_eq!(index.complete_install(hash(0)).unwrap(), PackageStatus::Active);
        assert_eq!(index.complete_install(hash(1)).unwrap(), PackageStatus::Active);

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(10)])),
                hash(1) => Some(HashSet::from_iter([hash(11)])),
            }
        );
        assert_eq!(
            index.dynamic.active_packages(),
            hashmap! {
                path("withmetafar1") => hash(0),
                path("withmetafar2") => hash(1),
            }
        );
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(0) => Package::Active {
                    path: path("withmetafar1"),
                    required_blobs: HashSet::from([hash(10)]),
                },
                hash(1) => Package::Active {
                    path: path("withmetafar2"),
                    required_blobs: HashSet::from([hash(11)]),
                },
            }
        );
    }

    #[test]
    fn set_retained_index_hashes_are_extended_with_dynamic_index_hashes() {
        let mut index = PackageIndex::new_test();
        index.start_install(hash(0));
        index.fulfill_meta_far(hash(0), path("withmetafar1")).unwrap();
        index.add_blobs(hash(0), HashSet::from([hash(1), hash(2)])).unwrap();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(1)])),
        }));

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(1), hash(2)])),
            }
        );
    }

    #[test]
    fn set_retained_index_with_no_dynamic_package_entry_puts_package_in_retained_only() {
        let mut index = PackageIndex::new_test();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
        }));

        index.start_install(hash(0));
        index.fulfill_meta_far(hash(0), path("retaiendonly")).unwrap();
        index.add_blobs(hash(0), HashSet::from([hash(123)])).unwrap();
        assert_eq!(index.complete_install(hash(0)).unwrap(), PackageStatus::Other);

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(123)])),
            }
        );
        assert_eq!(index.dynamic.active_packages(), hashmap! {});
        assert_eq!(index.dynamic.packages(), hashmap! {});
    }

    #[test]
    fn retained_index_is_not_informed_of_packages_it_does_not_track() {
        let mut index = PackageIndex::new_test();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(10)])),
            hash(1) => None,
        }));

        // install a package not tracked by the retained index
        index.start_install(hash(2));
        index.fulfill_meta_far(hash(2), path("dynamic-only")).unwrap();
        index.add_blobs(hash(2), HashSet::from([hash(10)])).unwrap();
        assert_eq!(index.complete_install(hash(2)).unwrap(), PackageStatus::Active);

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(10)])),
                hash(1) => None,
            }
        );
        assert_eq!(
            index.dynamic.active_packages(),
            hashmap! {
                path("dynamic-only") => hash(2),
            }
        );
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::Active {
                    path: path("dynamic-only"),
                    required_blobs: HashSet::from([hash(10)]),
                },
            }
        );
    }

    #[test]
    fn set_retained_index_to_self_is_nop() {
        let mut index = PackageIndex::new_test();

        index.start_install(hash(2));
        index.start_install(hash(3));
        index.fulfill_meta_far(hash(3), path("before")).unwrap();
        index.add_blobs(hash(3), HashSet::from([hash(10)])).unwrap();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(11)])),
            hash(1) => None,
            hash(2) => None,
            hash(3) => None,
            hash(4) => None,
            hash(5) => None,
        }));

        index.start_install(hash(4));
        index.start_install(hash(5));
        index.fulfill_meta_far(hash(5), path("after")).unwrap();
        index.add_blobs(hash(5), HashSet::from([hash(12)])).unwrap();

        let retained_index = index.retained.clone();

        index.set_retained_index(retained_index.clone());
        assert_eq!(index.retained, retained_index);
    }

    #[test]
    fn add_blobs_adds_to_dynamic_and_retained() {
        let mut index = PackageIndex::new_test();

        index.start_install(hash(2));
        index.fulfill_meta_far(hash(2), path("some-path")).unwrap();
        index.add_blobs(hash(2), HashSet::from([hash(10)])).unwrap();
        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(2) => Some(HashSet::from_iter([hash(10)])),
        }));

        index.add_blobs(hash(2), HashSet::from([hash(11)])).unwrap();

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(2) => Some(HashSet::from([hash(10), hash(11)]))
            }
        );
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::WithMetaFar {
                    path: path("some-path"),
                    required_blobs: HashSet::from([hash(10), hash(11)]),
                }
            }
        );
    }

    #[test]
    fn add_blobs_ignores_missing_dynamic_if_retained() {
        let mut index = PackageIndex::new_test();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(2) => Some(HashSet::from_iter([hash(10)])),
        }));
        index.start_install(hash(2));
        index.fulfill_meta_far(hash(2), path("some-path")).unwrap();

        index.add_blobs(hash(2), HashSet::from([hash(11)])).unwrap();

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(2) => Some(HashSet::from([hash(10), hash(11)]))
            }
        );
        assert_eq!(index.dynamic.packages(), hashmap! {});
    }

    #[test]
    fn add_blobs_errors_if_dynamic_index_in_wrong_state() {
        let mut index = PackageIndex::new_test();

        index.start_install(hash(2));

        assert_matches!(
            index.add_blobs(hash(2), hashset! {hash(11)}),
            Err(AddBlobsError::WrongState("Pending"))
        );
    }

    #[test]
    fn cancel_install_only_affects_dynamic_index() {
        let mut index = PackageIndex::new_test();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
        }));

        index.start_install(hash(0));
        index.start_install(hash(1));

        index.fulfill_meta_far(hash(0), path("a")).unwrap();
        index.add_blobs(hash(0), HashSet::from([hash(10)])).unwrap();
        index.fulfill_meta_far(hash(1), path("b")).unwrap();
        index.add_blobs(hash(1), HashSet::from([hash(11)])).unwrap();

        index.cancel_install(&hash(0));
        index.cancel_install(&hash(1));

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(10)])),
            }
        );
        assert_eq!(index.dynamic.active_packages(), hashmap! {});
        assert_eq!(index.dynamic.packages(), hashmap! {});
    }

    #[test]
    fn all_blobs_produces_union_of_dynamic_and_retained_all_blobs() {
        let mut index = PackageIndex::new_test();

        index.start_install(hash(0));

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
            hash(5) => None,
            hash(6) => Some(HashSet::from_iter([hash(60), hash(61)])),
        }));

        index.start_install(hash(1));

        index.fulfill_meta_far(hash(0), path("pkg1")).unwrap();
        index.add_blobs(hash(0), HashSet::from([hash(10)])).unwrap();
        index.fulfill_meta_far(hash(1), path("pkg2")).unwrap();
        index.add_blobs(hash(1), HashSet::from([hash(11), hash(61)])).unwrap();

        assert_eq!(index.complete_install(hash(0)).unwrap(), PackageStatus::Active);
        assert_eq!(index.complete_install(hash(1)).unwrap(), PackageStatus::Active);

        assert_eq!(
            index.all_blobs(),
            hashset! {hash(0), hash(1), hash(5), hash(6), hash(10), hash(11), hash(60), hash(61)}
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fulfill_meta_far_blob_with_missing_blobs() {
        let mut index = PackageIndex::new_test();

        let path = PackagePath::from_name_and_variant(
            "fake-package".parse().unwrap(),
            "0".parse().unwrap(),
        );
        index.start_install(hash(2));

        let index = Arc::new(async_lock::RwLock::new(index));

        let (blobfs_fake, blobfs) = fuchsia_pkg_testing::blobfs::Fake::new();

        add_meta_far_to_blobfs(&blobfs_fake, hash(2), "fake-package", [hash(3)], []);

        fulfill_meta_far_blob(&index, &blobfs, hash(2)).await.unwrap();
        index.write().await.add_blobs(hash(2), HashSet::from([hash(3)])).unwrap();

        let index = index.read().await;
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::WithMetaFar {
                    path,
                    required_blobs: HashSet::from([hash(3)]),
                }
            }
        );
        assert_eq!(index.dynamic.active_packages(), hashmap! {});
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fulfill_meta_far_blob_not_needed() {
        let index = PackageIndex::new_test();
        let index = Arc::new(async_lock::RwLock::new(index));

        let (blobfs_fake, blobfs) = fuchsia_pkg_testing::blobfs::Fake::new();

        let hash = Hash::from([2; 32]);
        add_meta_far_to_blobfs(&blobfs_fake, hash, "fake-package", [], []);

        assert_matches!(
            fulfill_meta_far_blob(&index, &blobfs, hash).await,
            Err(FulfillMetaFarError::FulfillNotNeededBlob(FulfillNotNeededBlobError{hash, state})) if hash == Hash::from([2; 32]) && state == "missing"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fulfill_meta_far_blob_not_found() {
        let mut index = PackageIndex::new_test();

        let meta_far_hash = Hash::from([2; 32]);
        index.start_install(meta_far_hash);

        let index = Arc::new(async_lock::RwLock::new(index));

        let (_blobfs_fake, blobfs) = fuchsia_pkg_testing::blobfs::Fake::new();

        assert_matches!(
            fulfill_meta_far_blob(&index, &blobfs, meta_far_hash).await,
            Err(FulfillMetaFarError::CreateRootDir(package_directory::Error::MissingMetaFar))
        );
    }
}
