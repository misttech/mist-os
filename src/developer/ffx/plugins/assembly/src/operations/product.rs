// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::operations::product::assembly_builder::ImageAssemblyConfigBuilder;
use anyhow::{bail, Context, Result};
use assembly_config_schema::assembly_config::{
    AdditionalPackageContents, CompiledPackageDefinition,
};
use assembly_config_schema::developer_overrides::DeveloperOverrides;
use assembly_config_schema::{
    AssemblyConfig, BoardInformation, BoardInputBundle, FeatureSupportLevel,
};
use assembly_file_relative_path::SupportsFileRelativePaths;
use assembly_images_config::ImagesConfig;
use assembly_tool::SdkToolProvider;
use assembly_util::{read_config, BlobfsCompiledPackageDestination, CompiledPackageDestination};
use camino::Utf8PathBuf;
use ffx_assembly_args::{PackageMode, PackageValidationHandling, ProductArgs};
use std::collections::BTreeMap;
use tracing::info;

mod assembly_builder;

pub fn assemble(args: ProductArgs) -> Result<()> {
    let ProductArgs {
        product,
        board_info,
        outdir,
        gendir: _,
        input_bundles_dir,
        legacy_bundle,
        additional_packages_path,
        package_validation,
        mode,
        custom_kernel_aib,
        developer_overrides,
    } = args;

    let _overrides = if let Some(overrides_path) = developer_overrides {
        let overrides = read_config(&overrides_path).context("Reading developer overrides")?;
        print_developer_overrides_banner(&overrides);
        Some(overrides)
    } else {
        None
    };

    info!("Reading configuration files.");
    info!("  product: {}", product);

    let config: AssemblyConfig = read_config(&product).context("Reading product configuration")?;

    let board_info_path = board_info;
    let board_info = read_config::<BoardInformation>(&board_info_path)
        .context("Reading board information")?
        // and then resolve the file-relative paths to be relative to the cwd instead.
        .resolve_paths_from_file(&board_info_path)
        .context("Resolving paths in board configuration.")?;

    // Parse the board's Board Input Bundles, if it has them, and merge their
    // configuration fields into that of the board_info struct.
    let mut board_input_bundles = Vec::new();
    for bundle_path in &board_info.input_bundles {
        let bundle_path = bundle_path.as_utf8_pathbuf().join("board_input_bundle.json");
        let bundle = read_config::<BoardInputBundle>(&bundle_path)
            .with_context(|| format!("Reading board input bundle: {bundle_path}"))?;
        let bundle = bundle
            .resolve_paths_from_file(&bundle_path)
            .with_context(|| format!("resolving paths in board input bundle: {bundle_path}"))?;
        board_input_bundles.push((bundle_path, bundle));
    }
    let board_input_bundles = board_input_bundles;

    // Find the Board Input Bundle that's providing the configuration files, by first finding _all_
    // structs that aren't None, and then verifying that we only have one of them.  This is perhaps
    // more complicated than strictly necessary to get that struct, because it collects all paths to
    // the bundles that are providing a Some() value, and reporting them all in the error.
    let board_configuration_files = board_input_bundles
        .iter()
        .filter_map(
            |(path, bib)| if let Some(cfg) = &bib.configuration { Some((path, cfg)) } else { None },
        )
        .collect::<Vec<_>>();

    let board_provided_config = if board_configuration_files.len() > 1 {
        let paths = board_configuration_files
            .iter()
            .map(|(path, _)| format!("  - {path}"))
            .collect::<Vec<_>>();
        let paths = paths.join("\n");
        bail!("Only one board input bundle can provide configuration files, found: \n{paths}");
    } else {
        board_configuration_files.first().map(|(_, cfg)| (*cfg).clone()).unwrap_or_default()
    };

    // Replace board_info with a new one that swaps its empty 'configuraton' field
    // for the consolidated one created from the board's input bundles.
    let board_info = BoardInformation { configuration: board_provided_config, ..board_info };

    // Get platform configuration based on the AssemblyConfig and the BoardInformation.
    let ramdisk_image = mode == PackageMode::DiskImageInZbi;
    let resource_dir = input_bundles_dir.join("resources");
    let configuration = assembly_platform_configuration::define_configuration(
        &config,
        &board_info,
        ramdisk_image,
        &outdir,
        &resource_dir,
    )?;

    // Now that all the configuration has been determined, create the builder
    // and start doing the work of creating the image assembly config.
    let mut builder = ImageAssemblyConfigBuilder::new(config.platform.build_type.clone());

    // Add the special platform AIB for the zircon kernel, or if provided, an
    // AIB that contains a custom kernel to use instead.
    let kernel_aib_path = match custom_kernel_aib {
        None => make_bundle_path(&input_bundles_dir, "zircon"),
        Some(custom_kernel_aib_path) => custom_kernel_aib_path,
    };
    builder
        .add_bundle(&kernel_aib_path)
        .with_context(|| format!("Adding kernel input bundle ({kernel_aib_path})"))?;

    // Set the info used for BoardDriver arguments.
    builder
        .set_board_driver_arguments(&board_info)
        .context("Setting arguments for the Board Driver")?;

    // Set the configuration for the rest of the packages.
    for (package, config) in configuration.package_configs {
        builder.set_package_config(package, config)?;
    }

    // Add the domain config packages.
    for (package, config) in configuration.domain_configs {
        builder.add_domain_config(package, config)?;
    }

    // Add the configuration capabilities.
    builder.add_configuration_capabilities(configuration.configuration_capabilities)?;

    // Add the board's Board Input Bundles, if it has them.
    for (bundle_path, bundle) in board_input_bundles {
        builder
            .add_board_input_bundle(
                bundle,
                config.platform.feature_set_level == FeatureSupportLevel::Bootstrap,
            )
            .with_context(|| format!("Adding board input bundle from: {bundle_path}"))?;
    }

    // Add the platform Assembly Input Bundles that were chosen by the configuration.
    for platform_bundle_name in &configuration.bundles {
        let platform_bundle_path = make_bundle_path(&input_bundles_dir, platform_bundle_name);
        builder.add_bundle(&platform_bundle_path).with_context(|| {
            format!("Adding platform bundle {platform_bundle_name} ({platform_bundle_path})")
        })?;
    }

    // Add the core shards.
    if !configuration.core_shards.is_empty() {
        let compiled_package_def =
            CompiledPackageDefinition::Additional(AdditionalPackageContents {
                name: CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::Core),
                component_shards: BTreeMap::from([(
                    "core".to_string(),
                    configuration.core_shards.clone(),
                )]),
            });
        builder
            .add_compiled_package(&compiled_package_def, "".into())
            .context("Adding core shards")?;
    }

    // Add the legacy bundle.
    let legacy_bundle_path = legacy_bundle.join("assembly_config.json");
    builder
        .add_bundle(&legacy_bundle_path)
        .context(format!("Adding legacy bundle: {legacy_bundle_path}"))?;

    // Set the Structured Configuration for the components in Bootfs
    builder.set_bootfs_structured_config(configuration.bootfs.components);

    // Add the bootfs files.
    builder.add_bootfs_files(&configuration.bootfs.files).context("Adding bootfs files")?;

    // Add product-specified packages and configuration
    builder
        .add_product_packages(config.product.packages)
        .context("Adding product-provided packages")?;

    builder
        .add_product_base_drivers(config.product.base_drivers)
        .context("Adding product-provided base-drivers")?;

    if let Some(package_config_path) = additional_packages_path {
        let additional_packages =
            read_config(package_config_path).context("Reading additional package config")?;
        builder.add_product_packages(additional_packages).context("Adding additional packages")?;
    }

    // Get the tool set.
    let tools = SdkToolProvider::try_new()?;

    // Serialize the builder state for forensic use.
    let builder_forensics_file_path = outdir.join("assembly_builder_forensics.json");
    let board_forensics_file_path = outdir.join("board_configuration_forensics.json");

    if let Some(parent_dir) = builder_forensics_file_path.parent() {
        std::fs::create_dir_all(parent_dir)
            .with_context(|| format!("unable to create outdir: {outdir}"))?;
    }
    let builder_forensics_file =
        std::fs::File::create(&builder_forensics_file_path).with_context(|| {
            format!("Failed to create builder forensics files: {builder_forensics_file_path}")
        })?;
    serde_json::to_writer_pretty(builder_forensics_file, &builder).with_context(|| {
        format!("Writing builder forensics file to: {builder_forensics_file_path}")
    })?;

    let board_forensics_file =
        std::fs::File::create(&board_forensics_file_path).with_context(|| {
            format!("Failed to create builder forensics files: {builder_forensics_file_path}")
        })?;
    serde_json::to_writer_pretty(board_forensics_file, &board_info)
        .with_context(|| format!("Writing board forensics file to: {board_forensics_file_path}"))?;

    // Do the actual building of everything for the Image Assembly config.
    let mut image_assembly =
        builder.build(&outdir, &tools).context("Building Image Assembly config")?;
    let images = ImagesConfig::from_product_and_board(
        &config.platform.storage.filesystems,
        &board_info.filesystems,
    )
    .context("Constructing images config")?;
    image_assembly.images_config = images;

    // Validate the built product assembly.
    assembly_validate_product::validate_product(
        &image_assembly,
        package_validation == PackageValidationHandling::Warning,
    )?;

    // Serialize out the Image Assembly configuration.
    let image_assembly_path = outdir.join("image_assembly.json");
    let image_assembly_file = std::fs::File::create(&image_assembly_path).with_context(|| {
        format!("Failed to create image assembly config file: {image_assembly_path}")
    })?;
    serde_json::to_writer_pretty(image_assembly_file, &image_assembly)
        .with_context(|| format!("Writing image assembly config file: {image_assembly_path}"))?;

    Ok(())
}

fn make_bundle_path(bundles_dir: &Utf8PathBuf, name: &str) -> Utf8PathBuf {
    bundles_dir.join(name).join("assembly_config.json")
}

fn print_developer_overrides_banner(overrides: &DeveloperOverrides) {
    println!();
    println!("WARNING:  Adding the following via developer overrides!");
    if overrides.developer_only_options.all_packages_in_base {
        println!("  all_packages_in_base: enabled")
    }
    println!();
}
