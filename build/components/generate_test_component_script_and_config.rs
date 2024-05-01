// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(about = "Generate a bash wrapper script amd test config for test components.")]
struct Args {
    #[structopt(long, help = "The path to the host binary.")]
    bin_path: String,

    #[structopt(long, help = "The path to the test pilot.")]
    test_pilot: String,

    #[structopt(long, help = "Generated script path.")]
    script_output_filename: String,

    #[structopt(long, help = "Path to component manifest.")]
    component_manifest_path: String,

    #[structopt(long, help = "Path to partial test config.")]
    partial_test_config: String,

    #[structopt(long, help = "Path to test component specific config.")]
    test_component_config: String,

    #[structopt(long, help = "Generated test config path.")]
    test_config_output_filename: String,
}

fn generate_bash_script(args: &Args) -> std::io::Result<()> {
    let mut file = File::create(&args.script_output_filename)?;
    file.write_all(b"#!/bin/bash\n")?;
    file.write_all(b"\n")?;
    file.write_all(
        format!(
            "{} --fuchsia-test-bin-path {} --fuchsia_test_configuration {}\n",
            args.test_pilot, args.bin_path, args.test_config_output_filename
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn main() -> std::io::Result<()> {
    let args = Args::from_args();

    assert!(Path::new(&args.component_manifest_path).exists());
    assert!(Path::new(&args.partial_test_config).exists());
    assert!(Path::new(&args.test_component_config).exists());

    generate_bash_script(&args)?;

    // Create a blank config.
    // TODO(https://fxbug.dev/327640651): Generate full test config.
    File::create(&args.test_config_output_filename)?;
    Ok(())
}
