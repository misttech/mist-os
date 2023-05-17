// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context as _;
use assembly_config_schema::{FileEntry, ImageAssemblyConfig};
use assembly_validate_util::{BootfsContents, PkgNamespace};
use camino::Utf8PathBuf;
use fuchsia_pkg::PackageManifest;
use rayon::iter::{ParallelBridge, ParallelIterator};
use std::{collections::BTreeMap, fmt, fs::File, path::PathBuf};
use version_history::{self, AbiRevision};

/// Validate a product config.
pub fn validate_product(
    product: &ImageAssemblyConfig,
    disable_package_validation: &Vec<String>,
) -> Result<(), ProductValidationError> {
    // validate the packages in the system/base/cache package sets
    let manifests = product.system.iter().chain(product.base.iter()).chain(product.cache.iter());
    let packages: BTreeMap<_, _> = manifests
        .par_bridge()
        .filter_map(|package_manifest_path| {
            match PackageManifest::try_load_from(&package_manifest_path) {
                Ok(manifest) => {
                    // After loading the manifest, validate it
                    if let Err(e) = validate_package(&manifest) {
                        // If there was a validation error, but the package is
                        // named in the allowlist for validation errors, then
                        // print a warning only.
                        if disable_package_validation.contains(&manifest.name().to_string()) {
                            println!("WARNING: The package named '{}', with manifest at {} failed validation but is allowlisted:\n{}", manifest.name(), package_manifest_path, e);
                            None
                        } else {
                            // This is very temporary, while OOT products are updated to populate
                            // their allowlist as a soft-transition.
                            // Only the default case will be included after the transition is complete.
                            match e {
                                e @ PackageValidationError::MissingAbiRevisionFile(_) => {
                                    println!("WARNING: The package named '{}', with manifest at {} failed validation but is NOT allowlisted:\n{}", manifest.name(), package_manifest_path, e);
                                    None
                                }
                                e @ PackageValidationError::InvalidAbiRevisionFile(_) => {
                                    println!("WARNING: The package named '{}', with manifest at {} failed validation but is NOT allowlisted:\n{}", manifest.name(), package_manifest_path, e);
                                    None

                                }
                                e @ PackageValidationError::UnsupportedAbiRevision { found: _, supported: _ } => {
                                    println!("WARNING: The package named '{}', with manifest at {} failed validation but is NOT allowlisted:\n{}", manifest.name(), package_manifest_path, e);
                                    None
                                }
                                e @ _ =>{
                                    // otherwise, return the error along with the path
                                    // to the package manifest.
                                    Some((package_manifest_path.to_owned(), e))
                                 }
                            }
                        }
                    } else {
                        None
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
fn validate_bootfs(bootfs_files: &[FileEntry]) -> Result<(), BootfsValidationError> {
    let mut bootfs = BootfsContents::from_iter(
        bootfs_files.iter().map(|entry| (&entry.destination, &entry.source)),
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
    if !version_history::is_supported_abi_revision(abi_revision) {
        return Err(PackageValidationError::UnsupportedAbiRevision {
            found: abi_revision,
            supported: version_history::get_supported_abi_revisions()
                .iter()
                .map(AbiRevision::from)
                .collect(),
        });
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
    UnsupportedAbiRevision { found: AbiRevision, supported: Vec<AbiRevision> },
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
            UnsupportedAbiRevision { found, supported } => {
                write!(f, "The package abi revision ({found}) is not supported")?;
                for revision in supported {
                    write!(f, "\n└── {revision}")?;
                }
                Ok(())
            }
        }
    }
}
