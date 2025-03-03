// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use argh::{ArgsInfo, FromArgs};
use sampler_config::assembly::MergedSamplerConfig;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;

/// Diagnostics config command
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
pub struct MergeConfigsCommand {
    /// paths to sampler project configs.
    #[argh(option)]
    pub project_config: Vec<PathBuf>,

    /// paths to sampler project templates.
    #[argh(option)]
    pub fire_project_template: Vec<PathBuf>,

    /// paths to sampler component configs.
    #[argh(option)]
    pub fire_component_config: Vec<PathBuf>,

    /// path to which the result will be written.
    #[argh(option)]
    pub output: PathBuf,
}

pub fn main() -> Result<(), Error> {
    let args: MergeConfigsCommand = argh::from_env();

    let mut config = MergedSamplerConfig::default();

    for project_config_path in args.project_config {
        config.project_configs.push(read_file(project_config_path)?);
    }
    for project_template_path in args.fire_project_template {
        config.fire_project_templates.push(read_file(project_template_path)?);
    }
    for component_config_path in args.fire_component_config {
        config.fire_component_configs.push(read_file(component_config_path)?);
    }

    write_file(args.output, config)?;

    Ok(())
}

fn read_file<T: DeserializeOwned>(path: PathBuf) -> anyhow::Result<T> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let result: T = serde_json5::from_reader(&mut reader)?;
    Ok(result)
}

fn write_file<T: Serialize>(path: PathBuf, value: T) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    serde_json5::to_writer(&mut writer, &value)?;
    writer.flush()?;
    Ok(())
}
