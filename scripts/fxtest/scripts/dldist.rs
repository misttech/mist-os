// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::FromArgs;
use std::fs::read_to_string;
use std::path::PathBuf;

/// Command to match a list of strings using Damerau–Levenshtein distance.
#[derive(Debug, FromArgs)]
struct Args {
    /// value to search for
    #[argh(option)]
    needle: String,

    /// path of file containing strings to match, one per line
    #[argh(option)]
    input: PathBuf,

    /// if set, return a perfect match if needle is a prefix of any input
    #[argh(switch)]
    match_prefixes: bool,

    /// if set, return a perfect match if needle is contained in any input
    #[argh(switch)]
    match_contains: bool,

    /// if set, print verbose debugging to stderr
    #[argh(switch, short = 'v')]
    verbose: bool,
}

const PERFECT_MATCH: usize = 0;

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let contents = read_to_string(args.input)?;
    for line in contents.lines() {
        let val = if args.match_prefixes && line.starts_with(&args.needle) {
            PERFECT_MATCH
        } else if args.match_contains && line.contains(&args.needle) {
            PERFECT_MATCH
        } else {
            strsim::damerau_levenshtein(&args.needle, line)
        };
        println!("{val}");
        if args.verbose {
            eprintln!("'{line}' = {val}");
        }
    }
    Ok(())
}
