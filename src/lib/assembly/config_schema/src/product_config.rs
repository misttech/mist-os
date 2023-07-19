// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_package_utils::{PackageInternalPathBuf, PackageManifestPathBuf, SourcePathBuf};
use serde::{Deserialize, Serialize};

use crate::common::DriverDetails;

/// The Product-provided configuration details.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductConfig {
    #[serde(default)]
    pub packages: ProductPackagesConfig,

    /// List of base drivers to include in the product.
    #[serde(default)]
    pub base_drivers: Vec<DriverDetails>,

    /// Start URL to pass to `session_manager`.
    ///
    /// Default to the empty string which creates a "paused" config that launches nothing to start.
    #[serde(default)]
    pub session_url: String,

    /// Generic product information.
    #[serde(default)]
    pub info: Option<ProductInfoConfig>,
}

/// Packages provided by the product, to add to the assembled images.
///
/// This also includes configuration for those packages:
///
/// ```json5
///   packages: {
///     base: [
///       {
///         manifest: "path/to/package_a/package_manifest.json",
///       },
///       {
///         manifest: "path/to/package_b/package_manifest.json",
///         config_data: {
///           "foo.cfg": "path/to/some/source/file/foo.cfg",
///           "bar/more/data.json": "path/to/some.json",
///         },
///       },
///     ],
///     cache: []
///   }
/// ```
///
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductPackagesConfig {
    /// Paths to package manifests, or more detailed json entries for packages
    /// to add to the 'base' package set.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub base: Vec<ProductPackageDetails>,

    /// Paths to package manifests, or more detailed json entries for packages
    /// to add to the 'cache' package set.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cache: Vec<ProductPackageDetails>,
}

/// Describes in more detail a package to add to the assembly.
#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductPackageDetails {
    /// Path to the package manifest for this package.
    pub manifest: PackageManifestPathBuf,

    /// Map of config_data entries for this package, from the destination path
    /// within the package, to the path where the source file is to be found.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub config_data: Vec<ProductConfigData>,
}

impl From<PackageManifestPathBuf> for ProductPackageDetails {
    fn from(manifest: PackageManifestPathBuf) -> Self {
        Self { manifest, config_data: Vec::default() }
    }
}

impl From<&str> for ProductPackageDetails {
    fn from(s: &str) -> Self {
        ProductPackageDetails { manifest: s.into(), config_data: Vec::default() }
    }
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductConfigData {
    /// Path to the config file on the host.
    pub source: SourcePathBuf,

    /// Path to find the file in the package on the target.
    pub destination: PackageInternalPathBuf,
}

/// Configuration options for product info.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProductInfoConfig {
    /// Name of the product.
    pub name: String,
    /// Model of the product.
    pub model: String,
    /// Manufacturer of the product.
    pub manufacturer: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_util as util;

    #[test]
    fn test_product_provided_config_data() {
        let json5 = r#"
            {
                base: [
                    {
                        manifest: "path/to/base/package_manifest.json"
                    },
                    {
                        manifest: "some/other/manifest.json",
                        config_data: [
                            {
                                destination: "dest/path/cfg.txt",
                                source: "source/path/cfg.txt",
                            },
                            {
                                destination: "other_data.json",
                                source: "source_other_data.json",
                            },
                        ]
                    }
                  ],
                cache: [
                    {
                        manifest: "path/to/cache/package_manifest.json"
                    }
                ]
            }
        "#;

        let mut cursor = std::io::Cursor::new(json5);
        let packages: ProductPackagesConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            packages.base,
            vec![
                ProductPackageDetails::from("path/to/base/package_manifest.json"),
                ProductPackageDetails {
                    manifest: "some/other/manifest.json".into(),
                    config_data: vec![
                        ProductConfigData {
                            destination: "dest/path/cfg.txt".into(),
                            source: "source/path/cfg.txt".into(),
                        },
                        ProductConfigData {
                            destination: "other_data.json".into(),
                            source: "source_other_data.json".into(),
                        },
                    ]
                }
            ]
        );
        assert_eq!(packages.cache, vec!["path/to/cache/package_manifest.json".into()]);
    }

    #[test]
    fn product_package_details_deserialization() {
        let json5 = r#"
            {
                manifest: "some/other/manifest.json",
                config_data: [
                    {
                        destination: "dest/path/cfg.txt",
                        source: "source/path/cfg.txt",
                    },
                    {
                        destination: "other_data.json",
                        source: "source_other_data.json",
                    },
                ]
            }
        "#;
        let expected = ProductPackageDetails {
            manifest: "some/other/manifest.json".into(),
            config_data: vec![
                ProductConfigData {
                    destination: "dest/path/cfg.txt".into(),
                    source: "source/path/cfg.txt".into(),
                },
                ProductConfigData {
                    destination: "other_data.json".into(),
                    source: "source_other_data.json".into(),
                },
            ],
        };
        let mut cursor = std::io::Cursor::new(json5);
        let details: ProductPackageDetails = util::from_reader(&mut cursor).unwrap();
        assert_eq!(details, expected);
    }

    #[test]
    fn product_package_details_serialization() {
        let entries = vec![
            ProductPackageDetails {
                manifest: "path/to/manifest.json".into(),
                config_data: Vec::default(),
            },
            ProductPackageDetails {
                manifest: "another/path/to/a/manifest.json".into(),
                config_data: vec![
                    ProductConfigData {
                        destination: "dest/path/A".into(),
                        source: "source/path/A".into(),
                    },
                    ProductConfigData {
                        destination: "dest/path/B".into(),
                        source: "source/path/B".into(),
                    },
                ],
            },
        ];
        let serialized = serde_json::to_value(entries).unwrap();
        let expected = serde_json::json!(
            [
                {
                    "manifest": "path/to/manifest.json"
                },
                {
                    "manifest": "another/path/to/a/manifest.json",
                    "config_data": [
                        {
                            "destination": "dest/path/A",
                            "source": "source/path/A",
                        },
                        {
                            "destination": "dest/path/B",
                            "source": "source/path/B",
                        },
                    ]
                }
            ]
        );
        assert_eq!(serialized, expected);
    }
}
