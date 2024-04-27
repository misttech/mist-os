// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `cmc` is the Component Manifest Compiler.

use anyhow::Error;
use cmc::{opts, run_cmc};
use structopt::StructOpt;

fn main() -> Result<(), Error> {
    let opt = opts::Opt::from_args();
    run_cmc(opt)
}
