// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Error};
use camino::Utf8PathBuf;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
pub struct Opt {
    #[structopt(short = "i", long = "input")]
    // Path to the tests.json file.
    pub input: Utf8PathBuf,

    #[structopt(short = "t", long = "test-components")]
    // Path to the test_components.json file.
    pub test_components_list: Utf8PathBuf,

    #[structopt(short = "o", long = "output")]
    // Path to output test-list.
    pub output: Utf8PathBuf,

    #[structopt(short = "b", long = "build-dir")]
    // Path to the build directory.
    pub build_dir: Utf8PathBuf,

    #[structopt(short = "d", long = "depfile")]
    // Path to output a depfile.
    pub depfile: Option<Utf8PathBuf>,

    #[structopt(long = "experimental-test-config")]
    /// This is experimental as we are just prototyping
    /// TODO(b/294567466): Eventually this will replace test-list.json.
    pub test_config_output: Option<Utf8PathBuf>,
}

impl Opt {
    pub fn validate(&self) -> Result<(), Error> {
        ensure!(self.input.exists(), "input {:?} does not exist", self.input);
        ensure!(
            self.test_components_list.exists(),
            "test_components_list {:?} does not exist",
            self.test_components_list
        );
        ensure!(self.build_dir.exists(), "build-dir {:?} does not exist", self.build_dir);
        Ok(())
    }
}
