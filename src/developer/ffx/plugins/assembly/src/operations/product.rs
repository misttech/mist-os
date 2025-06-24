// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use assembly_config_schema::developer_overrides::DeveloperOverrides;
use assembly_config_schema::{AssemblyConfig, BoardInformation};
use assembly_container::AssemblyContainer;
use assembly_file_relative_path::SupportsFileRelativePaths;
use assembly_platform_artifacts::PlatformArtifacts;
use assembly_sdk::SdkToolProvider;
use assembly_util::read_config;
use camino::Utf8PathBuf;
use ffx_assembly_args::{PackageValidationHandling, ProductArgs};
use fuchsia_pkg::PackageManifest;
use image_assembly_config_builder::{ProductAssembly, ValidationMode};
use log::info;

pub fn assemble(args: ProductArgs) -> Result<()> {
    let ProductArgs {
        product,
        board_info,
        outdir,
        gendir: _,
        input_bundles_dir,
        package_validation,
        custom_kernel_aib,
        custom_boot_shim_aib,
        suppress_overrides_warning,
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

    // Parse the input configs.
    let platform_artifacts = Some(PlatformArtifacts::from_dir_with_path(&input_bundles_dir)?)
        .context("Reading platform artifacts")?;
    let product_config =
        AssemblyConfig::from_dir(&product).context("Reading product configuration")?;
    let board_config =
        BoardInformation::from_dir(&board_info).context("Reading board configuration")?;
    let developer_overrides = if let Some(overrides_path) = developer_overrides {
        let developer_overrides = read_config::<DeveloperOverrides>(&overrides_path)
            .context("Reading developer overrides")?
            .resolve_paths_from_file(&overrides_path)
            .context("Resolving paths in developer overrides")?;

        let developer_overrides = developer_overrides.merge_developer_provided_files().context(
            "Merging developer-provided file paths into developer-provided configuration.",
        )?;

        if !suppress_overrides_warning {
            print_developer_overrides_banner(&developer_overrides, &overrides_path)
                .context("Displaying developer overrides.")?;
        }
        Some(developer_overrides)
    } else {
        None
    };

    // Prepare product assembly.
    let mut pa = ProductAssembly::new(platform_artifacts, product_config, board_config)?;
    if let Some(developer_overrides) = developer_overrides {
        pa = pa.add_developer_overrides(developer_overrides)?;
    }
    if let Some(path) = custom_kernel_aib {
        pa.set_kernel_aib(path);
    }
    if let Some(path) = custom_boot_shim_aib {
        pa.set_boot_shim_aib(path)?;
    }
    if package_validation == PackageValidationHandling::Warning {
        pa.set_validation_mode(ValidationMode::WarnOnly);
    }

    //////////////////////
    //
    // Generate the output files.  All builder modifications must be complete by here.

    // Serialize the builder state for forensic use.
    let builder_forensics_file_path = outdir.join("assembly_builder_forensics.json");
    let board_forensics_file_path = outdir.join("board_configuration_forensics.json");
    pa.write_forensics_files(builder_forensics_file_path, board_forensics_file_path);

    // Strip the mutability of the builder.
    let pa = pa;

    // Do the actual building and validation of everything for the Image
    // Assembly config.
    let tools = SdkToolProvider::try_new()?;
    let image_assembly_config =
        pa.build(&tools, &outdir).context("Building Image Assembly config")?;

    // Serialize out the Image Assembly configuration.
    let image_assembly_path = outdir.join("image_assembly.json");
    let image_assembly_file = std::fs::File::create(&image_assembly_path).with_context(|| {
        format!("Failed to create image assembly config file: {image_assembly_path}")
    })?;
    serde_json::to_writer_pretty(image_assembly_file, &image_assembly_config)
        .with_context(|| format!("Writing image assembly config file: {image_assembly_path}"))?;

    Ok(())
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

    if let Some(path) = &overrides.bootfs_files_package {
        let manifest = PackageManifest::try_load_from(&path)
            .with_context(|| format!("parsing {} as a package manifest", path))?;
        let blobs = manifest.into_blobs();
        if blobs.len() > 1 {
            println!();
            println!("  Additional bootfs files:");
            for blob in blobs {
                if blob.path.starts_with("meta/") {
                    continue;
                }
                println!("    {}  (from: {})", blob.path, blob.source_path);
            }
        }
    }

    println!();
    // And an additional empty line to make sure that any /r's don't attempt to overwrite the last
    // line of this warning.
    println!();
    Ok(())
}
