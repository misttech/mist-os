// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::{self, FromArgs};
use std::fs::read_to_string;
use std::path::PathBuf;
use strsim;

/// Command to match a list of strings using Damerau–Levenshtein distance.
#[derive(Debug, FromArgs)]
struct Args {
    /// value to search for
    #[argh(option)]
    needle: String,

    /// path of file containing strings to match, one per line
    #[argh(option)]
    input: PathBuf,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let contents = read_to_string(args.input)?;
    for line in contents.lines() {
        let val = strsim::damerau_levenshtein(&args.needle, line);
        println!("{}", val)
    }
    Ok(())
}
