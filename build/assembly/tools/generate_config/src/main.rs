// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool for generating assembly configs.

#![deny(missing_docs)]

use anyhow::Result;
use argh::FromArgs;
use assembly_config_schema::AssemblyConfig;
use assembly_container::AssemblyContainer;
use camino::Utf8PathBuf;
use fuchsia_pkg::PackageManifest;

/// Arguments to construct an assembly config.
#[derive(FromArgs)]
struct Args {
    /// which assembly config to generate.
    #[argh(subcommand)]
    command: Subcommand,
}

/// A subcommand to generate a specific assembly config.
#[derive(FromArgs)]
#[argh(subcommand)]
enum Subcommand {
    /// generate a product config.
    Product(ProductArgs),

    /// generate a product config using an input product config as a template.
    HybridProduct(HybridProductArgs),
}

/// Arguments to generate a product config.
#[derive(FromArgs)]
#[argh(subcommand, name = "product")]
struct ProductArgs {
    /// the input product config with absolute paths.
    #[argh(option)]
    config: Utf8PathBuf,

    /// the directory to write the product config to.
    #[argh(option)]
    output: Utf8PathBuf,
}

/// Arguments to generate a hybrid product config.
#[derive(FromArgs)]
#[argh(subcommand, name = "hybrid-product")]
struct HybridProductArgs {
    /// the input product config directory.
    #[argh(option)]
    input: Utf8PathBuf,

    /// an optional path to the config file inside the directory.
    #[argh(option)]
    config_path: Option<Utf8PathBuf>,

    /// a package to replace in the input.
    #[argh(option)]
    replace_package: Vec<Utf8PathBuf>,

    /// the directory to write the product config to.
    #[argh(option)]
    output: Utf8PathBuf,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();
    match args.command {
        Subcommand::Product(args) => generate_product(&args),
        Subcommand::HybridProduct(args) => generate_hybrid_product(&args),
    }
}

fn generate_product(args: &ProductArgs) -> Result<()> {
    let config = AssemblyConfig::from_config_path(&args.config)?;
    config.write_to_dir(&args.output)?;
    Ok(())
}

fn generate_hybrid_product(args: &HybridProductArgs) -> Result<()> {
    let config = match &args.config_path {
        Some(config_path) => AssemblyConfig::from_dir_with_config_path(&args.input, &config_path),
        None => AssemblyConfig::from_dir(&args.input),
    }?;

    // Write the config to a hermetic directory before we replace packages, so
    // that the package manifests have consistent locations.
    // TODO(https://fxbug.dev/391673787): This can go away when the product
    // config includes the package names.
    let tempdir = tempfile::tempdir().unwrap();
    let tempdir_path = Utf8PathBuf::try_from(tempdir.path().to_path_buf())?;
    let mut config = config.write_to_dir(tempdir_path)?;

    for package_manifest_path in &args.replace_package {
        let package_manifest = PackageManifest::try_load_from(&package_manifest_path)?;
        let package_name = package_manifest.name();
        if let Some(path) = find_package_in_product(&mut config, &package_name) {
            *path = package_manifest_path.clone();
        } else {
            anyhow::bail!("Could not find package to replace: {}", &package_name);
        }
    }
    config.write_to_dir(&args.output)?;
    Ok(())
}

fn find_package_in_product<'a>(
    config: &'a mut AssemblyConfig,
    package_name: impl AsRef<str>,
) -> Option<&'a mut Utf8PathBuf> {
    config.product.packages.base.iter_mut().chain(&mut config.product.packages.cache).find_map(
        |pkg| {
            // We are depending on the fact that PackageCopier copies the
            // package manifest to a location where the file name equals the
            // package name.
            //
            // In the future, we can consider encoding the package name into
            // the product config with BTreeMap<PackageName, Utf8PathBuf> to
            // avoid this.
            // TODO(https://fxbug.dev/391673787): Remove once the product config
            // keys packages by their name.
            if let Some(path) = pkg.manifest.as_utf8_pathbuf().file_name() {
                if path == package_name.as_ref() {
                    return Some(pkg.manifest.as_mut_utf8_pathbuf());
                }
            }
            return None;
        },
    )
}
