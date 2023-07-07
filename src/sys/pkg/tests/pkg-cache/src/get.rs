// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        blob_written, get_and_verify_package, get_and_verify_packages, get_missing_blobs,
        write_blob, TestEnv,
    },
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg::{self as fpkg, BlobInfo, BlobInfoIteratorMarker, NeededBlobsMarker},
    fidl_fuchsia_pkg_ext::{self as fpkg_ext, BlobId},
    fuchsia_merkle::MerkleTree,
    fuchsia_pkg_testing::{Package, PackageBuilder, SystemImageBuilder},
    fuchsia_zircon::Status,
    futures::prelude::*,
};

#[fuchsia_async::run_singlethreaded(test)]
async fn get_multiple_packages_with_no_content_blobs() {
    let env = TestEnv::builder().build().await;

    let packages = vec![
        PackageBuilder::new("pkg-a").build().await.unwrap(),
        PackageBuilder::new("pkg-b").build().await.unwrap(),
        PackageBuilder::new("pkg-c").build().await.unwrap(),
        PackageBuilder::new("pkg-d").build().await.unwrap(),
    ];

    // Assert the packages are not present before the get
    for pkg in &packages {
        assert_matches!(
            env.get_already_cached(&pkg.meta_far_merkle_root().to_string()).await,
            Err(e) if e.was_not_cached()
        );
    }

    let () = get_and_verify_packages(&env.proxies.package_cache, &packages).await;

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_single_package_with_no_content_blobs() {
    let env = TestEnv::builder().build().await;
    let mut initial_blobfs_blobs = env.blobfs.list_blobs().unwrap();

    let pkg = PackageBuilder::new("single-blob").build().await.unwrap();

    let meta_blob_info =
        BlobInfo { blob_id: BlobId::from(*pkg.meta_far_merkle_root()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    let (meta_far, _) = pkg.contents();

    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Uncompressed).await.unwrap().unwrap().unwrap();

    let () = write_blob(&meta_far.contents, *meta_blob).await.unwrap();
    let () = blob_written(&needed_blobs, meta_far.merkle).await;

    assert_eq!(get_missing_blobs(&needed_blobs).await, vec![]);

    let () = get_fut.await.unwrap().unwrap();
    let () = pkg.verify_contents(&dir).await.unwrap();

    // All blobs in the package should now be present in blobfs.
    let mut expected_blobs = pkg.list_blobs().unwrap();
    expected_blobs.append(&mut initial_blobfs_blobs);
    assert_eq!(env.blobfs.list_blobs().unwrap(), expected_blobs);

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_multiple_packages_with_content_blobs() {
    let env = TestEnv::builder().build().await;

    let packages = vec![
        PackageBuilder::new("multi-content-a")
            .add_resource_at("bin/foo", "a-bin-foo".as_bytes())
            .add_resource_at("data/content", "a-data-content".as_bytes())
            .add_resource_at("data/content2", "a-data-content2".as_bytes())
            .build()
            .await
            .unwrap(),
        PackageBuilder::new("multi-content-b")
            .add_resource_at("bin/foo", "b-bin-foo".as_bytes())
            .add_resource_at("data/content", "b-data-content-same".as_bytes())
            .add_resource_at("data/content2", "b-data-content-same".as_bytes())
            .build()
            .await
            .unwrap(),
        PackageBuilder::new("multi-content-c")
            .add_resource_at("bin/foo", "c-bin-foo".as_bytes())
            .add_resource_at("data/content", "c-data-content".as_bytes())
            .add_resource_at("data/content2", "c-data-content2".as_bytes())
            .build()
            .await
            .unwrap(),
        PackageBuilder::new("multi-content-d")
            .add_resource_at("bin/foo", "d-bin-foo".as_bytes())
            .add_resource_at("data/content", "d-data-content".as_bytes())
            .add_resource_at("data/content2", "d-data-content2".as_bytes())
            .build()
            .await
            .unwrap(),
    ];

    let () = get_and_verify_packages(&env.proxies.package_cache, &packages).await;

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_and_hold_directory() {
    let env = TestEnv::builder().build().await;

    let package = PackageBuilder::new("pkg-a").build().await.unwrap();

    // Request and write a package, hold the package directory.
    let dir = get_and_verify_package(&env.proxies.package_cache, &package).await;

    let meta_blob_info =
        BlobInfo { blob_id: BlobId::from(*package.meta_far_merkle_root()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();

    let (dir_2, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    // Request same package again.
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    // `OpenMetaBlob()` for already cached package closes the channel with with a `ZX_OK` epitaph.
    assert_matches!(
        needed_blobs.open_meta_blob(fpkg::BlobType::Uncompressed).await,
        Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. })
    );

    let () = get_fut.await.unwrap().unwrap();

    // Verify package directories from both requests.
    let () = package.verify_contents(&dir).await.unwrap();
    let () = package.verify_contents(&dir_2).await.unwrap();
}

#[fuchsia_async::run_singlethreaded(test)]
async fn unavailable_when_client_drops_needed_blobs_channel() {
    let env = TestEnv::builder().build().await;

    let pkg = PackageBuilder::new("pkg-a").build().await.unwrap();

    let meta_blob_info =
        BlobInfo { blob_id: BlobId::from(*pkg.meta_far_merkle_root()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (_dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    drop(needed_blobs);

    assert_eq!(get_fut.await.unwrap(), Err(Status::UNAVAILABLE));
}

#[fuchsia_async::run_singlethreaded(test)]
async fn handles_partially_written_pkg() {
    let env = TestEnv::builder().build().await;

    let pkg = PackageBuilder::new("partially-written")
        .add_resource_at("written-1", &b"some contents"[..])
        .add_resource_at("not-written-1", &b"different contents"[..])
        .build()
        .await
        .unwrap();

    let meta_far_hash = *pkg.meta_far_merkle_root();
    let meta_far_data = {
        use std::io::Read as _;

        let mut bytes = vec![];
        pkg.meta_far().unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    };

    let meta_blob_info = BlobInfo { blob_id: BlobId::from(meta_far_hash).into(), length: 0 };

    // Write the meta far to blobfs and one of the two content blobs to blobfs.
    let () = env.write_to_blobfs(&meta_far_hash, &meta_far_data).await;
    {
        let data = &b"some contents"[..];
        let hash = MerkleTree::from_reader(data).unwrap().root();
        let () = env.write_to_blobfs(&hash, data).await;
    }

    // Perform a Get(), expecting to only write the 1 remaining content blob.
    let dir = {
        let data = &b"different contents"[..];
        let hash = MerkleTree::from_reader(data).unwrap().root();

        let pkg_cache = env.client();
        let mut get = pkg_cache.get(meta_blob_info.into()).unwrap();

        assert_matches!(get.open_meta_blob(fpkg::BlobType::Uncompressed).await.unwrap(), None);
        let missing = get.get_missing_blobs().try_concat().await.unwrap();
        assert_eq!(missing, vec![fpkg_ext::BlobInfo { blob_id: hash.into(), length: 0 }]);

        let blob = get.open_blob(hash.into(), fpkg::BlobType::Uncompressed).await.unwrap().unwrap();
        let (blob, closer) = (blob.blob, blob.closer);
        let blob = match blob.truncate(data.len() as u64).await.unwrap() {
            fpkg_ext::cache::TruncateBlobSuccess::NeedsData(blob) => blob,
            fpkg_ext::cache::TruncateBlobSuccess::AllWritten(_) => panic!("not the empty blob"),
        };
        let blob = match blob.write(data).await.unwrap() {
            fpkg_ext::cache::BlobWriteSuccess::NeedsData(_) => panic!("blob should be written"),
            fpkg_ext::cache::BlobWriteSuccess::AllWritten(blob) => blob,
        };
        let () = blob.blob_written().await.unwrap();
        closer.close().await;

        get.finish().await.unwrap()
    };

    let () = pkg.verify_contents(&dir.into_proxy()).await.unwrap();

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_already_present_on_fs() {
    let pkg = PackageBuilder::new("some-package")
        .add_resource_at("some-blob", &b"some contents"[..])
        .build()
        .await
        .unwrap();
    let system_image_package = SystemImageBuilder::new().cache_packages(&[&pkg]).build().await;
    let env = TestEnv::builder()
        .blobfs_from_system_image_and_extra_packages(&system_image_package, &[&pkg])
        .await
        .build()
        .await;

    let meta_blob_info =
        BlobInfo { blob_id: BlobId::from(*pkg.meta_far_merkle_root()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();

    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

    // Call `PackageCache.Get()` for already cached package.
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    // `OpenMetaBlob()` for already cached package closes the channel with with a `ZX_OK` epitaph.
    assert_matches!(
        needed_blobs.open_meta_blob(fpkg::BlobType::Uncompressed).await,
        Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. })
    );

    // The remote end sends the epitaph, and then at some point later, closes the channel.
    // We check for both here to account for the channel not yet being closed when the
    // `GetMissingBlobs` call occurs.
    let (_blob_iterator, blob_iterator_server_end) =
        fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();
    assert_matches!(
        needed_blobs.get_missing_blobs(blob_iterator_server_end),
        Ok(()) | Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. })
    );

    let () = get_fut.await.unwrap().unwrap();

    // `dir` is resolved to package directory.
    let () = pkg.verify_contents(&dir).await.unwrap();

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_already_present_on_fs_with_pre_closed_needed_blobs() {
    let pkg = PackageBuilder::new("some-package")
        .add_resource_at("some-blob", &b"some contents"[..])
        .build()
        .await
        .unwrap();
    let system_image_package = SystemImageBuilder::new().cache_packages(&[&pkg]).build().await;
    let env = TestEnv::builder()
        .blobfs_from_system_image_and_extra_packages(&system_image_package, &[&pkg])
        .await
        .build()
        .await;

    let meta_blob_info =
        BlobInfo { blob_id: BlobId::from(*pkg.meta_far_merkle_root()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (pkgdir, pkgdir_server_end) =
        fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

    drop(needed_blobs);

    // Call `PackageCache.Get()` for already cached package.
    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(pkgdir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    let () = get_fut.await.unwrap().unwrap();

    let () = pkg.verify_contents(&pkgdir).await.unwrap();

    let () = env.stop().await;
}

// Does a PackageCache.Get of `superpackage`, then verifies that all the transitive subpackages are
// available via PackageCache.Get without needing to write any more blobs
async fn verify_superpackage_get(superpackage: &Package, subpackages: &[Package]) {
    let env = TestEnv::builder().build().await;
    let _: fio::DirectoryProxy =
        get_and_verify_package(&env.proxies.package_cache, superpackage).await;

    for subpackage in subpackages {
        let _: fio::DirectoryProxy =
            crate::verify_package_cached(&env.proxies.package_cache, subpackage).await;
    }

    let () = env.stop().await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_subpackage() {
    let subpackage = PackageBuilder::new("subpackage").build().await.unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("my-subpackage", &subpackage)
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_subpackage_with_content_blobs() {
    let subpackage = PackageBuilder::new("subpackage")
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

    let () = verify_superpackage_get(&superpackage, &[subpackage]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_two_subpackages() {
    let subpackage0 = PackageBuilder::new("subpackage0").build().await.unwrap();
    let subpackage1 = PackageBuilder::new("subpackage1").build().await.unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage-0", &subpackage0)
        .add_subpackage("subpackage-1", &subpackage1)
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage0, subpackage1]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_two_subpackages_with_content_blobs() {
    let subpackage0 = PackageBuilder::new("subpackage0")
        .add_resource_at("subpackage0-blob", "subpackage0-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();
    let subpackage1 = PackageBuilder::new("subpackage1")
        .add_resource_at("subpackage1-blob", "subpackage1-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage-0", &subpackage0)
        .add_subpackage("subpackage-1", &subpackage1)
        .add_resource_at("superpackage-blob", "superpackage-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage0, subpackage1]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_duplicate_subpackages() {
    let subpackage = PackageBuilder::new("subpackage").build().await.unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage", &subpackage)
        .add_subpackage("duplicate", &subpackage)
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_duplicate_subpackages_with_content_blobs() {
    let subpackage = PackageBuilder::new("subpackage")
        .add_resource_at("subpackage-blob", "subpackage-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage", &subpackage)
        .add_subpackage("duplicate", &subpackage)
        .add_resource_at("superpackage-blob", "superpackage-blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_subpackage_meta_far_as_content_blob() {
    let subpackage = PackageBuilder::new("subpackage").build().await.unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("my-subpackage", &subpackage)
        .add_resource_at("content-blob-meta-far", subpackage.contents().0.contents.as_slice())
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_two_subpackages_that_share_a_content_blob() {
    let subpackage0 = PackageBuilder::new("subpackage0")
        .add_resource_at("blob", "shared-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let subpackage1 = PackageBuilder::new("subpackage1")
        .add_resource_at("blob", "shared-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage-0", &subpackage0)
        .add_subpackage("subpackage-1", &subpackage1)
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage0, subpackage1]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_transitive_subpackage() {
    let subpackage1 = PackageBuilder::new("subpackage1")
        .add_resource_at("some-blob", "blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let subpackage0 = PackageBuilder::new("subpackage0")
        .add_resource_at("other-blob", "different-contents".as_bytes())
        .add_subpackage("depth", &subpackage1)
        .build()
        .await
        .unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage-0", &subpackage0)
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage0, subpackage1]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn get_package_with_transitive_subpackage_as_content_blob() {
    let subpackage1 = PackageBuilder::new("subpackage1")
        .add_resource_at("some-blob", "blob-contents".as_bytes())
        .build()
        .await
        .unwrap();

    let subpackage0 = PackageBuilder::new("subpackage0")
        .add_resource_at("other-blob", "different-contents".as_bytes())
        .add_subpackage("depth", &subpackage1)
        .build()
        .await
        .unwrap();

    let superpackage = PackageBuilder::new("superpackage")
        .add_subpackage("subpackage-0", &subpackage0)
        .add_resource_at("content-blob-meta-far", subpackage1.contents().0.contents.as_slice())
        .build()
        .await
        .unwrap();

    let () = verify_superpackage_get(&superpackage, &[subpackage0, subpackage1]).await;
}

#[fuchsia_async::run_singlethreaded(test)]
async fn fxblob() {
    let env = TestEnv::builder().use_fxblob().build().await;
    let initial_blobfs_blobs = env.blobfs.list_blobs().unwrap();
    let pkg = PackageBuilder::new("single-blob")
        .add_resource_at("content-blob", &b"content-blob-contents"[..])
        .build()
        .await
        .unwrap();
    let meta_blob_info =
        BlobInfo { blob_id: BlobId::from(*pkg.meta_far_merkle_root()).into(), length: 0 };
    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

    let get_fut = env
        .proxies
        .package_cache
        .get(&meta_blob_info, needed_blobs_server_end, Some(dir_server_end))
        .map_ok(|res| res.map_err(Status::from_raw));

    let (meta_far, _) = pkg.contents();
    let meta_blob =
        needed_blobs.open_meta_blob(fpkg::BlobType::Delivery).await.unwrap().unwrap().unwrap();
    match &*meta_blob {
        fpkg::BlobWriter::File(_) => panic!("should be using fuchsia.fxfs.BlobWriter"),
        fpkg::BlobWriter::Writer(_) => (),
    }
    let compressed = delivery_blob::Type1Blob::generate(
        &meta_far.contents,
        delivery_blob::CompressionMode::Always,
    );
    let () = write_blob(&compressed, *meta_blob).await.unwrap();
    let () = blob_written(&needed_blobs, meta_far.merkle).await;

    let [missing_blob]: [_; 1] = get_missing_blobs(&needed_blobs).await.try_into().unwrap();
    let content_blob = needed_blobs
        .open_blob(&missing_blob.blob_id, fpkg::BlobType::Delivery)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match &*content_blob {
        fpkg::BlobWriter::File(_) => panic!("should be using fuchsia.fxfs.BlobWriter"),
        fpkg::BlobWriter::Writer(_) => (),
    }
    let compressed = delivery_blob::Type1Blob::generate(
        b"content-blob-contents",
        delivery_blob::CompressionMode::Always,
    );
    let () = write_blob(&compressed, *content_blob).await.unwrap();
    let () = blob_written(&needed_blobs, BlobId::from(missing_blob.blob_id).into()).await;

    let () = get_fut.await.unwrap().unwrap();
    let () = pkg.verify_contents(&dir).await.unwrap();

    // All blobs in the package should now be present in blobfs.
    let pkg_blobs = pkg.list_blobs().unwrap();
    assert!(initial_blobfs_blobs.is_disjoint(&pkg_blobs));
    assert!(env.blobfs.list_blobs().unwrap().is_superset(&pkg_blobs));

    let () = env.stop().await;
}
