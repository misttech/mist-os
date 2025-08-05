// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_config::{ConfigLevel, EnvironmentContext};
use ffx_target_default_args::{SubCommand, TargetDefaultCommand};
use ffx_writer::{SimpleWriter, ToolIO};
use fho::{FfxMain, FfxTool};

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
        exec_target_default_impl(&self.context, self.cmd, &mut writer).await?;
        Ok(())
    }
}

const TARGET_GET_NO_TARGET_MSG: &str = "\
No default target.\n\
If exactly one target is connected, ffx will use that.\n";

pub async fn exec_target_default_impl<W: std::io::Write + ToolIO>(
    context: &EnvironmentContext,
    cmd: TargetDefaultCommand,
    writer: &mut W,
) -> Result<()> {
    match &cmd.subcommand {
        SubCommand::Get(_) => {
            // get_target_specifier can be overridden by `-t|--target` and it
            // seems more reasonable for `ffx target default get` to just ignore
            // that flag.
            let target = context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(ConfigLevel::Default))
                .get_optional::<Option<String>>()?;
            match target {
                Some(target) if !target.is_empty() => writeln!(writer, "{}", target),
                _ => write!(writer.stderr(), "{}", TARGET_GET_NO_TARGET_MSG),
            }?
        }
    };
    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::test_env;
    use ffx_target_default_args::*;
    use ffx_writer::TestBuffers;
    use tempfile::tempdir;

    #[fuchsia::test]
    async fn test_get_env_unset() -> Result<()> {
        let env = test_env().build().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Get(TargetDefaultGetCommand {}) },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_GET_NO_TARGET_MSG);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_env_empty() -> Result<()> {
        let env = test_env()
            .env_var("FUCHSIA_NODENAME", "")
            .env_var("FUCHSIA_DEVICE_ADDR", "")
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Get(TargetDefaultGetCommand {}) },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_GET_NO_TARGET_MSG);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_no_env() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .runtime_config(TARGET_DEFAULT_KEY, "distraction-target1")
            .in_tree(&test_build_dir.path())
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("distraction-target2".into())
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Build))
            .set("distraction-target3".into())
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Global))
            .set("distraction-target4".into())
            .expect("default target setting");

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Get(TargetDefaultGetCommand {}) },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_GET_NO_TARGET_MSG);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_all() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .in_tree(&test_build_dir.path())
            .env_var("FUCHSIA_NODENAME", "stateless-nodename-target")
            .env_var("FUCHSIA_DEVICE_ADDR", "stateless-device-addr-target")
            .runtime_config(TARGET_DEFAULT_KEY, "distraction-target1")
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("distraction-target2".into())
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Build))
            .set("distraction-target3".into())
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Global))
            .set("distraction-target4".into())
            .expect("default target setting");

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Get(TargetDefaultGetCommand {}) },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "stateless-device-addr-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }
}
