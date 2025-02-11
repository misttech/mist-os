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

    // Build systems generally don't add package names to the config, so it
    // serializes index numbers in place of package names by default.
    // We add the package names in now, so all the rest of the rules can assume
    // the config has proper package names.
    let config = config.add_package_names()?;
    config.write_to_dir(&args.output)?;
    Ok(())
}

fn generate_hybrid_product(args: &HybridProductArgs) -> Result<()> {
    let config = AssemblyConfig::from_dir(&args.input)?;

    // Normally this would not be necessary, because all generated configs come
    // from this tool, which adds the package names above, but we still need to
    // support older product configs without names.
    let mut config = config.add_package_names()?;

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
        |(name, pkg)| {
            if name == package_name.as_ref() {
                return Some(pkg.manifest.as_mut_utf8_pathbuf());
            }
            return None;
        },
    )
}
