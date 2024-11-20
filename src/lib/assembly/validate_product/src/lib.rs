// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_config_schema::ImageAssemblyConfig;
use assembly_constants::FileEntry;
use assembly_validate_package::{validate_component, validate_package, PackageValidationError};
use assembly_validate_util::{BootfsContents, PkgNamespace};
use camino::Utf8PathBuf;
use fuchsia_pkg::PackageManifest;
use rayon::iter::{ParallelBridge, ParallelIterator};
use std::collections::BTreeMap;
use std::fmt;

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
                    match validate_package(&manifest) {
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
