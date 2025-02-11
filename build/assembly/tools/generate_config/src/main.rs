// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool for generating assembly configs.

#![deny(missing_docs)]

mod board_input_bundle;
mod product_config;

use anyhow::Result;
use argh::FromArgs;
use camino::Utf8PathBuf;

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
#[allow(clippy::large_enum_variant)]
enum Subcommand {
    /// generate a product config.
    Product(ProductArgs),

    /// generate a product config using an input product config as a template.
    HybridProduct(HybridProductArgs),

    /// generate a board input bundle.
    BoardInputBundle(BoardInputBundleArgs),
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

    /// a depfile to write.
    #[argh(option)]
    depfile: Option<Utf8PathBuf>,
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

    /// a depfile to write.
    #[argh(option)]
    depfile: Option<Utf8PathBuf>,
}

/// Arguments to generate a board input bundle.
#[derive(FromArgs)]
#[argh(subcommand, name = "board-input-bundle")]
struct BoardInputBundleArgs {
    /// the directory to write the board input bundle to.
    #[argh(option)]
    output: Utf8PathBuf,

    /// the path to the file that describes all the drivers to add to the bundle.
    /// The format of this file is a json list of dictionaries that specify the
    /// following fields:
    /// 1) 'package': The path to the package manifest
    /// 2) 'set': The package set that it belongs to ("bootfs" or "base")
    /// 3) 'components': A list of the driver components in this package.
    #[argh(option)]
    pub drivers: Option<Utf8PathBuf>,

    /// the paths to package manifests for all packages to add to the base
    /// package set.
    #[argh(option)]
    pub base_packages: Vec<Utf8PathBuf>,

    /// the paths to package manifests for all packages to add to the bootfs
    /// package set.
    #[argh(option)]
    pub bootfs_packages: Vec<Utf8PathBuf>,

    /// cpu-manager configuration
    #[argh(option)]
    pub cpu_manager_config: Option<Utf8PathBuf>,

    /// energy model configuration for processor power management
    #[argh(option)]
    pub energy_model_config: Option<Utf8PathBuf>,

    /// arguments to pass to the kernel on boot
    #[argh(option)]
    pub kernel_boot_args: Vec<String>,

    /// power-manager configuration
    #[argh(option)]
    pub power_manager_config: Option<Utf8PathBuf>,

    /// power metrics recorder configuration
    #[argh(option)]
    pub power_metrics_recorder_config: Option<Utf8PathBuf>,

    /// system power modes configuration
    #[argh(option)]
    pub system_power_mode_config: Option<Utf8PathBuf>,

    /// thermal management configuration
    #[argh(option)]
    pub thermal_config: Option<Utf8PathBuf>,

    /// thread role configuration files
    #[argh(option)]
    pub thread_roles: Vec<Utf8PathBuf>,

    /// sysmem format costs configuration files
    ///
    /// Each file's content bytes are a persistent fidl
    /// fuchsia.sysmem2.FormatCosts. Normally json[5] would be preferable for
    /// config, but we generate this config in rust using FIDL types (to avoid
    /// repetition and to take advantage of FIDL rust codegen), and there's no
    /// json schema for FIDL types.
    #[argh(option)]
    pub sysmem_format_costs_config: Vec<Utf8PathBuf>,

    /// a depfile to write.
    #[argh(option)]
    depfile: Option<Utf8PathBuf>,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();
    match args.command {
        Subcommand::Product(args) => product_config::new(&args),
        Subcommand::HybridProduct(args) => product_config::hybrid(&args),
        Subcommand::BoardInputBundle(args) => board_input_bundle::new(&args),
    }
}
