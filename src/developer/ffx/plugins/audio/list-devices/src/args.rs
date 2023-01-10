// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Result},
    argh::FromArgs,
    ffx_core::ffx_command,
    std::str::FromStr,
};

/// TODO(fxbug.dev/109807) - Add support for writing infinite files.
#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "list-devices",
    description = "Prints all available audio devices on target",
    example = "ffx audio list-devices --output text"
)]
pub struct ListDevicesCommand {
    #[argh(
        option,
        description = "output format: accepted options are 'text' for readable text, or 'json' for a JSON dictionary",
        default = "default_output_format()"
    )]
    pub output: InfoOutputFormat,
}

#[derive(Debug, PartialEq)]
pub enum InfoOutputFormat {
    Json,
    Text,
}

impl FromStr for InfoOutputFormat {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_lowercase().as_str() {
            "json" => Ok(InfoOutputFormat::Json),
            "text" => Ok(InfoOutputFormat::Text),
            _ => Err(anyhow!("invalid format argument, {}. Expected one of: JSON, text", s)),
        }
    }
}

fn default_output_format() -> InfoOutputFormat {
    InfoOutputFormat::Text
}
