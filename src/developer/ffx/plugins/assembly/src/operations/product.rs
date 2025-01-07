// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::operations::product::assembly_builder::ImageAssemblyConfigBuilder;
use anyhow::{bail, Context, Result};
use assembly_config_schema::assembly_config::{
    AssemblyConfigWrapperForOverrides, CompiledComponentDefinition, CompiledPackageDefinition,
};
use assembly_config_schema::developer_overrides::DeveloperOverrides;
use assembly_config_schema::platform_config::PlatformConfig;
use assembly_config_schema::{
    AssemblyConfig, BoardInformation, BoardInputBundle, FeatureSupportLevel,
};
use assembly_constants::{BlobfsCompiledPackageDestination, CompiledPackageDestination};
use assembly_file_relative_path::SupportsFileRelativePaths;
use assembly_images_config::{FilesystemImageMode, ImagesConfig};
use assembly_tool::SdkToolProvider;
use assembly_util::read_config;
use camino::{Utf8Path, Utf8PathBuf};
use ffx_assembly_args::{PackageValidationHandling, ProductArgs};
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
        package_validation,
        custom_kernel_aib,
        developer_overrides,
    } = args;

    info!("Reading configuration files.");
    info!("  product: {}", product);

    if package_validation == PackageValidationHandling::Warning {
        eprintln!(
            "
*=========================================*
* PACKAGE VALIDATION DISABLED FOR PRODUCT *
*=========================================*
Resulting product is not supported and may misbehave!
"
        );
    }

    let product_path = product;

    let board_info_path = board_info;

    // If there are developer overrides, then those need to be parsed  and applied before other
    // actions can be taken, since they impact how the rest of the assembly process works.
    let (platform, product, board_info, developer_overrides) = if let Some(overrides_path) =
        developer_overrides
    {
        // If developer overrides are in use, parse to intermediate types so
        // that overrides can be applied.
        let AssemblyConfigWrapperForOverrides { platform, product, file_relative_paths } =
            read_config(&product_path).context("Reading product configuration")?;

        let developer_overrides = read_config::<DeveloperOverrides>(&overrides_path)
            .context("Reading developer overrides")?
            .resolve_paths_from_file(&overrides_path)
            .context("Resolving paths in developer overrides")?;

        let developer_overrides = developer_overrides.merge_developer_provided_files().context(
            "Merging developer-provided file paths into developer-provided configuration.",
        )?;

        print_developer_overrides_banner(&developer_overrides, &overrides_path)
            .context("Displaying developer overrides.")?;

        // Extract the platform, product, and board config developer overrides so that we can apply
        // them to the product and board configs.
        let platform_config_overrides = developer_overrides.platform;
        let product_config_overrides = developer_overrides.product;
        let board_config_overrides = developer_overrides.board;

        let product_config_path = if file_relative_paths { Some(&product_path) } else { None };

        let mut platform: PlatformConfig = merge_override_values_with_resolved_paths(
            platform,
            product_config_path,
            platform_config_overrides,
        )
        .context("Merging developer overrides into platform configuration")?;
        let product = merge_override_values_with_resolved_paths(
            product,
            product_config_path,
            product_config_overrides,
        )
        .context("Merging developer overrides into product configuration")?;
        let board_info = read_config::<serde_json::Value>(&board_info_path)
            .context("Reading board information")?;
        let board_info = merge_override_values_with_resolved_paths(
            board_info,
            Some(&board_info_path),
            board_config_overrides,
        )
        .context("Merging developer overrides into board configuration")?;

        // Reconstitute the developer overrides struct, but with a null platform and product
        // configs, since they've been used to modify the main platform and product configurations.
        let developer_overrides = DeveloperOverrides {
            platform: serde_json::Value::Null,
            product: serde_json::Value::Null,
            board: serde_json::Value::Null,
            ..developer_overrides
        };

        // If the developer overrides specifies `netboot_mode`, then we override
        // the image mode to 'ramdisk'.
        if developer_overrides.developer_only_options.netboot_mode {
            platform.storage.filesystems.image_mode = FilesystemImageMode::Ramdisk;
        }
        let platform = platform;

        (platform, product, board_info, Some(developer_overrides))
    } else {
        let config = read_config::<AssemblyConfig>(&product_path)
            .context("Reading product configuration")?;
        let config = if config.file_relative_paths {
            config
                .resolve_paths_from_file(&product_path)
                .context("Resolving paths in product config")?
        } else {
            config
        };
        let board_info = read_config::<BoardInformation>(&board_info_path)
            .context("Reading board configuration")?
            .resolve_paths_from_file(&board_info_path)
            .context("Resolving paths in board config")?;
        (config.platform, config.product, board_info, None)
    };

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
        .filter_map(|(path, bib)| bib.configuration.as_ref().map(|cfg| (path, cfg)))
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
    let resource_dir = input_bundles_dir.join("resources");
    let configuration = assembly_platform_configuration::define_configuration(
        &platform,
        &product,
        &board_info,
        &outdir,
        &resource_dir,
        developer_overrides.as_ref().and_then(|o| Some(&o.developer_only_options)),
    )?;

    // Now that all the configuration has been determined, create the builder
    // and start doing the work of creating the image assembly config.
    let image_mode = platform.storage.filesystems.image_mode;
    let mut builder =
        ImageAssemblyConfigBuilder::new(platform.build_type, board_info.name.clone(), image_mode);

    // Set the developer overrides, if any.
    if let Some(developer_overrides) = developer_overrides {
        builder
            .add_developer_overrides(developer_overrides)
            .context("Setting developer overrides")?;
    }

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

    // Add the kernel cmdline arguments
    builder.add_kernel_args(configuration.kernel_args)?;

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
                platform.feature_set_level == FeatureSupportLevel::Bootstrap
                    || platform.feature_set_level == FeatureSupportLevel::Embeddable,
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
        let compiled_package_def: CompiledPackageDefinition = CompiledPackageDefinition {
            name: CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::Core),
            components: vec![CompiledComponentDefinition {
                component_name: "core".to_string(),
                shards: configuration.core_shards.iter().map(Into::into).collect(),
            }],
            contents: Default::default(),
            includes: Default::default(),
            bootfs_package: Default::default(),
        };
        builder
            .add_compiled_package(&compiled_package_def, "".into())
            .context("Adding core shards")?;
    }

    // Add the legacy bundle.
    if let Some(legacy_bundle) = legacy_bundle {
        let legacy_bundle_path = legacy_bundle.join("assembly_config.json");
        builder
            .add_bundle(&legacy_bundle_path)
            .context(format!("Adding legacy bundle: {legacy_bundle_path}"))?;
    }

    // Add the bootfs files.
    builder.add_bootfs_files(&configuration.bootfs.files).context("Adding bootfs files")?;

    // Add product-specified packages and configuration
    builder.add_product_packages(product.packages).context("Adding product-provided packages")?;

    // Add product-specified memory buckets.
    if let Some(buckets) = platform.diagnostics.memory_monitor.buckets {
        builder.add_memory_buckets(&vec![buckets.into()])?;
    }

    // Add any packages compiled by the assembly process itself
    for package in configuration.compiled_packages.values() {
        builder
            .add_compiled_package(package, "".into())
            .context("adding configuration-generated package")?;
    }

    builder
        .add_product_base_drivers(product.base_drivers)
        .context("Adding product-provided base-drivers")?;

    // Add devicetree binary
    if let Some(devicetree_path) = &board_info.devicetree {
        builder
            .add_devicetree(devicetree_path.as_utf8_pathbuf())
            .context("Adding devicetree binary")?;
    }
    if let Some(devicetree_overlay_path) = &board_info.devicetree_overlay {
        builder
            .add_devicetree_overlay(devicetree_overlay_path.as_utf8_pathbuf())
            .context("Adding devicetree binary overlay")?;
    }

    // Construct and set the images config
    builder
        .set_images_config(
            ImagesConfig::from_product_and_board(
                &platform.storage.filesystems,
                &board_info.filesystems,
            )
            .context("Constructing images config")?,
        )
        .context("Setting images configuration.")?;

    //////////////////////
    //
    // Generate the output files.  All builder modifications must be complete by here.

    // Strip the mutability of the builder.
    let builder = builder;

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

    // Get the tool set.
    let tools = SdkToolProvider::try_new()?;

    // Do the actual building and validation of everything for the Image
    // Assembly config.
    let (image_assembly, validation_error) = builder
        .build_and_validate(
            &outdir,
            &tools,
            package_validation == PackageValidationHandling::Warning,
        )
        .context("Building Image Assembly config")?;

    if let Some(validation_error) = validation_error {
        return Err(validation_error.into());
    }

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

/// Merge overrides into the main configuration, after having resolved paths in the main
/// configuration, if that's necessary.
///
/// This requires:
///   1. deserialize the configuration struct
///   2. resolve paths if necessary
///   3. re-serialize to a serde_json::Value
///   4. perform the merge operation
///   5. deserialize the configuration struct
///
fn merge_override_values_with_resolved_paths<
    T: SupportsFileRelativePaths + serde::de::DeserializeOwned + serde::Serialize,
    P: AsRef<Utf8Path>,
>(
    config: serde_json::Value,
    config_path: Option<&P>,
    overrides: serde_json::Value,
) -> Result<T> {
    // Only perform the relativization if there's a config_path.
    let config = if let Some(config_path) = config_path {
        let parsed_config =
            serde_json::from_value::<T>(config).context("Parsing configuration value")?;
        let resolved_config = parsed_config
            .resolve_paths_from_file(config_path)
            .context("Resolving paths in parsed configuration value")?;
        serde_json::to_value(resolved_config)
            .context("Reserializing configuration value with resolved paths")?
    } else {
        config
    };

    let merged = assembly_config_schema::try_merge_into::<T>(config, overrides)
        .context("Merging platform config developer overrides")?;

    // Because serde_json and serde_json5 deserialize enums differently, we need to bounce the
    // serde_json::Value of the platform config through a string so that we can re-parse it
    // using serde_json5.
    // TODO: Remove this after the following issue is fixed:
    // https://github.com/google/serde_json5/issues/10
    let merged_json5 = serde_json::to_string_pretty(&merged)
        .context("Creating intermediate json5 from merged value")?;

    // Now deserialize the merged and serialized json5.  This works around the issue with enums.
    serde_json5::from_str(&merged_json5)
        .context("Deserializing merged value from intermediate json5")
}

fn print_developer_overrides_banner(
    overrides: &DeveloperOverrides,
    overrides_path: &Utf8PathBuf,
) -> Result<()> {
    let overrides_target = if let Some(target_name) = &overrides.target_name {
        target_name.as_str()
    } else {
        overrides_path.as_str()
    };
    println!();
    println!("WARNING!:  Adding the following via developer overrides from: {overrides_target}");

    let all_packages_in_base = overrides.developer_only_options.all_packages_in_base;
    let netboot_mode = overrides.developer_only_options.netboot_mode;
    if all_packages_in_base || netboot_mode {
        println!();
        println!("  Options:");
        if all_packages_in_base {
            println!("    all_packages_in_base: enabled")
        }
        if netboot_mode {
            println!("    netboot_mode: enabled")
        }
    }

    if overrides.platform.as_object().is_some_and(|p| !p.is_empty()) {
        println!();
        println!("  Platform Configuration Overrides / Additions:");
        for line in serde_json::to_string_pretty(&overrides.platform)?.lines() {
            println!("    {}", line);
        }
    }

    if overrides.product.as_object().is_some_and(|p| !p.is_empty()) {
        println!();
        println!("  Product Configuration Overrides / Additions:");
        for line in serde_json::to_string_pretty(&overrides.product)?.lines() {
            println!("    {}", line);
        }
    }

    if overrides.board.as_object().is_some_and(|p| !p.is_empty()) {
        println!();
        println!("  Board Configuration Overrides / Additions:");
        for line in serde_json::to_string_pretty(&overrides.board)?.lines() {
            println!("    {}", line);
        }
    }

    if !overrides.kernel.command_line_args.is_empty() {
        println!();
        println!("  Additional kernel command line arguments:");
        for arg in &overrides.kernel.command_line_args {
            println!("    {arg}");
        }
    }

    if !overrides.packages.is_empty() {
        println!();
        println!("  Additional packages:");
        for details in &overrides.packages {
            println!("    {} -> {}", details.set, details.package);
        }
    }

    if !overrides.shell_commands.is_empty() {
        println!();
        println!("  Additional shell command stubs:");
        for (entry, components) in &overrides.shell_commands {
            println!("    package: \"{entry}\"");
            for component in components {
                println!("      {component}")
            }
        }
    }

    if !overrides.packages_to_compile.is_empty() {
        println!();
        println!("  Additions to compiled packages:");
        for package in &overrides.packages_to_compile {
            println!("    package: \"{}\"", package.name);
            for component in &package.components {
                println!("      component: \"meta/{}.cm\"", component.component_name);
                for shard in &component.shards {
                    println!("        {shard}");
                }
            }
            if !package.contents.is_empty() {
                println!("      contents:");
                for content in &package.contents {
                    println!("        {}  (from: {})", content.destination, content.source);
                }
            }
        }
    }
    println!();
    // And an additional empty line to make sure that any /r's don't attempt to overwrite the last
    // line of this warning.
    println!();
    Ok(())
}
