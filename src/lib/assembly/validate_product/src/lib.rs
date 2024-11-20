// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context as _;
use assembly_config_schema::ImageAssemblyConfig;
use assembly_constants::FileEntry;
use assembly_validate_util::{BootfsContents, PkgNamespace};
use camino::Utf8PathBuf;
use fuchsia_pkg::PackageManifest;
use rayon::iter::{ParallelBridge, ParallelIterator};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use version_history::{AbiRevision, AbiRevisionError};

/// Validate a product config.
pub fn validate_product(
    product: &ImageAssemblyConfig,
    warn_only: bool,
) -> Result<(), ProductValidationError> {
    // validate the packages in the system/base/cache package sets
    let manifests = product.system.iter().chain(product.base.iter()).chain(product.cache.iter());
    let packages: BTreeMap<_, _> = manifests
        .par_bridge()
        .filter_map(|package_manifest_path| {
            match PackageManifest::try_load_from(&package_manifest_path) {
                Ok(manifest) => {
                    // After loading the manifest, validate it
                    match  validate_package(&manifest) {
                        Ok(()) => None,
                        Err(e) => {
                            // If validation issues have been downgraded to warnings, then print
                            // a warning instead of returning an error.
                            let print_warning = warn_only && match e {
                                PackageValidationError::MissingAbiRevisionFile(_) => true,
                                PackageValidationError::InvalidAbiRevisionFile(_) => true,
                                PackageValidationError::UnsupportedAbiRevision (_) => true,
                                _ => false};
                             if print_warning {
                                eprintln!("WARNING: The package named '{}', with manifest at {} failed validation:\n{}", manifest.name(), package_manifest_path, e);
                                None
                             } else {
                                // return the error
                                Some((package_manifest_path.to_owned(), e))
                            }
                        }
                    }
                }
                // Convert any error loading the manifest into the appropriate
                // error type.
                Err(e) => Some((package_manifest_path.to_owned(), PackageValidationError::LoadPackageManifest(e)))
            }
        })
        .collect();

    // validate the contents of bootfs
    match validate_bootfs(&product.bootfs_files) {
        Ok(()) if packages.is_empty() => Ok(()),
        Ok(()) => Err(ProductValidationError { bootfs: Default::default(), packages }),
        Err(bootfs) => Err(ProductValidationError { bootfs: Some(bootfs), packages }),
    }
}

/// Validate the contents of bootfs.
///
/// Assumes that all component manifests have a `.cm` extension within the destination namespace.
fn validate_bootfs(bootfs_files: &[FileEntry<String>]) -> Result<(), BootfsValidationError> {
    let mut bootfs = BootfsContents::from_iter(
        bootfs_files.iter().map(|entry| (entry.destination.to_string(), &entry.source)),
    )
    .map_err(BootfsValidationError::ReadContents)?;

    // validate components
    let mut errors = BTreeMap::new();
    for path in bootfs.paths().into_iter().filter(|p| p.ends_with(".cm")) {
        if let Err(e) = validate_component(&path, &mut bootfs) {
            errors.insert(path, e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(BootfsValidationError::InvalidComponents(errors))
    }
}

/// Validate a package's contents.
///
/// Assumes that all component manifests will be in the `meta/` directory and have a `.cm` extension
/// within the package namespace.
pub fn validate_package(manifest: &PackageManifest) -> Result<(), PackageValidationError> {
    let blobs = manifest.blobs();

    // read meta.far contents
    let meta_far_info = blobs
        .into_iter()
        .find(|b| b.path == "meta/")
        .ok_or(PackageValidationError::MissingMetaFar)?;
    let meta_far =
        File::open(&meta_far_info.source_path).map_err(|source| PackageValidationError::Open {
            source,
            path: PathBuf::from(meta_far_info.source_path.clone()),
        })?;
    let mut reader =
        fuchsia_archive::Utf8Reader::new(meta_far).map_err(PackageValidationError::ReadArchive)?;

    // validate components in the meta/ directory
    let mut errors = BTreeMap::new();
    for path in reader.paths().into_iter().filter(|p| p.ends_with(".cm")) {
        if let Err(e) = validate_component(&path, &mut reader) {
            errors.insert(path, e);
        }
    }
    if !errors.is_empty() {
        return Err(PackageValidationError::InvalidComponents(errors));
    }

    // validate the abi_revision of the package
    let raw_abi_revision = reader
        .read_file(fuchsia_pkg::ABI_REVISION_FILE_PATH)
        .map_err(|e| PackageValidationError::MissingAbiRevisionFile(e))?;
    let abi_revision = AbiRevision::try_from(raw_abi_revision.as_slice())
        .map_err(|e| PackageValidationError::InvalidAbiRevisionFile(e))?;

    match version_history_data::HISTORY.check_abi_revision_for_runtime(abi_revision) {
        Ok(()) => {}
        Err(AbiRevisionError::PlatformMismatch { .. })
        | Err(AbiRevisionError::UnstableMismatch { .. })
        | Err(AbiRevisionError::Malformed { .. }) => {
            // TODO(https://fxbug.dev/347724655): Make these cases errors.
            eprintln!(
                "Unsupported platform ABI revision: 0x{}.
This will become an error soon! See https://fxbug.dev/347724655",
                abi_revision
            );
        }
        Err(e @ AbiRevisionError::TooNew { .. })
        | Err(e @ AbiRevisionError::Retired { .. })
        | Err(e @ AbiRevisionError::Invalid) => {
            return Err(PackageValidationError::UnsupportedAbiRevision(e))
        }
    }

    Ok(())
}

/// Validate an individual component within the package.
fn validate_component(
    manifest_path: &str,
    pkg_namespace: &mut impl PkgNamespace,
) -> anyhow::Result<()> {
    assembly_structured_config::validate_component(manifest_path, pkg_namespace)
        .context("Validating structured configuration")?;
    Ok(())
}

/// Collection of all package validation failures within a product.
#[derive(Debug)]
pub struct ProductValidationError {
    /// Files in bootfs which failed validation.
    bootfs: Option<BootfsValidationError>,
    /// Packages which failed validation.
    packages: BTreeMap<Utf8PathBuf, PackageValidationError>,
}

impl From<ProductValidationError> for anyhow::Error {
    fn from(e: ProductValidationError) -> anyhow::Error {
        anyhow::Error::msg(e)
    }
}

impl fmt::Display for ProductValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Validating product assembly failed:")?;
        if let Some(error) = &self.bootfs {
            let error_msg = textwrap::indent(&error.to_string(), "        ");
            write!(f, "    └── Failed to validate bootfs: {}", error_msg)?;
        }
        for (package, error) in &self.packages {
            let error_msg = textwrap::indent(&error.to_string(), "        ");
            write!(f, "    └── {}: {}", package, error_msg)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum BootfsValidationError {
    ReadContents(assembly_validate_util::BootfsContentsError),
    InvalidComponents(BTreeMap<String, anyhow::Error>),
}

impl fmt::Display for BootfsValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use BootfsValidationError::*;
        match self {
            ReadContents(source) => {
                write!(f, "Unable to read bootfs contents: {}", source)
            }
            InvalidComponents(components) => {
                for (name, error) in components {
                    write!(f, "\n└── {}: {}", name, error)?;
                    let mut source = error.source();
                    while let Some(s) = source {
                        write!(f, "\n    └── {}", s)?;
                        source = s.source();
                    }
                }
                Ok(())
            }
        }
    }
}

/// Failures that can occur when validating packages.
#[derive(Debug)]
pub enum PackageValidationError {
    Open { path: PathBuf, source: std::io::Error },
    LoadPackageManifest(anyhow::Error),
    MissingMetaFar,
    ReadArchive(fuchsia_archive::Error),
    InvalidComponents(BTreeMap<String, anyhow::Error>),
    MissingAbiRevisionFile(fuchsia_archive::Error),
    InvalidAbiRevisionFile(std::array::TryFromSliceError),
    UnsupportedAbiRevision(version_history::AbiRevisionError),
}

impl fmt::Display for PackageValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PackageValidationError::*;
        match self {
            Open { path, source } => write!(f, "Unable to open `{}`: {}.", path.display(), source),
            LoadPackageManifest(source) => {
                write!(f, "Unable to decode JSON for package manifest: {}.", source)
            }
            MissingMetaFar => write!(f, "The package seems to be missing a meta/ directory."),
            ReadArchive(source) => write!(f, "Unable to read the package's meta.far: {}.", source),
            InvalidComponents(components) => {
                for (name, error) in components {
                    write!(f, "\n└── {}: {}", name, error)?;
                    let mut source = error.source();
                    while let Some(s) = source {
                        write!(f, "\n    └── {}", s)?;
                        source = s.source();
                    }
                }
                Ok(())
            }
            MissingAbiRevisionFile(cause) => {
                write!(
                    f,
                    "The package seems to be missing an abi revision file:  {}",
                    fuchsia_pkg::ABI_REVISION_FILE_PATH
                )?;
                write!(f, "\n└── {}", cause)?;
                Ok(())
            }
            InvalidAbiRevisionFile(cause) => {
                write!(f, "The package abi revision file was not valid")?;
                write!(f, "\n└── {cause}")?;
                Ok(())
            }
            UnsupportedAbiRevision(version_history::AbiRevisionError::TooNew {
                abi_revision,
                supported_versions,
            }) => write!(
                f,
                "Package targets an unknown target ABI revision: 0x{}.

Your Fuchsia SDK is probably too old to support it. Either update your
SDK to a newer version, or target one of the following supported API levels
when building this package:{}",
                abi_revision, supported_versions
            ),
            UnsupportedAbiRevision(version_history::AbiRevisionError::PlatformMismatch {
                abi_revision,
                package_date,
                package_commit_hash,
                platform_date,
                platform_commit_hash,
            }) => {
                write!(
                    f,
                    "Package targets an unsupported platform ABI revision: 0x{}

Assembly inputs must come from *exactly* the same Fuchsia version as the SDK.
",
                    abi_revision
                )?;
                if package_date == platform_date {
                    write!(
                        f,
                        "
Your assembly inputs and SDK appear to be both from the same day ({}), but
from different `integration.git` repos (Git revision prefix {:#X} vs {:#X}).
",
                        package_date, package_commit_hash, platform_commit_hash
                    )?;
                } else {
                    write!(
                        f,
                        "
Your assembly inputs are from: {}
Your Fuchsia SDK is from: {}
",
                        package_date, platform_date
                    )?;
                }

                write!(
                    f,
                    "
You probably updated your SDK without updating your assembly inputs, or vice
versa. Ensure your SDK and assembly inputs come from the same release and try
again."
                )
            }
            UnsupportedAbiRevision(version_history::AbiRevisionError::UnstableMismatch {
                abi_revision,
                package_sdk_date,
                package_sdk_commit_hash,
                platform_date,
                platform_commit_hash,
                supported_versions,
            }) => {
                write!(
                    f,
                    "Package targets an unsupported unstable ABI revision: 0x{}.

Packages that target unstable API levels like HEAD and NEXT must be built and
assembled by *exactly* the same version of the SDK.
",
                    abi_revision
                )?;

                if package_sdk_date == platform_date {
                    write!(
                        f,
                        "
The SDK that built the package comes from the same day as this SDK ({}), but
they appear to come from different `integration.git` repos (Git revision prefix
{:#X} vs {:#X}).
",
                        package_sdk_date, package_sdk_commit_hash, platform_commit_hash
                    )?;
                } else {
                    write!(
                        f,
                        "
The SDK that built the package is from: {}
Your Fuchsia SDK is from: {}
",
                        package_sdk_date, platform_date
                    )?;
                }
                write!(
                    f,
                    "
Ensure that the build and assembly steps use exactly the same SDK and try
again, or rebuild the package targeting one of the following stable API
levels:{}",
                    supported_versions
                )
            }
            UnsupportedAbiRevision(version_history::AbiRevisionError::Retired {
                version,
                supported_versions,
            }) => write!(
                f,
                "Package targets retired API level {} (0x{}).

API level {} has been retired and is no longer supported. The package's owner
must update it to target one of the following API levels:{}",
                version.api_level, version.abi_revision, version.api_level, supported_versions
            ),
            UnsupportedAbiRevision(version_history::AbiRevisionError::Invalid) => write!(
                f,
                "The package was marked with the 'Invalid' ABI revision:
0x{}. This is unusual and probably represents a bug.",
                AbiRevision::INVALID
            ),
            UnsupportedAbiRevision(version_history::AbiRevisionError::Malformed {
                abi_revision,
            }) => write!(
                f,
                "The package was marked with an unrecognized 'special'
ABI revision 0x{}.

This is unusual. The ABI revision may be malformed, or your Fuchsia SDK may be
too old to know what this means. Consider updating your SDK to a newer
version.",
                abi_revision
            ),
        }
    }
}
