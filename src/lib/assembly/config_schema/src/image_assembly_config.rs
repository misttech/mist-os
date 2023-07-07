// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use assembly_util as util;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::common::FileEntry;

/// The set of information that defines a fuchsia product.  All fields are
/// optional to allow for specifying incomplete configurations.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialImageAssemblyConfig {
    /// The packages whose files get added to the base package. The
    /// packages themselves are not added, but their individual files are
    /// extracted and added to the base package. These files are needed
    /// to bootstrap pkgfs.
    #[serde(default)]
    pub system: Vec<Utf8PathBuf>,

    /// The packages that are in the base package list, excluding drivers,
    /// which are to be added to the base package (data/static_packages).
    /// These packages get updated by flashing and OTAing, and cannot
    /// be garbage collected.
    #[serde(default)]
    pub base: Vec<Utf8PathBuf>,

    /// The packages that are in the cache package list, which is added
    /// to the base package (data/cache_packages). These packages get
    /// updated by flashing and OTAing, but can be garbage collected.
    #[serde(default)]
    pub cache: Vec<Utf8PathBuf>,

    /// The parameters that specify which kernel to put into the ZBI.
    pub kernel: Option<PartialKernelConfig>,

    /// The qemu kernel to use when starting the emulator.
    #[serde(default)]
    pub qemu_kernel: Option<Utf8PathBuf>,

    /// The list of additional boot args to add.
    #[serde(default)]
    pub boot_args: Vec<String>,

    /// The packages that are in the bootfs package list, which are
    /// added to the BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_packages: Vec<Utf8PathBuf>,

    /// The set of files to be placed in BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_files: Vec<FileEntry>,
}

impl PartialImageAssemblyConfig {
    /// Create a new PartialImageAssemblyConfig by merging N PartialImageAssemblyConfigs.
    /// At most one can specify a kernel path, or a clock backstop.
    ///
    /// Packages in the base, bootfs, and cache sets are deduplicated, as are any boot
    /// arguments, kernel args, or packages used to provide files for the system
    /// itself.
    ///
    /// bootfs entries are merged, and any entries with duplicate destination
    /// paths will cause an error.
    pub fn try_from_partials<I: IntoIterator<Item = PartialImageAssemblyConfig>>(
        configs: I,
    ) -> Result<Self> {
        let mut system = BTreeSet::new();
        let mut base = BTreeSet::new();
        let mut cache = BTreeSet::new();
        let mut boot_args = BTreeSet::new();
        let mut bootfs_files = BTreeMap::new();
        let mut bootfs_packages = BTreeSet::new();

        let mut kernel_path = None;
        let mut kernel_args = Vec::new();
        let mut kernel_clock_backstop = None;
        let mut qemu_kernel = None;

        for config in configs.into_iter() {
            system.extend(config.system);
            base.extend(config.base);
            cache.extend(config.cache);
            bootfs_packages.extend(config.bootfs_packages);
            boot_args.extend(config.boot_args);

            for entry in config.bootfs_files {
                add_bootfs_file(&mut bootfs_files, entry)?;
            }

            if let Some(PartialKernelConfig { path, mut args, clock_backstop }) = config.kernel {
                util::set_option_once_or(
                    &mut kernel_path,
                    path,
                    anyhow!("Only one product configuration can specify a kernel path"),
                )?;
                kernel_args.append(&mut args);

                util::set_option_once_or(
                    &mut kernel_clock_backstop,
                    clock_backstop,
                    anyhow!("Only one product configuration can specify a backstop time"),
                )?;
            }

            util::set_option_once_or(
                &mut qemu_kernel,
                config.qemu_kernel,
                anyhow!("Only one product configuration can specify a qemu kernel path"),
            )?;
        }

        Ok(Self {
            system: system.into_iter().collect(),
            base: base.into_iter().collect(),
            cache: cache.into_iter().collect(),
            kernel: Some(PartialKernelConfig {
                path: kernel_path,
                args: kernel_args,
                clock_backstop: kernel_clock_backstop,
            }),
            qemu_kernel,
            boot_args: boot_args.into_iter().collect(),
            bootfs_files: bootfs_files.into_values().collect(),
            bootfs_packages: bootfs_packages.into_iter().collect(),
        })
    }
}

/// The set of information that defines a fuchsia product.  This is capable of
/// being a complete configuration (it at least has a kernel).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageAssemblyConfig {
    /// The packages whose files get added to the base package. The
    /// packages themselves are not added, but their individual files are
    /// extracted and added to the base package. These files are needed
    /// to bootstrap pkgfs.
    #[serde(default)]
    pub system: Vec<Utf8PathBuf>,

    /// The packages that are in the base package list, excluding drivers
    /// which are added to the base package (data/static_packages).
    /// These packages get updated by flashing and OTAing, and cannot
    /// be garbage collected.
    #[serde(default)]
    pub base: Vec<Utf8PathBuf>,

    /// The packages that are in the cache package list, which is added
    /// to the base package (data/cache_packages). These packages get
    /// updated by flashing and OTAing, but can be garbage collected.
    #[serde(default)]
    pub cache: Vec<Utf8PathBuf>,

    /// The parameters that specify which kernel to put into the ZBI.
    pub kernel: KernelConfig,

    /// The qemu kernel to use when starting the emulator.
    pub qemu_kernel: Utf8PathBuf,

    /// The list of additional boot args to add.
    #[serde(default)]
    pub boot_args: Vec<String>,

    /// The set of files to be placed in BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_files: Vec<FileEntry>,

    /// The packages that are in the bootfs package list, which are
    /// added to the BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_packages: Vec<Utf8PathBuf>,
}

impl ImageAssemblyConfig {
    /// Helper function for constructing a ImageAssemblyConfig in tests in this
    /// and other modules within the crate.
    pub fn new_for_testing(kernel_path: impl AsRef<Utf8Path>, clock_backstop: u64) -> Self {
        Self {
            system: Vec::default(),
            base: Vec::default(),
            cache: Vec::default(),
            boot_args: Vec::default(),
            bootfs_files: Vec::default(),
            bootfs_packages: Vec::default(),
            kernel: KernelConfig {
                path: kernel_path.as_ref().into(),
                args: Vec::default(),
                clock_backstop,
            },
            qemu_kernel: "path/to/qemu/kernel".into(),
        }
    }

    /// Create a new ImageAssemblyConfig by merging N PartialImageAssemblyConfigs, only one
    /// of which must specify the kernel path and the clock_backstop.
    ///
    /// Packages in the base and cache sets are deduplicated, as are any boot
    /// arguments, kernel args, or packages used to provide files for the system
    /// itself.
    ///
    /// bootfs entries are merged, and any entries with duplicate destination
    /// paths will cause an error.
    pub fn try_from_partials<I: IntoIterator<Item = PartialImageAssemblyConfig>>(
        configs: I,
    ) -> Result<Self> {
        let PartialImageAssemblyConfig {
            system,
            base,
            cache,
            kernel,
            qemu_kernel,
            boot_args,
            bootfs_files,
            bootfs_packages,
        } = PartialImageAssemblyConfig::try_from_partials(configs)?;

        let PartialKernelConfig { path: kernel_path, args: cmdline_args, clock_backstop } =
            kernel.context("A kernel configuration must be specified")?;

        let kernel_path = kernel_path.context("No product configurations specify a kernel")?;
        let clock_backstop =
            clock_backstop.context("No product configurations specify a clock backstop time")?;

        let qemu_kernel = qemu_kernel.context("A qemu kernel configuration must be specified")?;

        Ok(Self {
            system,
            base,
            cache,
            kernel: KernelConfig { path: kernel_path, args: cmdline_args, clock_backstop },
            qemu_kernel,
            boot_args,
            bootfs_files,
            bootfs_packages,
        })
    }
}

/// Attempt to add the given entry to the map of bootfs entries.
/// Returns an error if it duplicates an existing entry.
fn add_bootfs_file(
    bootfs_entries: &mut BTreeMap<String, FileEntry>,
    entry: FileEntry,
) -> Result<()> {
    if let Some(existing_entry) = bootfs_entries.get(&entry.destination) {
        if existing_entry.source != entry.source {
            return Err(anyhow!(format!(
                "Found a duplicate bootfs entry for destination: {}, with sources:\n{}\n{}",
                entry.destination, entry.source, existing_entry.source
            )));
        }
    } else {
        bootfs_entries.insert(entry.destination.clone(), entry);
    }
    Ok(())
}

/// The information required to specify a kernel and its arguments, all optional
/// to allow for the partial specification
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialKernelConfig {
    /// The path to the prebuilt kernel.
    pub path: Option<Utf8PathBuf>,

    /// The list of command line arguments to pass to the kernel on startup.
    #[serde(default)]
    pub args: Vec<String>,

    /// The backstop UTC time for the clock.
    pub clock_backstop: Option<u64>,
}

/// The information required to specify a kernel and its arguments.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KernelConfig {
    /// The path to the prebuilt kernel.
    pub path: Utf8PathBuf,

    /// The list of command line arguments to pass to the kernel on startup.
    #[serde(default)]
    pub args: Vec<String>,

    /// The backstop UTC time for the clock.
    /// This is kept separate from the `args` to make it clear that this is a required argument.
    pub clock_backstop: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_util::from_reader;
    use assert_matches::assert_matches;
    use serde_json::json;

    fn from_json_str<T>(json: &str) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut cursor = std::io::Cursor::new(json);
        from_reader(&mut cursor)
    }

    #[test]
    fn product_from_json_file() {
        let json = r#"
            {
              "system": ["package0"],
              "base": ["package1", "package2"],
              "cache": ["package3", "package4"],
              "kernel": {
                "path": "path/to/kernel",
                "args": ["arg1", "arg2"],
                "clock_backstop": 0
              },
              "qemu_kernel": "path/to/qemu/kernel",
              "bootfs_files": [
                {
                    "source": "path/to/source",
                    "destination": "path/to/destination"
                }
              ],
              "bootfs_packages": ["package5", "package6"]
            }
        "#;

        let config: PartialImageAssemblyConfig = from_json_str(json).unwrap();
        assert_matches!(
            config.kernel,
            Some(PartialKernelConfig { path: Some(_), args: _, clock_backstop: Some(0) })
        );
    }

    #[test]
    fn product_from_json5_file() {
        let json = r#"
            {
              // json5 files can have comments in them.
              system: ["package0"],
              base: ["package1", "package2"],
              cache: ["package3", "package4"],
              kernel: {
                path: "path/to/kernel",
                args: ["arg1", "arg2"],
                clock_backstop: 0,
              },
              qemu_kernel: "path/to/qemu/kernel",
              // and lists can have trailing commas
              boot_args: ["arg1", "arg2", ],
              bootfs_files: [
                {
                    source: "path/to/source",
                    destination: "path/to/destination",
                }
              ],
              bootfs_packages: ["package5", "package6"],
            }
        "#;

        let config: PartialImageAssemblyConfig = from_json_str(json).unwrap();
        assert_matches!(
            config.kernel,
            Some(PartialKernelConfig { path: Some(_), args: _, clock_backstop: Some(0) })
        );
    }

    #[test]
    fn product_from_minimal_json_file() {
        let json = r#"
            {
              "kernel": {
                "path": "path/to/kernel",
                "clock_backstop": 0
              }
            }
        "#;

        let config: PartialImageAssemblyConfig = from_json_str(json).unwrap();
        assert_matches!(
            config.kernel,
            Some(PartialKernelConfig { path: Some(_), args: _, clock_backstop: Some(0) })
        );
    }

    #[test]
    fn product_from_invalid_json_file() {
        let json = r#"
            {
                "invalid": "data"
            }
        "#;

        let config: Result<ImageAssemblyConfig> = from_json_str(json);
        assert!(config.is_err());
    }

    #[test]
    fn merge_product_config() {
        let config_a = serde_json::from_value::<PartialImageAssemblyConfig>(json!({
            "system": ["package0a"],
            "base": ["package1a", "package2a"],
            "cache": ["package3a", "package4a"],
            "kernel": {
                "path": "path/to/kernel",
                "args": ["arg10", "arg20"],
                "clock_backstop": 0
            },
            "qemu_kernel": "path/to/qemu/kernel",
            "bootfs_files": [
                {
                    "source": "path/to/source/a",
                    "destination": "path/to/destination/a"
                }
            ],
            "bootfs_packages": ["package5a", "package6a"],
            "boot_args": [ "arg1a", "arg2a" ]
        }))
        .unwrap();

        let config_b = serde_json::from_value::<PartialImageAssemblyConfig>(json!({
          "system": ["package0b"],
          "base": ["package1a", "package2b"],
          "cache": ["package3b", "package4b"],
          "bootfs_files": [
              {
                  "source": "path/to/source/b",
                  "destination": "path/to/destination/b"
              }
          ],
          "bootfs_packages": ["package5b", "package6b"],
          "boot_args": [ "arg1b", "arg2b" ]
        }))
        .unwrap();

        let config_c = serde_json::from_value::<PartialImageAssemblyConfig>(json!({
          "system": ["package0c"],
          "base": ["package1a", "package2c"],
          "cache": ["package3c", "package4c"],
          "bootfs_files": [
              {
                  "source": "path/to/source/c",
                  "destination": "path/to/destination/c"
              }
          ],
          "bootfs_packages": ["package5c", "package6c"],
          "boot_args": [ "arg1c", "arg2c" ]
        }))
        .unwrap();

        let result =
            ImageAssemblyConfig::try_from_partials(vec![config_a, config_b, config_c]).unwrap();

        let expected = serde_json::from_value::<ImageAssemblyConfig>(json!({
            "system": ["package0a", "package0b", "package0c"],
            "base": ["package1a", "package2a", "package2b", "package2c"],
            "cache": ["package3a", "package3b", "package3c", "package4a", "package4b", "package4c"],
            "kernel": {
                "path": "path/to/kernel",
                "args": ["arg10", "arg20"],
                "clock_backstop": 0
            },
            "qemu_kernel": "path/to/qemu/kernel",
            "bootfs_files": [
                {
                    "source": "path/to/source/a",
                    "destination": "path/to/destination/a"
                },
                {
                    "source": "path/to/source/b",
                    "destination": "path/to/destination/b"
                },
                {
                    "source": "path/to/source/c",
                    "destination": "path/to/destination/c"
                },
            ],
            "bootfs_packages": [
                "package5a", "package5b", "package5c", "package6a", "package6b", "package6c"],
            "boot_args": [ "arg1a", "arg1b", "arg1c", "arg2a", "arg2b", "arg2c" ]
        }))
        .unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_fail_merge_with_no_kernel() {
        let config_a = PartialImageAssemblyConfig::default();
        let config_b = PartialImageAssemblyConfig::default();

        let result = ImageAssemblyConfig::try_from_partials(vec![config_a, config_b]);
        assert!(result.is_err());
    }

    #[test]
    fn test_fail_merge_with_more_than_one_kernel() {
        let config_a = PartialImageAssemblyConfig {
            kernel: Some(PartialKernelConfig {
                path: Some("foo".into()),
                args: Vec::default(),
                clock_backstop: Some(0),
            }),
            ..PartialImageAssemblyConfig::default()
        };
        let config_b = PartialImageAssemblyConfig {
            kernel: Some(PartialKernelConfig {
                path: Some("bar".into()),
                args: Vec::default(),
                clock_backstop: Some(2),
            }),
            ..PartialImageAssemblyConfig::default()
        };

        let result = ImageAssemblyConfig::try_from_partials(vec![config_a, config_b]);
        assert!(result.is_err());
    }
}
