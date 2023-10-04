// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use structopt::StructOpt;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let opts = inspect_testing::Options::from_args();
    if opts.rows == 0 || opts.columns == 0 {
        inspect_testing::Options::clap().print_help()?;
        std::process::exit(1);
    }
    inspect_testing::emit_example_inspect_data(opts).await
}
