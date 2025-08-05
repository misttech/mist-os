// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{common, ProductInputBundleArgs};

use anyhow::Result;
use assembly_container::AssemblyContainer;
use assembly_release_info::ReleaseInfo;
use fuchsia_pkg::PackageManifest;
use product_input_bundle::{ProductInputBundle, ProductPackageDetails, ProductPackagesConfig};
use std::collections::BTreeMap;

pub fn new(args: &ProductInputBundleArgs) -> Result<()> {
    let ProductInputBundleArgs {
        name,
        base_packages,
        cache_packages,
        flexible_packages,
        packages_for_product_config,
        output,
        version,
        version_file,
        repo,
        repo_file,
        depfile,
    } = args;

    let mut base = BTreeMap::<String, ProductPackageDetails>::new();
    for manifest in base_packages {
        let package_manifest = PackageManifest::try_load_from(&manifest)?;
        let name = package_manifest.name().to_string();
        base.insert(name, ProductPackageDetails { manifest: manifest.clone() });
    }
    let mut cache = BTreeMap::<String, ProductPackageDetails>::new();
    for manifest in cache_packages {
        let package_manifest = PackageManifest::try_load_from(&manifest)?;
        let name = package_manifest.name().to_string();
        cache.insert(name, ProductPackageDetails { manifest: manifest.clone() });
    }
    let mut flexible = BTreeMap::<String, ProductPackageDetails>::new();
    for manifest in flexible_packages {
        let package_manifest = PackageManifest::try_load_from(&manifest)?;
        let name = package_manifest.name().to_string();
        flexible.insert(name, ProductPackageDetails { manifest: manifest.clone() });
    }
    let mut for_product_config = BTreeMap::<String, ProductPackageDetails>::new();
    for manifest in packages_for_product_config {
        let package_manifest = PackageManifest::try_load_from(&manifest)?;
        let name = package_manifest.name().to_string();
        for_product_config.insert(name, ProductPackageDetails { manifest: manifest.clone() });
    }
    let bundle = ProductInputBundle {
        packages: ProductPackagesConfig { base, cache, flexible, for_product_config },
        release_info: ReleaseInfo {
            name: name.to_string(),
            repository: common::get_release_repository(repo, repo_file)?,
            version: common::get_release_version(version, version_file)?,
        },
    };
    bundle.write_to_dir(output, depfile.as_ref())?;
    Ok(())
}
