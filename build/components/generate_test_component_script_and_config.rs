// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::io::prelude::*;
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
}

fn generate_bash_script(args: &Args) -> std::io::Result<()> {
    let mut file = File::create(&args.script_output_filename)?;
    file.write_all(b"#!/bin/bash\n")?;
    file.write_all(b"\n")?;
    file.write_all(format!("FUCHSIA_BIN_PATH={} {}\n", args.bin_path, args.test_pilot).as_bytes())?;
    Ok(())
}

// TODO(https://fxbug.dev/327640651): Generate test config as well.
fn main() -> std::io::Result<()> {
    let args = Args::from_args();
    generate_bash_script(&args)?;
    Ok(())
}
