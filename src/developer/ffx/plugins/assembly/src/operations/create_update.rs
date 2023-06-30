// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subpackage_blobs_package::construct_subpackage_blobs_package;
use anyhow::{Context, Result};
use assembly_manifest::{AssemblyManifest, PackagesMetadata};
use assembly_partitions_config::PartitionsConfig;
use assembly_update_package::{Slot, UpdatePackageBuilder};
use assembly_update_packages_manifest::UpdatePackagesManifest;
use epoch::EpochFile;
use ffx_assembly_args::CreateUpdateArgs;
use fuchsia_pkg::PackageManifest;
use fuchsia_url::RepositoryUrl;
use std::collections::BTreeSet;
use std::fs::File;

pub fn create_update(args: CreateUpdateArgs) -> Result<()> {
    let mut file = File::open(&args.partitions)
        .with_context(|| format!("Failed to open: {}", args.partitions))?;
    let partitions = PartitionsConfig::from_reader(&mut file)
        .context("Failed to parse the partitions config")?;
    let epoch: EpochFile = EpochFile::Version1 { epoch: args.epoch };

    let system_a_manifest =
        args.system_a.as_ref().map(AssemblyManifest::try_load_from).transpose()?;

    let subpackage_blobs_package = if let Some(manifest) = &system_a_manifest {
        Some(construct_subpackage_blobs_package(
            manifest,
            &args.outdir,
            if let Some(gendir) = &args.gendir { gendir } else { &args.outdir },
            &args.subpackage_blobs_package_name,
        )?)
    } else {
        None
    };

    let mut builder = UpdatePackageBuilder::new(
        partitions,
        args.board_name,
        args.version_file,
        epoch,
        /*abi_revision=*/ None,
        &args.outdir,
    );

    // Set the package name.
    // Typically used for OTA tests.
    if let Some(name) = args.update_package_name {
        builder.set_name(name);
    }

    // Add the packages to update.
    if let Some(manifest) = &system_a_manifest {
        let mut packages = create_update_packages_manifest(manifest)?;

        // Inject the subpackage blobs package into the update package.
        if let Some(subpackage_blobs_package) = &subpackage_blobs_package {
            packages.add_by_manifest(&subpackage_blobs_package.manifest)?;
        };

        // Rewrite all the package URLs to use this repo as the repository.
        if let Some(default_repo) = args.rewrite_default_repo {
            let default_repo = RepositoryUrl::parse_host(default_repo)?;
            packages.set_repository(default_repo);
        }

        builder.add_packages(packages);
    }

    // Set the gendir separate from the outdir.
    if let Some(gendir) = args.gendir {
        builder.set_gendir(gendir);
    }

    // Set the images to update in the primary slot.
    if let Some(manifest) = system_a_manifest {
        builder.add_slot_images(Slot::Primary(manifest));
    }

    // Set the images to update in the recovery slot.
    if let Some(manifest) =
        args.system_r.as_ref().map(AssemblyManifest::try_load_from).transpose()?
    {
        builder.add_slot_images(Slot::Recovery(manifest));
    }

    builder.build()?;
    Ok(())
}

fn create_update_packages_manifest(
    assembly_manifest: &AssemblyManifest,
) -> Result<UpdatePackagesManifest> {
    let mut packages_manifest = UpdatePackagesManifest::V1(BTreeSet::new());
    for image in &assembly_manifest.images {
        if let Some(contents) = image.get_blobfs_contents() {
            let PackagesMetadata { base, cache } = &contents.packages;

            for package in base.0.iter().chain(cache.0.iter()) {
                let manifest = PackageManifest::try_load_from(&package.manifest)?;
                packages_manifest.add_by_manifest(&manifest)?;
            }
        }
    }

    Ok(packages_manifest)
}
