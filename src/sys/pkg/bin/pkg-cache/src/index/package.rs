// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use super::dynamic::CompleteInstallError;
use {
    super::{
        dynamic::{DynamicIndex, FulfillNotNeededBlobError},
        retained::RetainedIndex,
        writing::{CleanupGuard, WritingIndex},
    },
    fidl_fuchsia_pkg as fpkg,
    fuchsia_hash::Hash,
    fuchsia_inspect as finspect,
    futures::future::{BoxFuture, FutureExt as _},
    std::{collections::HashSet, sync::Arc},
};

/// The index of packages known to pkg-cache.
#[derive(Clone, Debug)]
pub struct PackageIndex {
    dynamic: DynamicIndex,
    retained: RetainedIndex,
    writing: WritingIndex,
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

#[derive(thiserror::Error, Debug)]
pub enum AddBlobsError {
    #[error("adding blobs to the dynamic index")]
    Dynamic(#[source] super::dynamic::AddBlobsError),

    #[error("adding blobs to the writing index")]
    Writing(#[source] super::writing::AddBlobsError),
}

impl PackageIndex {
    /// Creates an empty PackageIndex.
    pub fn from_config(protect_dynamic_packages: crate::DynamicProtection) -> Self {
        Self {
            dynamic: DynamicIndex::from_config(protect_dynamic_packages),
            retained: RetainedIndex::new(),
            writing: WritingIndex::new(),
        }
    }

    /// Creates an empty PackageIndex.
    #[cfg(test)]
    pub fn new() -> Self {
        Self {
            dynamic: DynamicIndex::new(),
            retained: RetainedIndex::new(),
            writing: WritingIndex::new(),
        }
    }

    /// Notifies the appropriate indices that the package with the given hash is going to be
    /// written, ensuring the meta.far blob is protected by the index.
    /// The returned `CleanupGuard` must be given back to `Self::stop_writing`.
    pub fn start_writing(
        &mut self,
        pkg: Hash,
        gc_protection: fpkg::GcProtection,
    ) -> Option<CleanupGuard> {
        match gc_protection {
            // The purpose of Retained protection Get's is to not automatically protect the package
            // and instead to rely entirely on the client (the system-updater) to protect the
            // package using the Retained index. This allows the system-updater to meet the GC
            // requirements of the OTA process.
            fpkg::GcProtection::Retained => None,
            fpkg::GcProtection::OpenPackageTracking => Some(self.writing.start(pkg)),
        }
    }

    /// Notifies the appropriate indices that the package with the given hash is going to be
    /// installed, ensuring the meta far blob is protected by the index.
    pub fn start_install(&mut self, package_hash: Hash, gc_protection: fpkg::GcProtection) {
        match gc_protection {
            // The purpose of Retained protection Get's is to not automatically protect the package
            // and instead to rely entirely on the client (the system-updater) to protect the
            // package using the Retained index. This allows the system-updater to meet the GC
            // requirements of the OTA process.
            fpkg::GcProtection::Retained => (),
            fpkg::GcProtection::OpenPackageTracking => self.dynamic.start_install(package_hash),
        }
    }

    fn fulfill_meta_far(
        &mut self,
        package_hash: Hash,
        package_path: fuchsia_pkg::PackagePath,
        gc_protection: fpkg::GcProtection,
    ) -> Result<(), FulfillMetaFarError> {
        match gc_protection {
            // The content blobs will be added by a subsequent call to `add_blobs`.
            fpkg::GcProtection::Retained => Ok(()),
            fpkg::GcProtection::OpenPackageTracking => {
                let () = self.dynamic.fulfill_meta_far(package_hash, package_path)?;
                Ok(())
            }
        }
    }

    /// Associate additional blobs (e.g. subpackage meta.fars and content blobs) with a package
    /// that is being cached.
    pub fn add_blobs(
        &mut self,
        package_hash: Hash,
        additional_blobs: HashSet<Hash>,
        gc_protection: fpkg::GcProtection,
    ) -> Result<(), AddBlobsError> {
        // Always give the retained index information about dependent blobs to improve OTA forward
        // progress.
        let _: bool = self.retained.add_blobs(&package_hash, &additional_blobs);
        match gc_protection {
            fpkg::GcProtection::Retained => Ok(()),
            fpkg::GcProtection::OpenPackageTracking => {
                let () = self
                    .writing
                    .add_blobs(&package_hash, &additional_blobs)
                    .map_err(AddBlobsError::Writing)?;
                self.dynamic
                    .add_blobs(package_hash, &additional_blobs)
                    .map_err(AddBlobsError::Dynamic)
            }
        }
    }

    /// Notifies the appropriate indices that the package with the given hash has completed
    /// installation.
    pub(crate) fn complete_install(
        &mut self,
        package_hash: Hash,
        gc_protection: fpkg::GcProtection,
    ) -> Result<(), CompleteInstallError> {
        match gc_protection {
            fpkg::GcProtection::Retained => Ok(()),
            fpkg::GcProtection::OpenPackageTracking => self.dynamic.complete_install(package_hash),
        }
    }

    /// Notifies the appropriate indices that the installation for the package with the given hash
    /// has been cancelled.
    pub fn cancel_install(&mut self, package_hash: &Hash, gc_protection: fpkg::GcProtection) {
        match gc_protection {
            fpkg::GcProtection::Retained => (),
            fpkg::GcProtection::OpenPackageTracking => self.dynamic.cancel_install(package_hash),
        }
    }

    /// Notifies the appropriate indices that the package corresponding to `guard` that was
    /// obtained from an earlier call to `Self::start_writing` is no longer being written.
    pub fn stop_writing(
        &mut self,
        guard: Option<super::writing::CleanupGuard>,
    ) -> Result<(), super::writing::StopError> {
        if let Some(guard) = guard {
            let () = self.writing.stop(guard)?;
        }
        Ok(())
    }

    fn set_retained_index(&mut self, mut index: RetainedIndex) {
        // Populate the index with the blobs from the dynamic index.
        let retained_packages = index.retained_packages().copied().collect::<Vec<_>>();
        for hash in retained_packages {
            if let Some(blobs) = self.dynamic.lookup_content_blobs(&hash) {
                // TODO(https://fxbug.dev/42064103) Consider replacing this panic with an error, or e.g.
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

    /// Returns all blobs protected by this index.
    pub fn all_blobs(&self) -> HashSet<Hash> {
        let mut all = self.dynamic.all_blobs();
        all.extend(self.retained.all_blobs());
        all.extend(self.writing.all_blobs());
        all
    }

    /// Returns a callback to be given to `finspect::Node::record_lazy_child`.
    /// The callback holds a weak reference to the PackageIndex.
    pub fn record_lazy_inspect(
        this: &Arc<async_lock::RwLock<Self>>,
    ) -> impl Fn() -> BoxFuture<'static, Result<finspect::Inspector, anyhow::Error>>
           + Send
           + Sync
           + 'static {
        let this = Arc::downgrade(this);
        move || {
            let this = this.clone();
            async move {
                let inspector = finspect::Inspector::default();
                if let Some(this) = this.upgrade() {
                    let index: Self = (*this.read().await).clone();
                    let root = inspector.root();
                    let () = root.record_child("dynamic", |n| index.dynamic.record_inspect(&n));
                    let () = root.record_child("retained", |n| index.retained.record_inspect(&n));
                    let () = root.record_child("writing", |n| index.writing.record_inspect(&n));
                } else {
                    inspector.root().record_string("error", "the package index was dropped");
                }
                Ok(inspector)
            }
            .boxed()
        }
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
    gc_protection: fpkg::GcProtection,
) -> Result<package_directory::RootDir<blobfs::Client>, FulfillMetaFarError> {
    let root_dir = package_directory::RootDir::new_raw(blobfs.clone(), meta_hash, None).await?;
    let () =
        index.write().await.fulfill_meta_far(meta_hash, root_dir.path().await?, gc_protection)?;
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
        crate::{index::dynamic::Package, test_utils::add_meta_far_to_blobfs},
        assert_matches::assert_matches,
        maplit::hashmap,
    };

    fn hash(n: u8) -> Hash {
        Hash::from([n; 32])
    }
    fn path(s: &str) -> fuchsia_pkg::PackagePath {
        fuchsia_pkg::PackagePath::from_name_and_variant(s.parse().unwrap(), "0".parse().unwrap())
    }

    #[test]
    fn set_retained_index_with_dynamic_package_in_pending_state_puts_package_in_both() {
        let mut index = PackageIndex::new();
        let guard = index.start_writing(hash(0), fpkg::GcProtection::OpenPackageTracking);

        index.start_install(hash(0), fpkg::GcProtection::OpenPackageTracking);

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
        }));

        index
            .fulfill_meta_far(hash(0), path("pending"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(0), HashSet::from([hash(1)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        let () = index.complete_install(hash(0), fpkg::GcProtection::OpenPackageTracking).unwrap();

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

        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn set_retained_index_with_dynamic_package_in_withmetafar_state_puts_package_in_both() {
        let mut index = PackageIndex::new();
        let guard0 = index.start_writing(hash(0), fpkg::GcProtection::OpenPackageTracking);
        let guard1 = index.start_writing(hash(1), fpkg::GcProtection::OpenPackageTracking);

        index.start_install(hash(0), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(1), fpkg::GcProtection::OpenPackageTracking);

        index
            .fulfill_meta_far(
                hash(0),
                path("withmetafar1"),
                fpkg::GcProtection::OpenPackageTracking,
            )
            .unwrap();
        index
            .add_blobs(hash(0), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .fulfill_meta_far(
                hash(1),
                path("withmetafar2"),
                fpkg::GcProtection::OpenPackageTracking,
            )
            .unwrap();
        index
            .add_blobs(hash(1), HashSet::from([hash(11)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();

        // Constructing a new RetainedIndex may race with a package install to the dynamic index.
        // Ensure index.set_retained_index handles both cases.
        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(10)])),
            hash(1) => None,
        }));

        let () = index.complete_install(hash(0), fpkg::GcProtection::OpenPackageTracking).unwrap();
        let () = index.complete_install(hash(1), fpkg::GcProtection::OpenPackageTracking).unwrap();

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

        let () = index.stop_writing(guard0).unwrap();
        let () = index.stop_writing(guard1).unwrap();
    }

    #[test]
    fn set_retained_index_hashes_are_extended_with_dynamic_and_writing_index_hashes() {
        let mut index = PackageIndex::new();
        let guard = index.start_writing(hash(0), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(0), fpkg::GcProtection::OpenPackageTracking);
        index
            .fulfill_meta_far(
                hash(0),
                path("withmetafar1"),
                fpkg::GcProtection::OpenPackageTracking,
            )
            .unwrap();
        index
            .add_blobs(
                hash(0),
                HashSet::from([hash(1), hash(2)]),
                fpkg::GcProtection::OpenPackageTracking,
            )
            .unwrap();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(1)])),
        }));

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(1), hash(2)])),
            }
        );

        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn retained_index_is_not_informed_of_packages_it_does_not_track() {
        let mut index = PackageIndex::new();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(10)])),
            hash(1) => None,
        }));

        // install a package not tracked by the retained index
        let guard = index.start_writing(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index
            .fulfill_meta_far(
                hash(2),
                path("dynamic-only"),
                fpkg::GcProtection::OpenPackageTracking,
            )
            .unwrap();
        index
            .add_blobs(hash(2), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        let () = index.complete_install(hash(2), fpkg::GcProtection::OpenPackageTracking).unwrap();

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
        assert_eq!(
            index.writing.packages(),
            hashmap! {
                hash(2) => HashSet::from_iter([hash(10)]),
            }
        );
        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn set_retained_index_to_self_is_nop() {
        let mut index = PackageIndex::new();
        let guard0 = index.start_writing(hash(3), fpkg::GcProtection::OpenPackageTracking);
        let guard1 = index.start_writing(hash(5), fpkg::GcProtection::OpenPackageTracking);

        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(3), fpkg::GcProtection::OpenPackageTracking);
        index
            .fulfill_meta_far(hash(3), path("before"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(3), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => Some(HashSet::from_iter([hash(11)])),
            hash(1) => None,
            hash(2) => None,
            hash(3) => None,
            hash(4) => None,
            hash(5) => None,
        }));

        index.start_install(hash(4), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(5), fpkg::GcProtection::OpenPackageTracking);
        index
            .fulfill_meta_far(hash(5), path("after"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(5), HashSet::from([hash(12)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();

        let retained_index = index.retained.clone();

        index.set_retained_index(retained_index.clone());
        assert_eq!(index.retained, retained_index);

        let () = index.stop_writing(guard0).unwrap();
        let () = index.stop_writing(guard1).unwrap();
    }

    #[test]
    fn add_blobs_with_open_package_tracking_protection_adds_to_dynamic_and_retained_and_writing() {
        let mut index = PackageIndex::new();

        let guard = index.start_writing(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index
            .fulfill_meta_far(hash(2), path("some-path"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(2), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(2) => Some(HashSet::from_iter([hash(10)])),
        }));

        index
            .add_blobs(hash(2), HashSet::from([hash(11)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();

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
        assert_eq!(
            index.writing.packages(),
            hashmap! {
                hash(2) => HashSet::from([hash(10), hash(11)])
            }
        );
        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn add_blobs_with_retained_protection_adds_to_retained_index_only() {
        let mut index = PackageIndex::new();

        let guard = index.start_writing(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);
        index
            .fulfill_meta_far(hash(2), path("some-path"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(2) => None
        }));

        index.add_blobs(hash(2), HashSet::from([hash(11)]), fpkg::GcProtection::Retained).unwrap();

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(2) => Some(HashSet::from([hash(11)]))
            }
        );
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::WithMetaFar {
                    path: path("some-path"),
                    required_blobs: HashSet::new(),
                }
            }
        );
        assert_eq!(
            index.writing.packages(),
            hashmap! {
                hash(2) => HashSet::new()
            }
        );
        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn retained_index_does_not_prevent_addition_to_dynamic_index() {
        let mut index = PackageIndex::new();
        let guard = index.start_writing(hash(2), fpkg::GcProtection::OpenPackageTracking);

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(2) => None,
        }));

        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::Pending
            }
        );

        index
            .fulfill_meta_far(hash(2), path("some-path"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(2), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::WithMetaFar {
                    path: path("some-path"),
                    required_blobs: HashSet::from([hash(10)]),
                }
            }
        );

        let () = index.complete_install(hash(2), fpkg::GcProtection::OpenPackageTracking).unwrap();
        assert_eq!(
            index.dynamic.packages(),
            hashmap! {
                hash(2) => Package::Active {
                    path: path("some-path"),
                    required_blobs: HashSet::from([hash(10)]),
                }
            }
        );

        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn add_blobs_errors_if_dynamic_index_in_wrong_state() {
        let mut index = PackageIndex::new();

        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);
        let guard = index.start_writing(hash(2), fpkg::GcProtection::OpenPackageTracking);

        assert_matches!(
            index.add_blobs(
                hash(2),
                HashSet::from([hash(11)]),
                fpkg::GcProtection::OpenPackageTracking
            ),
            Err(AddBlobsError::Dynamic(super::super::dynamic::AddBlobsError::WrongState(
                "Pending"
            )))
        );
        let () = index.stop_writing(guard).unwrap();
    }

    #[test]
    fn add_blobs_errors_if_writing_index_in_wrong_state() {
        let mut index = PackageIndex::new();

        assert_matches!(
            index.add_blobs(
                hash(2),
                HashSet::from([hash(11)]),
                fpkg::GcProtection::OpenPackageTracking
            ),
            Err(AddBlobsError::Writing(super::super::writing::AddBlobsError::UnknownPackage{
                pkg
            })) if pkg == hash(2)
        );
    }

    #[test]
    fn cancel_install_only_affects_dynamic_index() {
        let mut index = PackageIndex::new();

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
        }));

        let guard0 = index.start_writing(hash(0), fpkg::GcProtection::OpenPackageTracking);
        let guard1 = index.start_writing(hash(1), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(0), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(1), fpkg::GcProtection::OpenPackageTracking);

        index
            .fulfill_meta_far(hash(0), path("a"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(0), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .fulfill_meta_far(hash(1), path("b"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(1), HashSet::from([hash(11)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();

        index.cancel_install(&hash(0), fpkg::GcProtection::OpenPackageTracking);
        index.cancel_install(&hash(1), fpkg::GcProtection::OpenPackageTracking);

        assert_eq!(
            index.retained.packages(),
            hashmap! {
                hash(0) => Some(HashSet::from_iter([hash(10)])),
            }
        );
        assert_eq!(index.dynamic.active_packages(), hashmap! {});
        assert_eq!(index.dynamic.packages(), hashmap! {});
        assert_eq!(
            index.writing.packages(),
            hashmap! {
                hash(0) => HashSet::from_iter([hash(10)]),
                hash(1) => HashSet::from_iter([hash(11)])
            }
        );
        let () = index.stop_writing(guard0).unwrap();
        let () = index.stop_writing(guard1).unwrap();
    }

    #[test]
    fn all_blobs_produces_union_of_dynamic_and_retained_and_writing_all_blobs() {
        let mut index = PackageIndex::new();

        let guard0 = index.start_writing(hash(0), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(0), fpkg::GcProtection::OpenPackageTracking);

        index.set_retained_index(RetainedIndex::from_packages(hashmap! {
            hash(0) => None,
            hash(5) => None,
            hash(6) => Some(HashSet::from_iter([hash(60), hash(61)])),
        }));

        let guard1 = index.start_writing(hash(1), fpkg::GcProtection::OpenPackageTracking);
        index.start_install(hash(1), fpkg::GcProtection::OpenPackageTracking);

        index
            .fulfill_meta_far(hash(0), path("pkg1"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(hash(0), HashSet::from([hash(10)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .fulfill_meta_far(hash(1), path("pkg2"), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();
        index
            .add_blobs(
                hash(1),
                HashSet::from([hash(11), hash(61)]),
                fpkg::GcProtection::OpenPackageTracking,
            )
            .unwrap();

        let () = index.complete_install(hash(0), fpkg::GcProtection::OpenPackageTracking).unwrap();
        let () = index.complete_install(hash(1), fpkg::GcProtection::OpenPackageTracking).unwrap();

        assert_eq!(
            index.all_blobs(),
            HashSet::from([
                hash(0),
                hash(1),
                hash(5),
                hash(6),
                hash(10),
                hash(11),
                hash(60),
                hash(61)
            ])
        );
        let () = index.stop_writing(guard0).unwrap();
        let () = index.stop_writing(guard1).unwrap();
    }

    #[fuchsia::test]
    async fn fulfill_meta_far_blob_with_missing_blobs() {
        let mut index = PackageIndex::new();
        let guard = index.start_writing(hash(2), fpkg::GcProtection::OpenPackageTracking);

        let path = fuchsia_pkg::PackagePath::from_name_and_variant(
            "fake-package".parse().unwrap(),
            "0".parse().unwrap(),
        );
        index.start_install(hash(2), fpkg::GcProtection::OpenPackageTracking);

        let index = Arc::new(async_lock::RwLock::new(index));

        let (blobfs_fake, blobfs) = fuchsia_pkg_testing::blobfs::Fake::new();

        add_meta_far_to_blobfs(&blobfs_fake, hash(2), "fake-package", [hash(3)], []);

        fulfill_meta_far_blob(&index, &blobfs, hash(2), fpkg::GcProtection::OpenPackageTracking)
            .await
            .unwrap();
        index
            .write()
            .await
            .add_blobs(hash(2), HashSet::from([hash(3)]), fpkg::GcProtection::OpenPackageTracking)
            .unwrap();

        assert_eq!(
            index.read().await.dynamic.packages(),
            hashmap! {
                hash(2) => Package::WithMetaFar {
                    path,
                    required_blobs: HashSet::from([hash(3)]),
                }
            }
        );
        assert_eq!(index.read().await.dynamic.active_packages(), hashmap! {});

        let () = index.write().await.stop_writing(guard).unwrap();
    }

    #[fuchsia::test]
    async fn fulfill_meta_far_blob_not_needed() {
        let index = PackageIndex::new();
        let index = Arc::new(async_lock::RwLock::new(index));

        let (blobfs_fake, blobfs) = fuchsia_pkg_testing::blobfs::Fake::new();

        let hash = Hash::from([2; 32]);
        add_meta_far_to_blobfs(&blobfs_fake, hash, "fake-package", [], []);

        assert_matches!(
            fulfill_meta_far_blob(
                &index,
                &blobfs,
                hash,
                fpkg::GcProtection::OpenPackageTracking
            ).await,
            Err(FulfillMetaFarError::FulfillNotNeededBlob(FulfillNotNeededBlobError{hash, state}))
                if hash == Hash::from([2; 32]) && state == "missing"
        );
    }

    #[fuchsia::test]
    async fn fulfill_meta_far_blob_not_found() {
        let mut index = PackageIndex::new();

        let meta_far_hash = Hash::from([2; 32]);
        index.start_install(meta_far_hash, fpkg::GcProtection::OpenPackageTracking);

        let index = Arc::new(async_lock::RwLock::new(index));

        let (_blobfs_fake, blobfs) = fuchsia_pkg_testing::blobfs::Fake::new();

        assert_matches!(
            fulfill_meta_far_blob(
                &index,
                &blobfs,
                meta_far_hash,
                fpkg::GcProtection::OpenPackageTracking
            )
            .await,
            Err(FulfillMetaFarError::CreateRootDir(package_directory::Error::MissingMetaFar))
        );
    }

    #[test]
    fn stop_writing_fowarded_to_writing_index() {
        let mut index = PackageIndex::new();

        let guard = index.start_writing(hash(0), fpkg::GcProtection::OpenPackageTracking);
        let () = index.stop_writing(guard).unwrap();
        assert_eq!(index.writing.packages(), hashmap! {})
    }
}
