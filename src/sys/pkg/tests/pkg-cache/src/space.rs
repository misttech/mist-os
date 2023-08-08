// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{blob_written, compress_and_write_blob, get_missing_blobs, TestEnv},
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio, fidl_fuchsia_paver as fpaver,
    fidl_fuchsia_pkg::{self as fpkg, NeededBlobsMarker},
    fidl_fuchsia_pkg_ext::BlobId,
    fidl_fuchsia_space::ErrorCode,
    fuchsia_async::OnSignals,
    fuchsia_pkg_testing::{Package, PackageBuilder, SystemImageBuilder},
    fuchsia_zircon::{self as zx, Status},
    futures::TryFutureExt,
    mock_paver::{hooks as mphooks, MockPaverServiceBuilder, PaverEvent},
    rand::prelude::*,
    std::collections::{BTreeSet, HashMap},
};

#[fuchsia::test]
async fn gc_error_pending_commit() {
    let (throttle_hook, throttler) = mphooks::throttle();

    let system_image_package = SystemImageBuilder::new().build().await;
    let env = TestEnv::builder()
        .blobfs_from_system_image(&system_image_package)
        .await
        .paver_service_builder(
            MockPaverServiceBuilder::new()
                .insert_hook(throttle_hook)
                .insert_hook(mphooks::config_status(|_| Ok(fpaver::ConfigurationStatus::Pending))),
        )
        .build()
        .await;

    // Allow the paver to emit enough events to unblock the CommitStatusProvider FIDL server, but
    // few enough to guarantee the commit is still pending.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::QueryCurrentConfiguration,
        PaverEvent::QueryConfigurationStatus { configuration: fpaver::Configuration::A },
    ]);
    assert_matches!(env.proxies.space_manager.gc().await, Ok(Err(ErrorCode::PendingCommit)));

    // When the commit completes, GC should unblock as well.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::SetConfigurationHealthy { configuration: fpaver::Configuration::A },
        PaverEvent::SetConfigurationUnbootable { configuration: fpaver::Configuration::B },
        PaverEvent::BootManagerFlush,
    ]);
    let event_pair =
        env.proxies.commit_status_provider.is_current_system_committed().await.unwrap();
    assert_eq!(OnSignals::new(&event_pair, zx::Signals::USER_0).await, Ok(zx::Signals::USER_0));
    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
}

/// Create a TestEnv and SystemImage package from the supplied static packages.
async fn setup_test_env(static_packages: &[&Package]) -> (TestEnv, Package) {
    let system_image = SystemImageBuilder::new().static_packages(static_packages).build().await;
    let env = TestEnv::builder()
        .blobfs_from_system_image_and_extra_packages(&system_image, static_packages)
        .await
        .build()
        .await;
    let () = env.block_until_started().await;
    (env, system_image)
}

/// Assert that performing a GC does nothing on a blobfs that only includes the system image and
/// static packages.
#[fuchsia::test]
async fn gc_noop_system_image() {
    let static_package = PackageBuilder::new("static-package")
        .add_resource_at("resource", &[][..])
        .build()
        .await
        .unwrap();
    let (env, _) = setup_test_env(&[&static_package]).await;
    let original_blobs = env.blobfs.list_blobs().expect("to get an initial list of blobs");

    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    assert_eq!(env.blobfs.list_blobs().expect("to get new blobs"), original_blobs);
}

/// Assert that any blobs protected by the dynamic index are ineligible for garbage collection.
/// Furthermore, ensure that an incomplete package does not lose blobs, and that the previous
/// packages' blobs survive until the new package is entirely written.
#[fuchsia::test]
async fn gc_dynamic_index_protected() {
    let (env, sysimg_pkg) = setup_test_env(&[]).await;

    let pkg = PackageBuilder::new("gc_dynamic_index_protected_pkg_cache")
        .add_resource_at("bin/x", "bin-x-version-1".as_bytes())
        .add_resource_at("data/unchanging", "unchanging-content".as_bytes())
        .build()
        .await
        .unwrap();
    let _: fio::DirectoryProxy =
        crate::get_and_verify_package(&env.proxies.package_cache, &pkg).await;

    // Ensure that the just-fetched blobs are not reaped by a GC cycle.
    let mut test_blobs = env.blobfs.list_blobs().expect("to get an initial list of blobs");

    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    assert_eq!(env.blobfs.list_blobs().expect("to get new blobs"), test_blobs);

    // Fetch an updated package, skipping both its content blobs to guarantee that there are
    // missing blobs. This helps us ensure that the meta.far is not lost.
    let pkgprime = PackageBuilder::new("gc_dynamic_index_protected_pkg_cache")
        .add_resource_at("bin/x", "bin-x-version-2".as_bytes())
        .add_resource_at("bin/y", "bin-y-version-1".as_bytes())
        .add_resource_at("data/unchanging", "unchanging-content".as_bytes())
        .build()
        .await
        .unwrap();

    // Here, we persist the meta.far
    let meta_blob_info =
        fpkg::BlobInfo { blob_id: BlobId::from(*pkgprime.hash()).into(), length: 0 };
    let package_cache = &env.proxies.package_cache;

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let get_fut = package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    let (meta_far, contents) = pkgprime.contents();
    let mut contents = contents
        .into_iter()
        .map(|(hash, bytes)| (BlobId::from(hash), bytes))
        .collect::<HashMap<_, Vec<u8>>>();

    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Delivery).await.unwrap().unwrap().unwrap();
    let () = compress_and_write_blob(&meta_far.contents, *meta_blob).await.unwrap();
    let () = blob_written(&needed_blobs, meta_far.merkle).await;

    // Ensure that the new meta.far is persisted despite having missing blobs, and the "old" blobs
    // are not removed.
    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    test_blobs.insert(*pkgprime.hash());
    assert_eq!(env.blobfs.list_blobs().expect("to get new blobs"), test_blobs);

    // Fully fetch pkgprime, and ensure that blobs from the old package are not persisted past GC.
    let missing_blobs = get_missing_blobs(&needed_blobs).await;
    for blob in missing_blobs {
        let buf = contents.remove(&blob.blob_id.into()).unwrap();

        let content_blob = needed_blobs
            .open_blob(&blob.blob_id, fpkg::BlobType::Delivery)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        let () = compress_and_write_blob(&buf, *content_blob).await.unwrap();
        let () = blob_written(&needed_blobs, BlobId::from(blob.blob_id).into()).await;

        // Run a GC to try to reap blobs protected by meta far.
        assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    }

    let () = get_fut.await.unwrap().unwrap();
    let () = pkgprime.verify_contents(&dir).await.unwrap();
    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));

    // At this point, we expect blobfs to only contain the blobs from the system image package and
    // from pkgprime.
    let expected_blobs = sysimg_pkg
        .list_blobs()
        .unwrap()
        .union(&pkgprime.list_blobs().unwrap())
        .cloned()
        .collect::<BTreeSet<_>>();

    assert_eq!(env.blobfs.list_blobs().expect("all blobs"), expected_blobs);
}

#[fuchsia::test]
async fn gc_unowned_blob() {
    let env = TestEnv::builder().build().await;
    let unowned_content = &b"blob not referenced by any protected packages"[..];
    let unowned_hash = fuchsia_merkle::MerkleTree::from_reader(unowned_content).unwrap().root();
    let () = env.write_to_blobfs(&unowned_hash, unowned_content).await;
    assert!(env.blobfs.list_blobs().unwrap().contains(&unowned_hash));

    let () = env.proxies.space_manager.gc().await.unwrap().unwrap();

    assert!(!env.blobfs.list_blobs().unwrap().contains(&unowned_hash));
}

/// Effectively the same as gc_dynamic_index_protected, except that the updated package also
/// existed as a static package as well.
#[fuchsia::test]
async fn gc_updated_static_package() {
    let static_package = PackageBuilder::new("gc_updated_static_package_pkg_cache")
        .add_resource_at("bin/x", "bin-x-version-0".as_bytes())
        .add_resource_at("data/unchanging", "unchanging-content".as_bytes())
        .build()
        .await
        .unwrap();

    let (env, _) = setup_test_env(&[&static_package]).await;
    let initial_blobs = env.blobfs.list_blobs().expect("to get initial blob list");

    let pkg = PackageBuilder::new("gc_updated_static_package_pkg_cache")
        .add_resource_at("bin/x", "bin-x-version-1".as_bytes())
        .add_resource_at("data/unchanging", "unchanging-content".as_bytes())
        .build()
        .await
        .unwrap();
    let _: fio::DirectoryProxy =
        crate::get_and_verify_package(&env.proxies.package_cache, &pkg).await;

    // Ensure that the just-fetched blobs are not reaped by a GC cycle.
    let mut test_blobs = env.blobfs.list_blobs().expect("to get an initial list of blobs");

    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    assert_eq!(env.blobfs.list_blobs().expect("to get new blobs"), test_blobs);

    let pkgprime = PackageBuilder::new("gc_updated_static_package_pkg_cache")
        .add_resource_at("bin/x", "bin-x-version-2".as_bytes())
        .add_resource_at("bin/y", "bin-y-version-1".as_bytes())
        .add_resource_at("data/unchanging", "unchanging-content".as_bytes())
        .build()
        .await
        .unwrap();

    // Here, we persist the meta.far
    let meta_blob_info =
        fpkg::BlobInfo { blob_id: BlobId::from(*pkgprime.hash()).into(), length: 0 };
    let package_cache = &env.proxies.package_cache;

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let get_fut = package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    let (meta_far, contents) = pkgprime.contents();
    let mut contents = contents
        .into_iter()
        .map(|(hash, bytes)| (BlobId::from(hash), bytes))
        .collect::<HashMap<_, Vec<u8>>>();

    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Delivery).await.unwrap().unwrap().unwrap();
    let () = compress_and_write_blob(&meta_far.contents, *meta_blob).await.unwrap();
    let () = blob_written(&needed_blobs, meta_far.merkle).await;

    // Ensure that the new meta.far is persisted despite having missing blobs, and the "old" blobs
    // are not removed.
    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    test_blobs.insert(*pkgprime.hash());
    assert_eq!(env.blobfs.list_blobs().expect("to get new blobs"), test_blobs);

    // Fully fetch pkgprime, and ensure that blobs from the old package are not persisted past GC.
    let missing_blobs = get_missing_blobs(&needed_blobs).await;
    for blob in missing_blobs {
        let buf = contents.remove(&blob.blob_id.into()).unwrap();

        let content_blob = needed_blobs
            .open_blob(&blob.blob_id, fpkg::BlobType::Delivery)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        let () = compress_and_write_blob(&buf, *content_blob).await.unwrap();
        let () = blob_written(&needed_blobs, BlobId::from(blob.blob_id).into()).await;

        // Run a GC to try to reap blobs protected by meta far.
        assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));
    }

    let () = get_fut.await.unwrap().unwrap();
    let () = pkgprime.verify_contents(&dir).await.unwrap();
    assert_matches!(env.proxies.space_manager.gc().await, Ok(Ok(())));

    // At this point, we expect blobfs to only contain the blobs from the system image package and
    // from pkgprime.
    let expected_blobs =
        initial_blobs.union(&pkgprime.list_blobs().unwrap()).cloned().collect::<BTreeSet<_>>();

    assert_eq!(env.blobfs.list_blobs().expect("all blobs"), expected_blobs);
}

async fn gc_frees_space_so_write_can_succeed(blob_implementation: blobfs_ramdisk::Implementation) {
    // Create a 7 MB blobfs (14,336 blocks * 512 bytes / block).
    let small_blobfs = blobfs_ramdisk::Ramdisk::builder()
        .block_count(14336)
        .into_blobfs_builder()
        .await
        .expect("made blobfs builder")
        .implementation(blob_implementation)
        .start()
        .await
        .expect("started blobfs");

    // Write an orphaned incompressible 4 MB blob.
    let mut orphan_data = vec![0; 4 * 1024 * 1024];
    StdRng::from_seed([0u8; 32]).fill(&mut orphan_data[..]);
    let orphan_hash = fuchsia_merkle::MerkleTree::from_reader(&orphan_data[..]).unwrap().root();
    let () = small_blobfs.add_blob_from(orphan_hash, &orphan_data[..]).await.unwrap();
    assert!(small_blobfs.list_blobs().unwrap().contains(&orphan_hash));

    // Create a TestEnv using this blobfs.
    let system_image_package = SystemImageBuilder::new().build().await;
    system_image_package.write_to_blobfs(&small_blobfs).await;
    let env = TestEnv::builder()
        .blobfs_and_system_image_hash(small_blobfs, Some(*system_image_package.hash()))
        .blobfs_impl(blob_implementation)
        .build()
        .await;

    // Try to cache a package with an incompressible 4 MB meta.far.
    let pkg = PackageBuilder::new("pkg-a")
        .add_resource_at("meta/asset", &orphan_data[..])
        .build()
        .await
        .expect("build large package");
    assert_ne!(*pkg.hash(), orphan_hash);
    let meta_blob_info = fpkg::BlobInfo { blob_id: BlobId::from(*pkg.hash()).into(), length: 0 };
    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    // Writing the meta.far should fail with NO_SPACE.
    let (meta_far, _contents) = pkg.contents();
    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Delivery).await.unwrap().unwrap().unwrap();
    let () = compress_and_write_blob(&meta_far.contents, *meta_blob)
        .await
        .unwrap_err()
        .assert_out_of_space();

    // GC should free space, allowing the meta.far write and therefore get to succeed.
    let () = env.proxies.space_manager.gc().await.unwrap().unwrap();
    assert!(!env.blobfs.list_blobs().unwrap().contains(&orphan_hash));
    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Delivery).await.unwrap().unwrap().unwrap();
    let () = compress_and_write_blob(&meta_far.contents, *meta_blob).await.unwrap();
    let () = blob_written(&needed_blobs, meta_far.merkle).await;
    let (_, blob_iterator_server_end) =
        fidl::endpoints::create_proxy::<fpkg::BlobInfoIteratorMarker>().unwrap();
    let () = needed_blobs.get_missing_blobs(blob_iterator_server_end).unwrap();

    let () = get_fut.await.unwrap().unwrap();
    let () = pkg.verify_contents(&dir).await.unwrap();
}

#[fuchsia_async::run_singlethreaded(test)]
async fn gc_frees_space_so_write_can_succeed_cpp_blobfs() {
    let () = gc_frees_space_so_write_can_succeed(blobfs_ramdisk::Implementation::CppBlobfs).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn gc_frees_space_so_write_can_succeed_fxblob() {
    let () = gc_frees_space_so_write_can_succeed(blobfs_ramdisk::Implementation::Fxblob).await;
}

enum GcProtection {
    Dynamic,
    Retained,
}

async fn blobs_protected_from_gc_during_get(gc_protection: GcProtection) {
    let env = TestEnv::builder().build().await;
    let initial_blobs = env.blobfs.list_blobs().unwrap();

    let subsubpackage = PackageBuilder::new("subsubpackage")
        .add_resource_at("subsubpackage-blob", "subsubpackage-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();
    let subpackage = PackageBuilder::new("subpackage")
        .add_subpackage("my-subsubpackage", &subsubpackage)
        .add_resource_at("subpackage-blob", "subpackage-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();
    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("my-subpackage", &subpackage)
        .add_resource_at("superpackage-blob", "superpackage-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    // Verify that none of the to-be-fetched blobs are in blobfs.
    let to_be_fetched: Vec<(fuchsia_merkle::Hash, Vec<u8>)> = vec![
        (*superpackage.hash(), superpackage.contents().0.contents.clone()),
        superpackage.contents().1.into_iter().next().unwrap(),
        (*subpackage.hash(), subpackage.contents().0.contents.clone()),
        subpackage.contents().1.into_iter().next().unwrap(),
        (*subsubpackage.hash(), subsubpackage.contents().0.contents),
        subsubpackage.contents().1.into_iter().next().unwrap(),
    ];
    let to_be_fetched_hashes = BTreeSet::from_iter(to_be_fetched.iter().map(|(hash, _)| *hash));
    assert_eq!(to_be_fetched_hashes.len(), 6);
    assert!(initial_blobs.is_disjoint(&to_be_fetched_hashes));

    // Verify that none of the to-be-fetched blobs are protected yet.
    for (hash, bytes) in to_be_fetched.iter() {
        let () = env.blobfs.write_blob(*hash, bytes).await.unwrap();
    }
    assert!(env.blobfs.list_blobs().unwrap().is_superset(&to_be_fetched_hashes));
    let () = env.proxies.space_manager.gc().await.unwrap().unwrap();
    assert!(env.blobfs.list_blobs().unwrap().is_disjoint(&to_be_fetched_hashes));

    // Start the Get.
    match gc_protection {
        GcProtection::Retained => {
            crate::replace_retained_packages(
                &env.proxies.retained_packages,
                &[(*superpackage.hash()).into()],
            )
            .await
        }

        // Ephemeral packages are added to the dynamic index unless they are already in the
        // retained index.
        GcProtection::Dynamic => (),
    }
    let meta_blob_info =
        fpkg::BlobInfo { blob_id: BlobId::from(*superpackage.hash()).into(), length: 0 };
    let (needed_blobs, needed_blobs_server) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (dir, dir_server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server, Some(dir_server))
        .map_ok(|res| res.map_err(Status::from_raw));

    let blob_is_present_and_protected = |i: usize| {
        let (i, env, to_be_fetched) = (i, &env, &to_be_fetched);
        async move {
            assert!(env.blobfs.list_blobs().unwrap().contains(&to_be_fetched[i].0));
            let () = env.proxies.space_manager.gc().await.unwrap().unwrap();
            assert!(env.blobfs.list_blobs().unwrap().contains(&to_be_fetched[i].0));
        }
    };

    // Write the superpackage meta.far.
    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Delivery).await.unwrap().unwrap().unwrap();
    let () = compress_and_write_blob(&to_be_fetched[0].1, *meta_blob).await.unwrap();
    let () = blob_written(&needed_blobs, to_be_fetched[0].0).await;
    let () = blob_is_present_and_protected(0).await;

    // Read the superpackage content blob and subpackage meta.far from the missing blobs iterator
    // to guarantee that pkg-cache is ready for them to be written.
    let (blob_iterator, blob_iterator_server_end) =
        fidl::endpoints::create_proxy::<fpkg::BlobInfoIteratorMarker>().unwrap();
    let () = needed_blobs.get_missing_blobs(blob_iterator_server_end).unwrap();
    assert_eq!(
        blob_iterator.next().await.unwrap(),
        vec![
            fpkg::BlobInfo { blob_id: BlobId::from(to_be_fetched[2].0).into(), length: 0 },
            fpkg::BlobInfo { blob_id: BlobId::from(to_be_fetched[1].0).into(), length: 0 },
        ]
    );

    let write_blob = |i: usize| {
        let (i, needed_blobs, to_be_fetched) = (i, &needed_blobs, &to_be_fetched);
        async move {
            let blob = needed_blobs
                .open_blob(&BlobId::from(to_be_fetched[i].0).into(), fpkg::BlobType::Delivery)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let () = compress_and_write_blob(&to_be_fetched[i].1, *blob).await.unwrap();
            let () = blob_written(&needed_blobs, to_be_fetched[i].0).await;
        }
    };

    // Write the superpackage content blob.
    let () = write_blob(1).await;
    let () = blob_is_present_and_protected(1).await;

    // Write the subpackage meta.far.
    let () = write_blob(2).await;
    let () = blob_is_present_and_protected(2).await;

    // Prepare pkg-cache for the subpackage content blob and subsubpackage meta.far.
    assert_eq!(
        blob_iterator.next().await.unwrap(),
        vec![
            fpkg::BlobInfo { blob_id: BlobId::from(to_be_fetched[4].0).into(), length: 0 },
            fpkg::BlobInfo { blob_id: BlobId::from(to_be_fetched[3].0).into(), length: 0 },
        ]
    );

    // Write the subpackage content blob.
    let () = write_blob(3).await;
    let () = blob_is_present_and_protected(3).await;

    // Write the subsubpackage meta.far.
    let () = write_blob(4).await;
    let () = blob_is_present_and_protected(4).await;

    // Prepare pkg-cache for the subsubpackage content blob.
    assert_eq!(
        blob_iterator.next().await.unwrap(),
        vec![fpkg::BlobInfo { blob_id: BlobId::from(to_be_fetched[5].0).into(), length: 0 },]
    );

    // Write the subsubpackage content blob.
    let () = write_blob(5).await;
    let () = blob_is_present_and_protected(5).await;

    // Complete the Get.
    assert_eq!(blob_iterator.next().await.unwrap(), vec![]);
    let () = get_fut.await.unwrap().unwrap();
    let () = superpackage.verify_contents(&dir).await.unwrap();

    // All blobs should still be protected.
    let () = env.proxies.space_manager.gc().await.unwrap().unwrap();
    assert!(env.blobfs.list_blobs().unwrap().is_superset(&to_be_fetched_hashes));

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn blobs_protected_from_gc_during_get_by_retained_index() {
    let () = blobs_protected_from_gc_during_get(GcProtection::Retained).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn subpackage_blobs_protected_from_gc_by_dynamic_index() {
    let () = blobs_protected_from_gc_during_get(GcProtection::Dynamic).await;
}
