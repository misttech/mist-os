// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;
use test_case::test_case;

#[test_case("blobs"; "relative")]
#[test_case("./blobs"; "current dir relative")]
#[test_case("/blobs"; "root relative")]
#[test_case("//fuchsia.com/blobs"; "no scheme")]
#[test_case("https://fuchsia.com/blobs"; "absolute")]
#[fasync::run_singlethreaded(test)]
async fn packageless_update_with_relative_blob_base_url(blob_base_url: &str) {
    let content_blob = vec![1; 200];
    let content_blob_hash = fuchsia_merkle::from_slice(&content_blob).root();
    let zbi_content = b"zbi contents";
    let zbi_hash = fuchsia_merkle::from_slice(zbi_content).root();

    let env = TestEnv::builder()
        .ota_manifest(OtaManifestV1 {
            blob_base_url: blob_base_url.into(),
            images: vec![manifest::Image {
                fuchsia_merkle_root: zbi_hash,
                sha256: sha256(2),
                size: zbi_content.len() as u64,
                slot: manifest::Slot::AB,
                image_type: manifest::ImageType::Asset(AssetType::Zbi),
                delivery_blob_type: 1,
            }],
            ..make_manifest([manifest::Blob {
                uncompressed_size: content_blob.len() as u64,
                delivery_blob_type: 1,
                fuchsia_merkle_root: content_blob_hash,
            }])
        })
        .blob(content_blob_hash, content_blob)
        .blob(zbi_hash, zbi_content.to_vec())
        .build()
        .await;

    env.run_packageless_update().await.unwrap();

    env.assert_interactions(initial_interactions().chain([
        ReplaceRetainedBlobs(vec![zbi_hash.into(), content_blob_hash.into()]),
        Gc,
        Paver(PaverEvent::ReadAsset {
            configuration: paver::Configuration::B,
            asset: paver::Asset::Kernel,
        }),
        Paver(PaverEvent::ReadAsset {
            configuration: paver::Configuration::A,
            asset: paver::Asset::Kernel,
        }),
        OtaDownloader(OtaDownloaderEvent::FetchBlob(zbi_hash.into())),
        Paver(PaverEvent::WriteAsset {
            configuration: paver::Configuration::B,
            asset: paver::Asset::Kernel,
            payload: zbi_content.to_vec(),
        }),
        Paver(PaverEvent::DataSinkFlush),
        ReplaceRetainedBlobs(vec![content_blob_hash.into()]),
        Gc,
        OtaDownloader(OtaDownloaderEvent::FetchBlob(content_blob_hash.into())),
        BlobfsSync,
        Paver(PaverEvent::SetConfigurationActive { configuration: paver::Configuration::B }),
        Paver(PaverEvent::BootManagerFlush),
        Reboot,
    ]));
}
