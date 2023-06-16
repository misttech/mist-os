// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context as _;
use anyhow::Result;
use argh::FromArgs;
use std::fs::read_dir;
use std::fs::write;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;

/// Arguments passed to the Scrutiny X smoke test intended to exercise Scrutiny X features against
/// a realistic assembled system.
#[derive(Debug, FromArgs, PartialEq)]
struct Args {
    /// path to depfile for build action management.
    #[argh(option)]
    pub depfile: Option<PathBuf>,

    /// path to stamp file for build action management.
    #[argh(option)]
    pub stamp: Option<PathBuf>,

    /// path to the directory containing the product bundle for the build.
    #[argh(option)]
    pub product_bundle: PathBuf,

    /// output debug logging.
    #[argh(switch)]
    pub debug: bool,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    if args.depfile.is_some() != args.stamp.is_some() {
        bail!("must specify both --depfile and --stamp, or neither");
    }

    if args.debug {
        tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(Level::TRACE)
            .with_span_events(FmtSpan::CLOSE)
            .init()
    }

    run_smoke_test(args)
}

#[tracing::instrument(level = "trace")]
fn run_smoke_test(args: Args) -> Result<()> {
    let Args { depfile, stamp, product_bundle, .. } = args;

    // Create an instance of `scrutiny_x::boxed::Scrutiny` here and operate on it

    if let (Some(depfile), Some(stamp)) = (depfile, stamp) {
        let depfile = File::create(depfile).context("creating depfile")?;
        write_depfile(depfile, &product_bundle, &stamp).context("writing depfile")?;

        write(stamp, "Scrutiny X boxed smoke test complete\n").context("writing stamp file")?;
    }

    Ok(())
}

#[tracing::instrument(level = "trace", skip_all)]
fn write_depfile<W: Write, InputsPath: AsRef<Path>, OutputPath: AsRef<Path>>(
    mut depfile: W,
    inputs_path: InputsPath,
    output_path: OutputPath,
) -> Result<()> {
    depfile.write_all(path_bytes(output_path.as_ref())?)?;
    depfile.write_all(":".as_bytes())?;

    walk_for_depfile(&mut depfile, inputs_path)?;
    depfile.write_all("\n".as_bytes())?;

    Ok(())
}

fn walk_for_depfile<W: Write, P: AsRef<Path>>(depfile: &mut W, directory: P) -> Result<()> {
    for entry in read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            walk_for_depfile(depfile, &path)?;
        } else {
            depfile.write_all(" ".as_bytes())?;
            depfile.write_all(path_bytes(path.as_path())?)?;
        }
    }

    Ok(())
}

fn path_bytes(path: &Path) -> Result<&[u8]> {
    Ok(path.to_str().ok_or_else(|| anyhow!("invalid path: {:?}", path))?.as_bytes())
}
