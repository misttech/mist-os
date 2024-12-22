// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_config::api::ConfigError;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_config::EnvironmentContext;
use ffx_target_default_args::{SubCommand, TargetDefaultCommand, TargetDefaultGetCommand};
use fho::{user_error, FfxMain, FfxTool, SimpleWriter};

#[derive(FfxTool)]
pub struct TargetDefaultTool {
    #[command]
    cmd: TargetDefaultCommand,
    context: EnvironmentContext,
}

fho::embedded_plugin!(TargetDefaultTool);

#[async_trait(?Send)]
impl FfxMain for TargetDefaultTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        exec_target_default_impl(self.context, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn exec_target_default_impl<W: std::io::Write>(
    context: EnvironmentContext,
    cmd: TargetDefaultCommand,
    writer: &mut W,
) -> Result<()> {
    match &cmd.subcommand {
        SubCommand::Get(TargetDefaultGetCommand { level: Some(level), build_dir: _ }) => {
            let res: String = context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(*level))
                .get()
                .unwrap_or_else(|_| "".to_owned());
            writeln!(writer, "{}", res)?;
        }
        SubCommand::Get(_) => {
            let res: String = context.get(TARGET_DEFAULT_KEY).unwrap_or_else(|_| "".to_owned());
            writeln!(writer, "{}", res)?;
        }
        SubCommand::Set(set) => {
            context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(set.level))
                .set(serde_json::Value::String(set.nodename.clone()))
                .await?
        }
        SubCommand::Unset(unset) => {
            match context.query(TARGET_DEFAULT_KEY).level(Some(unset.level)).remove().await {
                Ok(()) => {
                    // TODO(b/351861659): Re enable this if/when the exception process is finalized
                    // eprintln!("Successfully unset the {} level default target.", unset.level);
                    Ok(())
                }
                ref res @ Err(ref _e) => {
                    let err = res.as_ref().unwrap_err();
                    Err(if let Some(config_err) = err.downcast_ref::<ConfigError>() {
                        match config_err {
                            ConfigError::Error(e) => {
                                user_error!(
                                    "Failed to unset the {} level default target.\n{}",
                                    unset.level,
                                    e
                                )
                            }
                            ConfigError::KeyNotFound | ConfigError::EmptyKey => {
                                user_error!("No {} level default target to unset.", unset.level)
                            }
                            ConfigError::BadValue { .. } => {
                                // Only occurs in strict mode, and ffx config doesn't support strict
                                unreachable!()
                            }
                        }
                    } else {
                        user_error!(
                            "Failed to unset the {} level default target.\n{}",
                            unset.level,
                            err
                        )
                    })
                }
            }
        }?,
    };
    Ok(())
}
