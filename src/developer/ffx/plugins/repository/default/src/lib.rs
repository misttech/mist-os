// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_repository_default_args::{RepositoryDefaultCommand, SubCommand};
use ffx_writer::{SimpleWriter, ToolIO};
use fho::{bug, FfxMain, FfxTool, Result};

pub(crate) const CONFIG_KEY_DEFAULT: &str = "repository.default";

#[derive(FfxTool)]
pub struct RepoDefaultTool {
    #[command]
    pub cmd: RepositoryDefaultCommand,
}

fho::embedded_plugin!(RepoDefaultTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for RepoDefaultTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        exec_repository_default_impl(self.cmd, &mut writer).await
    }
}

pub async fn exec_repository_default_impl<W: std::io::Write + ToolIO>(
    cmd: RepositoryDefaultCommand,
    writer: &mut W,
) -> Result<()> {
    match &cmd.subcommand {
        SubCommand::Get(_) => {
            let res: String = ffx_config::get(CONFIG_KEY_DEFAULT).unwrap_or_else(|_| "".to_owned());
            writeln!(writer, "{}", res).map_err(|e| bug!(e))?;
        }
        SubCommand::Set(set) => {
            ffx_config::query(CONFIG_KEY_DEFAULT)
                .level(Some(set.level))
                .set(serde_json::Value::String(set.name.clone()))
                .await?
        }
        SubCommand::Unset(unset) => {
            let _ = ffx_config::query(CONFIG_KEY_DEFAULT)
                .level(Some(unset.level))
                .remove()
                .await
                .map_err(|e| writeln!(writer.stderr(), "warning: {}", e));
        }
    };
    Ok(())
}
