// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::FromArgs;
use assembly_config_schema::AssemblyConfig;
use camino::Utf8PathBuf;
use serdedoc::DocWriter;

/// Assembly documentation generator.
#[derive(Debug, FromArgs)]
struct AssemblyDocArgs {
    /// output directory
    #[argh(option)]
    output_dir: Utf8PathBuf,
}

fn main() -> Result<()> {
    let args: AssemblyDocArgs = argh::from_env();
    let writer = DocWriter::new("/reference/assembly/".to_string());
    writer.write::<AssemblyConfig>(args.output_dir)?;
    Ok(())
}
