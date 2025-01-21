// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeMap;

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use assembly_package_utils::{PackageInternalPathBuf, PackageManifestPathBuf};
use camino::{Utf8Path, Utf8PathBuf};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::DriverDetails;

/// The Product-provided configuration details.
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema, SupportsFileRelativePaths)]
#[serde(default, deny_unknown_fields)]
pub struct ProductConfig {
    #[file_relative_paths]
    pub packages: ProductPackagesConfig,

    /// List of base drivers to include in the product.
    pub base_drivers: Vec<DriverDetails>,

    /// Product-specific session information.
    ///
    /// Default to None which creates a "paused" config that launches nothing to start.
    pub session: Option<ProductSessionConfig>,

    /// Generic product information.
    pub info: Option<ProductInfoConfig>,

    /// The file paths to various build information.
    #[file_relative_paths]
    pub build_info: Option<BuildInfoConfig>,

    /// The policy given to component_manager that restricts where sensitive capabilities can be
    /// routed.
    #[file_relative_paths]
    pub component_policy: ComponentPolicyConfig,

    /// Components which depend on trusted applications running in the TEE.
    pub tee_clients: Vec<TeeClient>,

    /// Components which should run as trusted applications in Fuchsia.
    pub trusted_apps: Vec<TrustedApp>,
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
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema, SupportsFileRelativePaths)]
#[serde(default, deny_unknown_fields)]
pub struct ProductPackagesConfig {
    /// Paths to package manifests, or more detailed json entries for packages
    /// to add to the 'base' package set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[file_relative_paths]
    pub base: Vec<ProductPackageDetails>,

    /// Paths to package manifests, or more detailed json entries for packages
    /// to add to the 'cache' package set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[file_relative_paths]
    pub cache: Vec<ProductPackageDetails>,
}

/// Describes in more detail a package to add to the assembly.
#[derive(Debug, PartialEq, Deserialize, Serialize, JsonSchema, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct ProductPackageDetails {
    /// Path to the package manifest for this package.
    pub manifest: FileRelativePathBuf,

    /// Map of config_data entries for this package, from the destination path
    /// within the package, to the path where the source file is to be found.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub config_data: Vec<ProductConfigData>,
}

impl From<PackageManifestPathBuf> for ProductPackageDetails {
    fn from(manifest: PackageManifestPathBuf) -> Self {
        let manifestpath: &Utf8Path = manifest.as_ref();
        let path: Utf8PathBuf = manifestpath.into();
        Self { manifest: FileRelativePathBuf::Resolved(path), config_data: Vec::default() }
    }
}

impl From<&str> for ProductPackageDetails {
    fn from(s: &str) -> Self {
        ProductPackageDetails { manifest: s.into(), config_data: Vec::default() }
    }
}

#[derive(Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductConfigData {
    /// Path to the config file on the host.
    pub source: FileRelativePathBuf,

    /// Path to find the file in the package on the target.
    pub destination: PackageInternalPathBuf,
}

/// Configuration options for product info.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductInfoConfig {
    /// Name of the product.
    pub name: String,
    /// Model of the product.
    pub model: String,
    /// Manufacturer of the product.
    pub manufacturer: String,
}

/// Configuration options for build info.
#[derive(
    Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(deny_unknown_fields)]
pub struct BuildInfoConfig {
    /// Name of the product build target.
    pub name: String,
    /// Path to the version file.
    pub version: FileRelativePathBuf,
    /// Path to the jiri snapshot.
    pub jiri_snapshot: FileRelativePathBuf,
    /// Path to the latest commit date.
    pub latest_commit_date: FileRelativePathBuf,
}

/// Configuration options for the component policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, SupportsFileRelativePaths)]
#[serde(default, deny_unknown_fields)]
pub struct ComponentPolicyConfig {
    /// The file paths to a product-provided component policies.
    #[file_relative_paths]
    pub product_policies: Vec<FileRelativePathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct TeeClientFeatures {
    /// Whether this component needs /dev-class/securemem routed to it. If true, the securemem
    //// directory will be routed as dev-securemem.
    pub securemem: bool,
    /// Whether this component requires persistent storage, routed as /data.
    pub persistent_storage: bool,
    /// Whether this component requires tmp storage, routed as /tmp.
    pub tmp_storage: bool,
}

/// A configuration for a component which depends on TEE-based protocols.
/// Examples include components which implement DRM, or authentication services.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct TeeClient {
    /// The URL of the component.
    pub component_url: String,
    /// GUIDs which of the form fuchsia.tee.Application.{GUID} will match a
    /// protocol provided by the TEE.
    #[serde(default)]
    pub guids: Vec<String>,
    /// Capabilities provided by this component which should be routed to the
    /// rest of the system.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Additional protocols which are required for this component to work, and
    /// which will be routed from 'parent'
    #[serde(default)]
    pub additional_required_protocols: Vec<String>,
    /// Config data files required for this component to work, and which will be inserted into
    /// config data for this package (with a package name based on the component URL)
    #[serde(default)]
    pub config_data: Option<BTreeMap<String, String>>,
    /// Additional features required for the component to function.
    #[serde(default)]
    pub additional_required_features: TeeClientFeatures,
}

/// Configuration for how to run a trusted application in Fuchsia.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct TrustedApp {
    /// The URL of the component.
    pub component_url: String,
    /// The GUID that identifies this trusted app for clients.
    pub guid: String,
}

/// Product configuration options for the session:
///
/// ```json5
///   session: {
///     url: "fuchsia-pkg://fuchsia.com/my_session#meta/my_session.cm",
///     initial_element: {
///         collection: "elements",
///         url: "fuchsia-pkg://fuchsia.com/my_component#meta/my_component.cm"
///         view_id_annotation: "my_component"
///     }
///   }
/// ```
///
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductSessionConfig {
    /// Start URL to pass to `session_manager`.
    pub url: String,

    /// Specifies initial element properties for the window manager.
    #[serde(default)]
    pub initial_element: Option<InitialElement>,
}

/// Platform configuration options for the window manager.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitialElement {
    /// Specifies the collection in which the window manager should launch the
    /// initial element, if one is given by `url`. Defaults to "elements".
    #[serde(default = "collection_default")]
    pub collection: String,

    /// Specifies the Fuchsia package URL of the element the window manager
    /// should launch on startup, if one is given.
    pub url: String,

    /// Specifies the annotation value by which the window manager can identify
    /// a view presented by the element it launched on startup, if one is given
    /// by `url`.
    pub view_id_annotation: String,
}

fn collection_default() -> String {
    String::from("elements")
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
                ProductPackageDetails {
                    manifest: FileRelativePathBuf::FileRelative(
                        "path/to/base/package_manifest.json".into()
                    ),
                    config_data: Vec::default()
                },
                ProductPackageDetails {
                    manifest: FileRelativePathBuf::FileRelative("some/other/manifest.json".into()),
                    config_data: vec![
                        ProductConfigData {
                            destination: "dest/path/cfg.txt".into(),
                            source: FileRelativePathBuf::FileRelative("source/path/cfg.txt".into()),
                        },
                        ProductConfigData {
                            destination: "other_data.json".into(),
                            source: FileRelativePathBuf::FileRelative(
                                "source_other_data.json".into()
                            ),
                        },
                    ]
                }
            ]
        );
        assert_eq!(
            packages.cache,
            vec![ProductPackageDetails {
                manifest: FileRelativePathBuf::FileRelative(
                    "path/to/cache/package_manifest.json".into()
                ),
                config_data: Vec::default()
            }]
        );
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
            manifest: FileRelativePathBuf::FileRelative("some/other/manifest.json".into()),
            config_data: vec![
                ProductConfigData {
                    destination: "dest/path/cfg.txt".into(),
                    source: FileRelativePathBuf::FileRelative("source/path/cfg.txt".into()),
                },
                ProductConfigData {
                    destination: "other_data.json".into(),
                    source: FileRelativePathBuf::FileRelative("source_other_data.json".into()),
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
