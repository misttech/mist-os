// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use assembly_config_schema::BuildType;
use assembly_util::{BootfsDestination, CompiledPackageDestination, PackageDestination};
use camino::Utf8PathBuf;
use strum::IntoEnumIterator;

#[derive(FromArgs)]
/// Produce the bootfs/packages allowlist for assembly-generated files.
struct Args {
    /// the path to the output packages allowlist.
    #[argh(option)]
    packages: Utf8PathBuf,

    /// the path to the output bootfs file allowlist.
    #[argh(option)]
    bootfs: Utf8PathBuf,

    /// build type of the product.
    #[argh(option)]
    build_type: BuildType,
}

fn main() {
    let args: Args = argh::from_env();

    let mut packages: Vec<String> = PackageDestination::iter()
        .filter_map(|v| match (v, args.build_type) {
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
    let mut compiled_packages: Vec<String> = CompiledPackageDestination::iter()
        .filter_map(|v| match (v, args.build_type) {
            (
                CompiledPackageDestination::Fshost
                | CompiledPackageDestination::ForTest
                | CompiledPackageDestination::ForTest2,
                _,
            ) => None,
            (CompiledPackageDestination::Toolbox, BuildType::User) => None,
            (a @ CompiledPackageDestination::Toolbox, _) => Some(a.to_string()),
            (a @ _, _) => Some(a.to_string()),
        })
        .collect();
    packages.append(&mut compiled_packages);
    packages.sort();
    std::fs::write(args.packages, packages.join("\n")).expect("Writing packages allowlist");

    let mut bootfs: Vec<String> = BootfsDestination::iter()
        .filter_map(|v| match v {
            BootfsDestination::FromAIB(_) | BootfsDestination::ForTest => None,
            a @ _ => Some(a.to_string()),
        })
        .collect();
    bootfs.sort();
    std::fs::write(args.bootfs, bootfs.join("\n")).expect("Writing bootfs allowlist");
}
