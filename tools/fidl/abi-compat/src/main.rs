// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    fs::File,
    io::{stderr, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use argh::FromArgs;
use compare::CompatibilityProblems;
use maplit as _;

mod compare;
mod convert;
mod ir;

/// Evaluate ABI compatibility between two Fuchsia platform versions.
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

    /// if any errors are found print them to stderr and return an exit status
    #[argh(switch, short = 'e')]
    enforce: bool,
}

fn compare_platforms(external: PathBuf, platform: PathBuf) -> Result<CompatibilityProblems> {
    let external = ir::IR::load(&external).context(format!("loading {external:?}"))?;
    let platform = ir::IR::load(&platform).context(format!("loading {platform:?}"))?;

    let external = convert::convert_platform(external).context("processing external IR")?;
    let platform = convert::convert_platform(platform).context("processing platform IR")?;

    compare::compatible(&external, &platform).context("comparing external and platform IR")
}

fn main() -> Result<ExitCode> {
    let args: Args = argh::from_env();

    let mut out = File::create(args.out).unwrap();

    let problems = compare_platforms(args.external, args.platform).unwrap();

    let (errors, warnings) = problems.into_errors_and_warnings();

    let has_errors = errors.len() > 0;

    for inc in errors {
        writeln!(out, "{}", inc)?;
        if args.enforce {
            writeln!(stderr(), "{}", inc)?;
        }
    }
    for inc in warnings {
        writeln!(out, "{}", inc)?;
    }

    if args.enforce && has_errors {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}
