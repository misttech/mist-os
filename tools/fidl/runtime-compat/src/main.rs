// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fs::File, io, path::PathBuf};

use anyhow::{Context, Result};
use argh::FromArgs;
use maplit as _;

mod compare;
mod convert;
mod ir;

/// Evaluate runtime compatibility between two Fuchsia platform versions.
#[derive(FromArgs)]
struct Args {
    /// path to a JSON file representing the platform version used by an external component.
    #[argh(option)]
    external: PathBuf,

    /// path to a JSON file representing the platform version used by the platform.
    #[argh(option)]
    platform: PathBuf,

    /// path to write a report to.
    #[argh(option)]
    out: PathBuf,
}

fn compare_platforms<W: io::Write>(external: PathBuf, platform: PathBuf, mut out: W) -> Result<()> {
    let external = ir::IR::load(&external).context(format!("loading {external:?}"))?;
    let platform = ir::IR::load(&platform).context(format!("loading {platform:?}"))?;

    let external = convert::convert_platform(external).context("processing external IR")?;
    let platform = convert::convert_platform(platform).context("processing platform IR")?;

    let problems =
        compare::compatible(&external, &platform).context("comparing external and platform IR")?;

    for inc in problems {
        writeln!(out, "{}", inc)?;
    }

    Ok(())
}

fn main() {
    let args: Args = argh::from_env();

    let out = File::create(args.out).unwrap();

    compare_platforms(args.external, args.platform, out).unwrap();
}
