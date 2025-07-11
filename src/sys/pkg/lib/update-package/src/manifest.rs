// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Structs for parsing an OTA manifest.

use crate::images::AssetType;
use crate::SystemVersion;
use serde::{Deserialize, Serialize};

/// Returns structured OTA manifest data based on raw file contents.
pub fn parse_ota_manifest(contents: &[u8]) -> Result<OtaManifestV1, OtaManifestError> {
    let manifest: VersionedOtaManifest =
        serde_json::from_slice(contents).map_err(OtaManifestError::Parse)?;

    manifest.version1.ok_or(OtaManifestError::NoSupportedVersion)
}

/// An error encountered while parsing the OTA manifest.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum OtaManifestError {
    #[error("while parsing json")]
    Parse(#[source] serde_json::error::Error),

    #[error("no supported version found")]
    NoSupportedVersion,
}

/// The versioned manifest, can support multiple versions in the same manifest.
#[derive(Serialize, Deserialize, Debug)]
pub struct VersionedOtaManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    version1: Option<OtaManifestV1>,
}

/// Information about a particular version of the OS.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct OtaManifestV1 {
    /// The version of the target build.
    pub build_version: SystemVersion,
    /// The board this OTA is for, must match build-info/board.
    pub board: String,
    /// The epoch of this OTA.
    pub epoch: u64,
    /// The base URL of the blobs, the final URL for each blob will be
    /// "{blob_base_url}/{delivery_blob_type}/{fuchsia_merkle_root}".
    pub blob_base_url: url::Url,
    /// The images for this version.
    pub images: Vec<Image>,
    /// The blobs for this version.
    pub blobs: Vec<Blob>,
}

/// The slot of an image.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
pub enum Slot {
    /// The A or B slot.
    #[default]
    AB,
    /// The recovery slot in ABR.
    R,
}

/// An image to be written to a partition.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Image {
    /// The slot of the image.
    pub slot: Slot,
    /// The type of the image.
    #[serde(flatten)]
    pub image_type: ImageType,
    /// The sha256 hash of the image.
    pub sha256: fuchsia_hash::Sha256,
    /// The size of the image in bytes.
    pub size: u64,
    /// The delivery blob type.
    pub delivery_blob_type: u32,
    /// The fuchsia merkle root of the image.
    pub fuchsia_merkle_root: fuchsia_hash::Hash,
}

/// The type of the image, asset or firmware.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageType {
    /// ZBI or VbMeta.
    Asset(AssetType),
    /// Other A/B partitions.
    Firmware(String),
}

/// A content blob.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Blob {
    /// The uncompressed size of the blob.
    pub uncompressed_size: u64,
    /// The delivery blob type.
    pub delivery_blob_type: u32,
    /// The fuchsia merkle root of the uncompressed blob.
    pub fuchsia_merkle_root: fuchsia_hash::Hash,
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use std::str::FromStr;

    #[test]
    fn test_parse_ota_manifest_success() {
        let json = serde_json::json!({
            "version1": {
                "build_version": "1.2.3.4",
                "board": "test-board",
                "epoch": 1,
                "blob_base_url": "http://example.com",
                "images": [
                    {
                        "slot": "AB",
                        "asset": "zbi",
                        "sha256": "00".repeat(32),
                        "size": 1234,
                        "delivery_blob_type": 1,
                        "fuchsia_merkle_root": "01".repeat(32),
                    },
                    {
                        "slot": "AB",
                        "firmware": "bootloader",
                        "sha256": "02".repeat(32),
                        "size": 3456,
                        "delivery_blob_type": 1,
                        "fuchsia_merkle_root": "03".repeat(32),
                    },
                ],
                "blobs": [
                    {
                        "uncompressed_size": 5678,
                        "delivery_blob_type": 2,
                        "fuchsia_merkle_root": "04".repeat(32),
                    }
                ]
            }
        });
        let manifest = parse_ota_manifest(json.to_string().as_bytes()).unwrap();
        assert_eq!(manifest.build_version, SystemVersion::from_str("1.2.3.4").unwrap());
        assert_eq!(manifest.board, "test-board");
        assert_eq!(manifest.epoch, 1);
        assert_eq!(manifest.blob_base_url.as_str(), "http://example.com/");

        assert_eq!(manifest.images.len(), 2);
        assert_eq!(manifest.images[0].slot, Slot::AB);
        assert_eq!(manifest.images[0].image_type, ImageType::Asset(AssetType::Zbi));
        assert_eq!(manifest.images[0].sha256, [0; 32].into());
        assert_eq!(manifest.images[0].size, 1234);
        assert_eq!(manifest.images[0].delivery_blob_type, 1);
        assert_eq!(manifest.images[0].fuchsia_merkle_root, [1; 32].into());

        assert_eq!(manifest.images[1].slot, Slot::AB);
        assert_eq!(manifest.images[1].image_type, ImageType::Firmware("bootloader".to_string()));
        assert_eq!(manifest.images[1].sha256, [2; 32].into());
        assert_eq!(manifest.images[1].size, 3456);
        assert_eq!(manifest.images[1].delivery_blob_type, 1);
        assert_eq!(manifest.images[1].fuchsia_merkle_root, [3; 32].into());

        assert_eq!(manifest.blobs.len(), 1);
        assert_eq!(manifest.blobs[0].uncompressed_size, 5678);
        assert_eq!(manifest.blobs[0].delivery_blob_type, 2);
        assert_eq!(manifest.blobs[0].fuchsia_merkle_root, [4; 32].into());
    }

    #[test]
    fn test_serialize_ota_manifest() {
        let manifest = OtaManifestV1 {
            build_version: SystemVersion::from_str("1.2.3.4").unwrap(),
            board: "test-board".to_string(),
            epoch: 1,
            blob_base_url: "http://example.com".parse().unwrap(),
            images: vec![
                Image {
                    slot: Slot::AB,
                    image_type: ImageType::Asset(AssetType::Zbi),
                    sha256: [0; 32].into(),
                    size: 1234,
                    delivery_blob_type: 1,
                    fuchsia_merkle_root: [1; 32].into(),
                },
                Image {
                    slot: Slot::AB,
                    image_type: ImageType::Firmware("bootloader".to_string()),
                    sha256: [2; 32].into(),
                    size: 3456,
                    delivery_blob_type: 1,
                    fuchsia_merkle_root: [3; 32].into(),
                },
            ],
            blobs: vec![Blob {
                uncompressed_size: 5678,
                delivery_blob_type: 2,
                fuchsia_merkle_root: [4; 32].into(),
            }],
        };

        let versioned_manifest = VersionedOtaManifest { version1: Some(manifest) };

        let json = serde_json::to_value(versioned_manifest).unwrap();
        let expected = serde_json::json!({
            "version1": {
                "build_version": "1.2.3.4",
                "board": "test-board",
                "epoch": 1,
                "blob_base_url": "http://example.com/",
                "images": [
                    {
                        "slot": "AB",
                        "asset": "zbi",
                        "sha256": "00".repeat(32),
                        "size": 1234,
                        "delivery_blob_type": 1,
                        "fuchsia_merkle_root": "01".repeat(32),
                    },
                    {
                        "slot": "AB",
                        "firmware": "bootloader",
                        "sha256": "02".repeat(32),
                        "size": 3456,
                        "delivery_blob_type": 1,
                        "fuchsia_merkle_root": "03".repeat(32),
                    },
                ],
                "blobs": [
                    {
                        "uncompressed_size": 5678,
                        "delivery_blob_type": 2,
                        "fuchsia_merkle_root": "04".repeat(32),
                    }
                ]
            }
        });
        assert_eq!(json, expected);
    }

    #[test]
    fn test_parse_ota_manifest_no_supported_version() {
        let json = serde_json::json!({
            "version2": {}
        });
        let err = parse_ota_manifest(json.to_string().as_bytes()).unwrap_err();
        assert_matches!(err, OtaManifestError::NoSupportedVersion);
    }

    #[test]
    fn test_parse_ota_manifest_invalid_json() {
        let err = parse_ota_manifest(b"invalid json").unwrap_err();
        assert_matches!(err, OtaManifestError::Parse(_));
    }
}
