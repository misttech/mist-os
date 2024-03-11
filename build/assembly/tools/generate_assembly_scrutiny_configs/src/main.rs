// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use assembly_config_schema::BuildType;
use assembly_util::{
    BlobfsCompiledPackageDestination, BootfsDestination, BootfsPackageDestination,
    PackageDestination,
};
use camino::Utf8PathBuf;
use strum::IntoEnumIterator;

#[derive(FromArgs)]
/// Produce the bootfs/packages allowlist for assembly-generated files.
struct Args {
    /// the path to the output packages allowlist.
    #[argh(option)]
    static_packages: Utf8PathBuf,

    /// the path to the output bootfs packages allowlist.
    #[argh(option)]
    bootfs_packages: Utf8PathBuf,

    /// the path to the output bootfs file allowlist.
    #[argh(option)]
    bootfs_files: Utf8PathBuf,

    /// build type of the product.
    #[argh(option)]
    build_type: BuildType,
}

fn main() {
    let args: Args = argh::from_env();

    let mut static_packages: Vec<String> = PackageDestination::iter()
        .filter_map(|v| match (v, args.build_type) {
            // This script only returns assembly-generated files.
            // Files from AIBs or the product are collected and merged in a separate process.
            (
                PackageDestination::FromAIB(_)
                | PackageDestination::FromProduct(_)
                | PackageDestination::ForTest,
                _,
            ) => None,
            (PackageDestination::ShellCommands, BuildType::User) => None,
            (a @ PackageDestination::ShellCommands, _) => Some(a.to_string()),
            (a @ _, _) => Some(a.to_string()),
        })
        .collect();
    let mut compiled_packages: Vec<String> = BlobfsCompiledPackageDestination::iter()
        .filter_map(|v| match (v, args.build_type) {
            // Toolbox should not be included on user.
            (BlobfsCompiledPackageDestination::Toolbox, BuildType::User) => None,
            (a @ BlobfsCompiledPackageDestination::Toolbox, _) => Some(a.to_string()),
            (a @ _, _) => Some(a.to_string()),
        })
        .collect();
    static_packages.append(&mut compiled_packages);
    static_packages.sort();
    std::fs::write(args.static_packages, static_packages.join("\n"))
        .expect("Writing packages allowlist");

    let mut bootfs_packages: Vec<String> = BootfsPackageDestination::iter()
        .filter_map(|v| match v {
            // This script only returns assembly-generated files.
            // Files from AIBs are collected and merged in a separate process.
            BootfsPackageDestination::FromAIB(_) | BootfsPackageDestination::ForTest => None,
            a @ _ => Some(a.to_string()),
        })
        .collect();
    bootfs_packages.sort();
    std::fs::write(args.bootfs_packages, bootfs_packages.join("\n"))
        .expect("Writing bootfs packages allowlist");

    let mut bootfs_files: Vec<String> = BootfsDestination::iter()
        .filter_map(|v| match v {
            // This script only returns assembly-generated files.
            // Files from AIBs are collected and merged in a separate process.
            BootfsDestination::FromAIB(_) | BootfsDestination::ForTest => None,
            a @ _ => Some(a.to_string()),
        })
        .collect();
    bootfs_files.sort();
    std::fs::write(args.bootfs_files, bootfs_files.join("\n")).expect("Writing bootfs allowlist");
}
