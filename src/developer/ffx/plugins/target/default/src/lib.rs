// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_target_default_args::{SubCommand, TargetDefaultCommand, TargetDefaultGetCommand};
use fho::{FfxMain, FfxTool, SimpleWriter};

#[derive(FfxTool)]
pub struct TargetDefaultTool {
    #[command]
    cmd: TargetDefaultCommand,
}

fho::embedded_plugin!(TargetDefaultTool);

#[async_trait(?Send)]
impl FfxMain for TargetDefaultTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        exec_target_default_impl(self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn exec_target_default_impl<W: std::io::Write>(
    cmd: TargetDefaultCommand,
    writer: &mut W,
) -> Result<()> {
    match &cmd.subcommand {
        SubCommand::Get(TargetDefaultGetCommand { level: Some(level), build_dir }) => {
            let res: String = ffx_config::query(TARGET_DEFAULT_KEY)
                .level(Some(*level))
                .build(build_dir.as_deref().map(|dir| dir.into()))
                .get()
                .await
                .unwrap_or("".to_owned());
            writeln!(writer, "{}", res)?;
        }
        SubCommand::Get(_) => {
            let res: String = ffx_config::get(TARGET_DEFAULT_KEY).await.unwrap_or("".to_owned());
            writeln!(writer, "{}", res)?;
        }
        SubCommand::Set(set) => {
            ffx_config::query(TARGET_DEFAULT_KEY)
                .level(Some(set.level))
                .build(set.build_dir.as_deref().map(|dir| dir.into()))
                .set(serde_json::Value::String(set.nodename.clone()))
                .await?
        }
        SubCommand::Unset(unset) => {
            let _ = ffx_config::query(TARGET_DEFAULT_KEY)
                .level(Some(unset.level))
                .build(unset.build_dir.as_deref().map(|dir| dir.into()))
                .remove()
                .await
                .map_err(|e| eprintln!("warning: {}", e));
        }
    };
    Ok(())
}
