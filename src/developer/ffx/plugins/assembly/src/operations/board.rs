// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Write;

use anyhow::{bail, Context, Result};
use assembly_config_schema::{
    BoardInputBundle, BoardProvidedConfig, PackageDetails, PackageSet, PackagedDriverDetails,
};
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use assembly_package_copy::PackageCopier;
use assembly_util as util;
use camino::Utf8PathBuf;
use ffx_assembly_args::BoardInputBundleArgs;
use serde::{Deserialize, Serialize};
use util::fast_copy;

pub fn board_input_bundle(args: BoardInputBundleArgs) -> Result<()> {
    let BoardInputBundleArgs {
        outdir,
        depfile,
        drivers,
        base_packages,
        bootfs_packages,
        kernel_boot_args,
        power_manager_config,
        thermal_config,
        power_metrics_recorder_config,
    } = args;
    let bundle_file_path = outdir.join("board_input_bundle.json");

    //========
    // Parse and collect the command-line arguments.

    // Parse the driver information from the file passed on the command-line.
    let drivers_information: DriversInformationHelper = match drivers {
        Some(drivers_path) => {
            util::read_config(&drivers_path).context("Loading driver information")?
        }
        None => Default::default(),
    };

    //========
    // Prepare the outdir for the copying of the packages and blobs

    // Erase all existing files from the outdir, so that the outdir won't ever
    // contain any stale output files.
    std::fs::remove_dir_all(&outdir).with_context(|| {
        format!("Deleting previous contents of the output directory: {}", &outdir)
    })?;

    // Always create the directories (this allows the hermetic inputs validator
    // to see that they are directories, on incremental builds when removing
    // the last package or subpackage).
    let packages_dir = outdir.join("packages");
    let subpackages_dir = outdir.join("subpackages");
    let blobstore = outdir.join("blobs");
    let config_files_dir = outdir.join("config");

    std::fs::create_dir_all(&packages_dir)
        .with_context(|| format!("Creating packages directory: {}", &packages_dir))?;
    std::fs::create_dir_all(&subpackages_dir)
        .with_context(|| format!("Creating subpackages directory: {}", &subpackages_dir))?;
    std::fs::create_dir_all(&blobstore)
        .with_context(|| format!("Creating blobstore directory: {}", &blobstore))?;
    std::fs::create_dir(&config_files_dir)
        .with_context(|| format!("Creating config files directory: {}", &config_files_dir))?;

    //========
    // Copy the configuration files provided, if any
    let power_manager_config =
        copy_config_file(&power_manager_config, "power_manager.json5", &config_files_dir)?;
    let thermal_config =
        copy_config_file(&thermal_config, "thermal_config.json", &config_files_dir)?;
    let power_metrics_recorder_config = copy_config_file(
        &power_metrics_recorder_config,
        "power_metrics_recorder_config.json",
        &config_files_dir,
    )?;

    //========
    // Copy the drivers and packages to the outdir, writing the package manifests
    // using file-relative paths to the subpackages dir and the blobstore.  As
    // the drivers and packages are added to the package

    let mut package_copier = PackageCopier::new(&packages_dir, &subpackages_dir, &blobstore);

    // Prepare the drivers for copying, and gather their new paths for use in
    // the BoardInputBundle struct.
    let mut drivers = vec![];
    for DriverInformation { package, set, components } in drivers_information.drivers {
        let copied_package_manifest_path = package_copier
            .add_package_from_manifest_path(&package)
            .with_context(|| format!("Preparing to copy driver package: {}", package))?;

        drivers.push(PackagedDriverDetails {
            package: copied_package_manifest_path.into(),
            set,
            components,
        })
    }

    // Prepare the packages for copying, and gather their new paths for use in
    // the BoardInputBundle struct.
    let mut packages = vec![];
    for (pkg_set, pkgs) in
        [(PackageSet::Base, base_packages), (PackageSet::Bootfs, bootfs_packages)]
    {
        for package_manifest_path in pkgs {
            let copied_package_manifest_path = package_copier
                .add_package_from_manifest_path(&package_manifest_path)
                .with_context(|| format!("Preparing to copy package: {}", package_manifest_path))?;

            packages.push(PackageDetails {
                package: copied_package_manifest_path.into(),
                set: pkg_set.clone(),
            })
        }
    }

    let inputs_for_depfile = package_copier
        .perform_copy()
        .with_context(|| format!("copying packages to: {}", outdir))?;

    //========
    // Create the BoardInputBundle struct

    let bundle = BoardInputBundle {
        drivers,
        packages,
        kernel_boot_args: kernel_boot_args.into_iter().collect(),
        configuration: if power_manager_config.is_some()
            || thermal_config.is_some()
            || power_metrics_recorder_config.is_some()
        {
            Some(BoardProvidedConfig {
                power_manager: power_manager_config,
                thermal: thermal_config,
                power_metrics_recorder: power_metrics_recorder_config,
            })
        } else {
            None
        },
    }
    // And convert all paths to be file-relative.
    .make_paths_relative_to_file(&bundle_file_path)
    .with_context(|| format!("Making board input bundle paths relative to: {bundle_file_path}"))?;

    let bundle_file = std::fs::File::create(&bundle_file_path)
        .with_context(|| format!("Failed to create bundle manifest file: {bundle_file_path}"))?;

    // and then write out the file.
    serde_json::to_writer_pretty(bundle_file, &bundle)
        .with_context(|| format!("Writing bundle manifest file to: {bundle_file_path}"))?;

    if let Some(depfile) = depfile {
        write_depfile(&depfile, &bundle_file_path, inputs_for_depfile)
            .with_context(|| format!("Writing depfile to {}", depfile))?;
    }

    Ok(())
}

/// Helper struct for deserializing the driver information file that's passed to
/// the creation of a board input bundle.  This is its own type so that it
/// can be exactly matched to the CLI arguments, and separately versioned from
/// internal types used by assembly.
// LINT.IfChange
#[derive(Default, Debug, Serialize, Deserialize)]
struct DriversInformationHelper {
    drivers: Vec<DriverInformation>,
}

/// Each packaged driver's information
#[derive(Debug, Serialize, Deserialize)]
struct DriverInformation {
    /// The path (relative to the current working dir) of the package manifest
    package: Utf8PathBuf,

    /// Which set this package belongs to.
    set: PackageSet,

    /// The driver components within the package, e.g. meta/foo.cm.
    components: Vec<Utf8PathBuf>,
}
// LINT.ThenChange(//build/assembly/board_input_bundle.gni)

fn write_depfile(
    depfile: &Utf8PathBuf,
    for_output: &Utf8PathBuf,
    inputs: impl IntoIterator<Item = Utf8PathBuf>,
) -> Result<()> {
    let mut depfile_writer = std::io::BufWriter::new(std::fs::File::create(&depfile)?);

    write!(depfile_writer, "{}:", for_output)?;

    for input in inputs {
        write!(depfile_writer, "\\\n    {}", input)?;
    }

    depfile_writer.flush()?;
    Ok(())
}

fn copy_config_file(
    source: &Option<Utf8PathBuf>,
    name: &str,
    config_files_dir: &Utf8PathBuf,
) -> Result<Option<FileRelativePathBuf>> {
    Ok(match source {
        Some(src) => {
            let dst = config_files_dir.join(name);
            if !dst.exists() {
                fast_copy(src, &dst)
                    .with_context(|| format!("Copying config file from {src} to {dst}"))?;
                Some(dst.into())
            } else {
                bail!("Destination file exists copying config file from {src} to {dst}");
            }
        }
        None => None,
    })
}
