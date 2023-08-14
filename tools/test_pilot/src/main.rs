// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use structopt::StructOpt;

mod opts;
mod test_config;

fn main() -> Result<(), Error> {
    let args: opts::CommandLineArgs = opts::CommandLineArgs::from_args();
    let test_config = parse_test_config(&args)?;
    println!("{:#?}", args);
    println!("{:#?}", test_config);
    Ok(())
}

pub fn parse_test_config(
    args: &opts::CommandLineArgs,
) -> Result<test_config::TestConfiguration, Error> {
    test_config::TestConfiguration::try_from(args.test_config.as_path())
}
