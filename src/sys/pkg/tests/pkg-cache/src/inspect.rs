// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    blob_written, compress_and_write_blob, get_missing_blobs, replace_retained_packages,
    write_meta_far, write_needed_blobs, TestEnv,
};
use assert_matches::assert_matches;
use diagnostics_assertions::{assert_data_tree, tree_assertion, AnyProperty};
use diagnostics_hierarchy::DiagnosticsHierarchy;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_pkg::{self as fpkg, BlobInfo, NeededBlobsMarker};
use fidl_fuchsia_pkg_ext::BlobId;
use fuchsia_pkg_testing::{PackageBuilder, SystemImageBuilder};
use futures::prelude::*;
use std::collections::HashMap;

#[fuchsia::test]
async fn system_image_hash_present() {
    let system_image_package = SystemImageBuilder::new().build().await;
    let env =
        TestEnv::builder().blobfs_from_system_image(&system_image_package).await.build().await;
    env.block_until_started().await;

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "system_image": system_image_package.hash().to_string(),
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn system_image_hash_ignored() {
    let env = TestEnv::builder().ignore_system_image().build().await;
    env.block_until_started().await;

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "system_image": "ignored",
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn base_packages() {
    let env = TestEnv::builder().build().await;
    env.block_until_started().await;

    let hierarchy = env.inspect_hierarchy().await;

    assert_data_tree!(
        hierarchy,
        root: contains {
            "base-packages": {
                "fuchsia-pkg://fuchsia.com/system_image": {
                    "hash": AnyProperty
                }
            }
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn cache_packages() {
    let cache_package = PackageBuilder::new("a-cache-package").build().await.unwrap();
    let system_image_package =
        SystemImageBuilder::new().cache_packages(&[&cache_package]).build().await;
    let env = TestEnv::builder()
        .blobfs_from_system_image_and_extra_packages(&system_image_package, &[&cache_package])
        .await
        .build()
        .await;
    env.block_until_started().await;

    let hierarchy = env.inspect_hierarchy().await;

    assert_data_tree!(
        hierarchy,
        root: contains {
            "cache-packages": {
                "fuchsia-pkg://fuchsia.com/a-cache-package": {
                    "hash": cache_package.hash().to_string()
                }
            }
        }

    );
    env.stop().await;
}

#[fuchsia::test]
async fn executability_restrictions_enabled() {
    let env = TestEnv::builder().build().await;
    env.block_until_started().await;

    let hierarchy = env.inspect_hierarchy().await;

    assert_data_tree!(
        hierarchy,
        root: contains {
            "executability-restrictions": "Enforce".to_string(),
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn executability_restrictions_disabled() {
    let system_image_package =
        SystemImageBuilder::new().pkgfs_disable_executability_restrictions().build().await;
    let env =
        TestEnv::builder().blobfs_from_system_image(&system_image_package).await.build().await;
    env.block_until_started().await;

    let hierarchy = env.inspect_hierarchy().await;

    assert_data_tree!(
        hierarchy,
        root: contains {
            "executability-restrictions": "DoNotEnforce".to_string(),
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn package_cache_get() {
    async fn expect_and_return_inspect(env: &TestEnv, state: &'static str) -> DiagnosticsHierarchy {
        let hierarchy = env
            .wait_for_and_return_inspect_state(tree_assertion!(
                "root": contains {
                    "fuchsia.pkg.PackageCache": contains {
                        "get": contains {
                            "0": contains {
                                "state": state
                            }
                        }
                    }
                }
            ))
            .await;

        assert_data_tree!(
            &hierarchy,
            root: contains {
                "fuchsia.pkg.PackageCache": {
                    "get": {
                        "0" : contains {
                            "state": state,
                            "started-time": AnyProperty,
                            "meta-far-id":
                                "311ff6cebeff1e3d4c4dbe79c340caee13acea24ad69a20a7dd23b521b8db8c9",
                            "meta-far-length": 42u64,
                        }
                    }
                }
            }
        );

        hierarchy
    }

    async fn contains_missing_blob_stats(
        hierarchy: &DiagnosticsHierarchy,
        remaining: u64,
        open: u64,
        written: u64,
    ) {
        assert_data_tree!(
            hierarchy,
            root: contains {
                "fuchsia.pkg.PackageCache": {
                    "get": {
                        "0" : contains {
                            "known-remaining": remaining,
                            "open": open,
                            "written": written,
                        }
                    }
                }
            }
        );
    }

    let env = TestEnv::builder().build().await;
    let package = PackageBuilder::new("multi-pkg-a")
        .add_resource_at("bin/foo", "a-bin-foo".as_bytes())
        .add_resource_at("data/content", "a-data-content".as_bytes())
        .build()
        .await
        .unwrap();

    let meta_blob_info = BlobInfo { blob_id: BlobId::from(*package.hash()).into(), length: 42 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>();
    let (_dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let get_fut = env
        .proxies
        .package_cache
        .get(
            &meta_blob_info,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end,
            dir_server_end,
        )
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    // Request received, expect client requesting meta far.
    expect_and_return_inspect(&env, "need-meta-far").await;

    // Expect client fulfilling meta far.
    let (meta_far, contents) = package.contents();
    write_meta_far(&needed_blobs, meta_far).await;

    // Meta far done, expect client requesting missing blobs.
    expect_and_return_inspect(&env, "enumerate-missing-blobs").await;

    let missing_blobs = get_missing_blobs(&needed_blobs).await;

    // Missing blobs requested, expect client writing content blobs.
    let hierarchy = expect_and_return_inspect(&env, "need-content-blobs").await;
    contains_missing_blob_stats(&hierarchy, 2, 0, 0).await;

    let mut contents = contents
        .into_iter()
        .map(|(hash, bytes)| (BlobId::from(hash), bytes))
        .collect::<HashMap<_, Vec<u8>>>();

    let mut missing_blobs_iter = missing_blobs.into_iter();
    let blob = missing_blobs_iter.next().unwrap();

    let buf = contents.remove(&blob.blob_id.into()).unwrap();

    let content_blob = needed_blobs.open_blob(&blob.blob_id).await.unwrap().unwrap().unwrap();

    // Content blob open for writing.
    let hierarchy = expect_and_return_inspect(&env, "need-content-blobs").await;
    contains_missing_blob_stats(&hierarchy, 2, 1, 0).await;
    let () = compress_and_write_blob(&buf, *content_blob).await.unwrap();
    let () = blob_written(&needed_blobs, BlobId::from(blob.blob_id).into()).await;

    // Content blob written.
    let hierarchy = expect_and_return_inspect(&env, "need-content-blobs").await;
    contains_missing_blob_stats(&hierarchy, 1, 0, 1).await;

    let blob = missing_blobs_iter.next().unwrap();

    let buf = contents.remove(&blob.blob_id.into()).unwrap();

    let content_blob = needed_blobs.open_blob(&blob.blob_id).await.unwrap().unwrap().unwrap();

    // Last content blob open for writing.
    let hierarchy = expect_and_return_inspect(&env, "need-content-blobs").await;
    contains_missing_blob_stats(&hierarchy, 1, 1, 1).await;
    let () = compress_and_write_blob(&buf, *content_blob).await.unwrap();
    let () = blob_written(&needed_blobs, BlobId::from(blob.blob_id).into()).await;

    // Last content blob written.
    assert_eq!(contents, Default::default());
    assert_eq!(None, missing_blobs_iter.next());

    let () = get_fut.await.unwrap().unwrap();

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "fuchsia.pkg.PackageCache": {
                "get": {}
            }
        }
    );

    env.stop().await;
}

#[fuchsia::test]
async fn package_cache_concurrent_gets() {
    let package = PackageBuilder::new("a-blob").build().await.unwrap();
    let package2 = PackageBuilder::new("b-blob").build().await.unwrap();
    let env = TestEnv::builder().build().await;

    let blob_id = BlobId::from(*package.hash());
    let meta_blob_info = BlobInfo { blob_id: blob_id.into(), length: 42 };

    let (_needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>();
    let (_dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let _get_fut = env
        .proxies
        .package_cache
        .get(
            &meta_blob_info,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end,
            dir_server_end,
        )
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    // Initiate concurrent connection to `PackageCache`.
    let package_cache_proxy2: fpkg::PackageCacheProxy = env
        .apps
        .realm_instance
        .root
        .connect_to_protocol_at_exposed_dir()
        .expect("connect to package cache");

    let blob_id2 = BlobId::from(*package2.hash());
    let meta_blob_info2 = BlobInfo { blob_id: blob_id2.into(), length: 7 };
    let (_needed_blobs2, needed_blobs_server_end2) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>();
    let (_dir, dir_server_end2) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let _get_fut = package_cache_proxy2
        .get(
            &meta_blob_info2,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end2,
            dir_server_end2,
        )
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    let hierarchy = env
        .wait_for_and_return_inspect_state(tree_assertion!(
            "root": contains {
                "fuchsia.pkg.PackageCache": contains {
                    "get": contains {
                        "0": contains {
                            "state": AnyProperty,
                        },
                        "1": contains {
                            "state": AnyProperty,
                        }
                    }
                }
            }
        ))
        .await;
    assert_data_tree!(
        &hierarchy,
        root: contains {
            "fuchsia.pkg.PackageCache": {
                "get": {
                    "0" : {
                        "state": "need-meta-far",
                        "started-time": AnyProperty,
                        "meta-far-id": AnyProperty,
                        "meta-far-length": AnyProperty,
                        "gc-protection": "OpenPackageTracking",
                    },
                    "1" : {
                        "state": "need-meta-far",
                        "started-time": AnyProperty,
                        "meta-far-id": AnyProperty,
                        "meta-far-length": AnyProperty,
                        "gc-protection": "OpenPackageTracking",
                    }
                }
            }
        }
    );

    let values = ["0", "1"]
        .iter()
        .map(|i| {
            let node =
                hierarchy.get_child_by_path(&["fuchsia.pkg.PackageCache", "get", i]).unwrap();
            let length =
                node.get_property("meta-far-length").and_then(|property| property.uint()).unwrap();
            let hash =
                node.get_property("meta-far-id").and_then(|property| property.string()).unwrap();
            (hash, length)
        })
        .collect::<Vec<_>>();

    assert_matches!(
        values[..],
        [
            ("0e2fe90610179abbd7e648f0526419d48e226aabaef32e52d79b4cfa4187c880", 42u64),
            ("31ec29010d421b02992dff997a05786bd000aed369ffa77749c8629fdbeacf05", 7u64)
        ] | [
            ("31ec29010d421b02992dff997a05786bd000aed369ffa77749c8629fdbeacf05", 7u64),
            ("0e2fe90610179abbd7e648f0526419d48e226aabaef32e52d79b4cfa4187c880", 42u64)
        ]
    );
    env.stop().await;
}

#[fuchsia::test]
async fn retained_index_inital_state() {
    let env = TestEnv::builder().build().await;

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "index": contains {
                "retained" : {
                    "entries" : {},
                }
            }
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn retained_index_updated_and_persisted() {
    let env = TestEnv::builder().build().await;
    let packages = vec![
        PackageBuilder::new("pkg-a").build().await.unwrap(),
        PackageBuilder::new("multi-pkg-a")
            .add_resource_at("bin/foo", "a-bin-foo".as_bytes())
            .add_resource_at("data/content", "a-data-content".as_bytes())
            .build()
            .await
            .unwrap(),
    ];
    let blob_ids = packages.iter().map(|pkg| BlobId::from(*pkg.hash())).collect::<Vec<_>>();

    replace_retained_packages(&env.proxies.retained_packages, blob_ids.as_slice()).await;

    let meta_blob_info = BlobInfo { blob_id: blob_ids[0].into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>();
    let (_dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let get_fut = env
        .proxies
        .package_cache
        .get(
            &meta_blob_info,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end,
            dir_server_end,
        )
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "index": contains {
                "retained" : {
                    "entries" : {
                        blob_ids[0].to_string() => {
                            "state": "need-meta-far",
                        },
                        blob_ids[1].to_string() => {
                            "state": "need-meta-far",
                        }
                    },
                }
            }
        }
    );

    let (meta_far, _contents) = packages[0].contents();
    write_meta_far(&needed_blobs, meta_far).await;

    // Writing meta far triggers index update, however that's done concurrently
    // with the test, and there's no clear signal available through FIDL API.

    env.wait_for_and_return_inspect_state(tree_assertion!(
        root: contains {
            "index": contains {
                "retained" : {
                    "entries" : contains {
                        blob_ids[0].to_string() => {
                            "state": "known",
                            "blobs-count": 0u64
                        }
                    },
                }
            }
        }
    ))
    .await;

    let meta_blob_info2 = BlobInfo { blob_id: BlobId::from(*packages[1].hash()).into(), length: 0 };

    let (needed_blobs2, needed_blobs_server_end2) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>();
    let (_dir2, dir_server_end2) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let get_fut2 = env
        .proxies
        .package_cache
        .get(
            &meta_blob_info2,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end2,
            dir_server_end2,
        )
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    let (meta_far2, _contents2) = packages[1].contents();
    write_meta_far(&needed_blobs2, meta_far2).await;

    // Writing meta far triggers index update, however that's done concurrently
    // with the test, and there's no clear signal available through FIDL API.

    env.wait_for_and_return_inspect_state(tree_assertion!(
        root: contains {
            "index": contains {
                "retained" : {
                    "entries" : contains {
                        blob_ids[1].to_string() => {
                            "state": "known",
                            "blobs-count": 2u64
                        },
                    },
                }
            }
        }
    ))
    .await;

    drop(needed_blobs);
    drop(needed_blobs2);
    assert_matches!(get_fut.await.unwrap(), Err(_));
    assert_matches!(get_fut2.await.unwrap(), Err(_));

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "index": contains {
                "retained" : {
                    "entries" : {
                        blob_ids[0].to_string() => {
                            "state": "known",
                            "blobs-count": 0u64
                        },
                        blob_ids[1].to_string() => {
                            "state": "known",
                            "blobs-count": 2u64
                        },
                    },
                }
            }
        }
    );
    env.stop().await;
}

#[fuchsia::test]
async fn index_updated_mid_package_write() {
    let env = TestEnv::builder().build().await;
    let package = PackageBuilder::new("multi-pkg-a")
        .add_resource_at("bin/foo", "a-bin-foo".as_bytes())
        .add_resource_at("data/content", "a-data-content".as_bytes())
        .build()
        .await
        .unwrap();
    let blob_id = BlobId::from(*package.hash());
    let meta_blob_info = BlobInfo { blob_id: blob_id.into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<NeededBlobsMarker>();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let get_fut = env
        .proxies
        .package_cache
        .get(
            &meta_blob_info,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end,
            dir_server_end,
        )
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    let (meta_far, contents) = package.contents();
    write_meta_far(&needed_blobs, meta_far).await;

    replace_retained_packages(&env.proxies.retained_packages, &[blob_id]).await;

    // Writing meta far triggers index update, however that's done concurrently
    // with the test, and there's no clear signal available through FIDL API.

    env.wait_for_and_return_inspect_state(tree_assertion!(
        root: contains {
            "index": contains {
                "retained" : {
                    "entries" : {
                        blob_id.to_string() => {
                            "state": "known",
                            "blobs-count": 2u64,
                        },
                    },
                },
            }
        }
    ))
    .await;

    write_needed_blobs(&needed_blobs, contents).await;
    let () = get_fut.await.unwrap().unwrap();
    let () = package.verify_contents(&dir).await.unwrap();

    let hierarchy = env.inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "index": {
                "retained" : {
                    "entries" : {
                        blob_id.to_string() => {
                            "state": "known",
                            "blobs-count": 2u64,
                        },
                    },
                },
                "writing" : {},
            }
        }
    );
    env.stop().await;
}
