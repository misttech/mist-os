// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_config::api::ConfigError;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_config::{ConfigLevel, EnvironmentContext};
use ffx_target_default_args::{SubCommand, TargetDefaultCommand};
use ffx_writer::{SimpleWriter, ToolIO};
use fho::{user_error, FfxMain, FfxTool};

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

pub async fn exec_target_default_impl<W: std::io::Write + ToolIO>(
    context: &EnvironmentContext,
    cmd: TargetDefaultCommand,
    writer: &mut W,
) -> Result<()> {
    match &cmd.subcommand {
        SubCommand::Get(_) => {
            let res =
                ffx_target::get_target_specifier(&context).await?.unwrap_or_else(|| "".into());
            writeln!(writer, "{}", res)?;
        }
        SubCommand::Set(args) => {
            context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(ConfigLevel::User))
                .set(serde_json::Value::String(args.nodename.clone()))
                .await?
        }
        SubCommand::Unset(_) => {
            // Used for checking for the effectiveness of this command.
            // See https://fxbug.dev/394386370 for more details.
            let default_from_env = context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(ConfigLevel::Default))
                .get_optional::<Option<String>>()
                .or_else(|e| {
                    writeln!(
                        writer.stderr(),
                        "Failed to query the default target at the default \
                        configuration level: {:#?}\n\
                        Default targets can also be set via $FUCHSIA_NODENAME or
                        $FUCHSIA_DEVICE_ADDR but this tool can't account for
                        that, so please check whether those environment
                        variables are empty.",
                        e
                    )?;
                    Ok::<Option<String>, std::io::Error>(None)
                })?
                .unwrap_or_else(|| "".into());
            let mut did_unset = false;
            for level in [ConfigLevel::User, ConfigLevel::Global, ConfigLevel::Build] {
                did_unset |= ensure_unset_at_level(context, level).await?;
            }
            match (default_from_env.as_str(), did_unset) {
                ("", false) => {
                    writeln!(writer.stderr(), "No default targets to unset.")?;
                    Ok(())
                }
                ("", true) => Ok(()),
                (target, false) => Err(user_error!(
                    "Cannot unset default target {}.\n{}",
                    default_target_env_source(context)?,
                    unset_target_env_message(context, target),
                )),
                (target, true) => Err(user_error!(
                    "The current default target has been unset from ffx \
                    configuration, but the default {} cannot be unset by this \
                    tool.\n{}",
                    default_target_env_source(context)?,
                    unset_target_env_message(context, target),
                )),
            }?
        }
    };
    Ok(())
}

fn default_target_env_source(context: &EnvironmentContext) -> Result<String> {
    match (
        context.env_var("FUCHSIA_NODENAME").unwrap_or_else(|_| "".into()).as_str(),
        context.env_var("FUCHSIA_DEVICE_ADDR").unwrap_or_else(|_| "".into()).as_str(),
    ) {
        ("", "") => ffx_bail!(
            "{TARGET_DEFAULT_KEY} is set at ConfigLevel::Default, but neither \
            $FUCHSIA_NODENAME nor $FUCHSIA_DEVICE_ADDR are set."
        ),
        (value, "") => Ok(format!("$FUCHSIA_NODENAME=\"{value}\"")),
        ("", value) => Ok(format!("$FUCHSIA_DEVICE_ADDR=\"{value}\"")),
        (value1, value2) => {
            Ok(format!("$FUCHSIA_NODENAME=\"{value1}\" and $FUCHSIA_DEVICE_ADDR=\"{value2}\""))
        }
    }
}

fn unset_target_env_message(context: &EnvironmentContext, env_default: &str) -> String {
    if context.is_in_tree() {
        format!(
            "Please run `fx unset-device` and/or manually unset the \
            environment variable(s) to unset \"{env_default}\"."
        )
    } else {
        format!(
            "Please manually unset the environment variable(s) to unset \
            \"{env_default}\"."
        )
    }
}

async fn ensure_unset_at_level(context: &EnvironmentContext, level: ConfigLevel) -> Result<bool> {
    match context.query(TARGET_DEFAULT_KEY).level(Some(level)).remove().await {
        Ok(()) => {
            // TODO(b/351861659): Re enable this if/when the exception process is finalized
            // eprintln!("Successfully unset the {} level default target.", level);
            Ok(true)
        }
        ref res @ Err(ref _e) => {
            let err = res.as_ref().unwrap_err();
            match err.downcast_ref::<ConfigError>() {
                Some(ConfigError::KeyNotFound) | Some(ConfigError::EmptyKey) => Ok(false),
                Some(ConfigError::UnconfiguredLevel { level }) => {
                    // Can happen at ConfigLevel::Build in non in-tree cases.
                    tracing::warn!(
                        "Failed to unset the {} level default target \
                        configuration level.\n{:?}",
                        level,
                        err
                    );
                    Ok(false)
                }
                Some(ConfigError::Error(e)) => {
                    ffx_bail!("Failed to unset the {} level default target.\n{}", level, e)
                }
                Some(ConfigError::BadValue { .. }) => {
                    // Only occurs in strict mode when a ConfigQuery attempts to
                    // interpolate an environment variable.
                    // Unreachable since we're unsetting a value rather than
                    // getting a value.
                    unreachable!()
                }
                None => {
                    ffx_bail!("Failed to unset the {} level default target.\n{}", level, err)
                }
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::{test_env, test_init};
    use ffx_target_default_args::*;
    use ffx_writer::TestBuffers;
    use tempfile::tempdir;

    #[fuchsia::test]
    async fn test_get_no_default() -> Result<()> {
        let env = test_init().await.unwrap();
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
        assert_eq!(stdout, "\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_configuration() -> Result<()> {
        let env = test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("foo-target".into())
            .await
            .expect("default target setting");

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Get(TargetDefaultGetCommand {}) },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "foo-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_env_fuchsia_nodename() -> Result<()> {
        let env = test_env().env_var("FUCHSIA_NODENAME", "bar-target").build().await.unwrap();
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
        assert_eq!(stdout, "bar-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_env_fuchsia_device_addr() -> Result<()> {
        let env = test_env().env_var("FUCHSIA_DEVICE_ADDR", "baz-target").build().await.unwrap();
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
        assert_eq!(stdout, "baz-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_env_and_configuration() -> Result<()> {
        let env = test_env()
            .env_var("FUCHSIA_NODENAME", "env1-target")
            .env_var("FUCHSIA_DEVICE_ADDR", "env2-target")
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("config-target".into())
            .await
            .expect("default target setting");

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Get(TargetDefaultGetCommand {}) },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "config-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_set() -> Result<()> {
        let env = test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        exec_target_default_impl(
            &env.context,
            TargetDefaultCommand {
                subcommand: SubCommand::Set(TargetDefaultSetCommand {
                    nodename: "foo-set-device".into(),
                }),
            },
            &mut writer,
        )
        .await
        .unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");

        let config_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");

        assert_eq!(config_default_target, Some("foo-set-device".into()));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_no_config_no_env() -> Result<()> {
        let env = test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "No default targets to unset.\n");
        assert!(result.is_ok());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_no_config_fuchsia_nodename_isolated() -> Result<()> {
        let env = test_env().env_var("FUCHSIA_NODENAME", "foo-unset-target").build().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Cannot unset default target \
            $FUCHSIA_NODENAME=\"foo-unset-target\".\n\
            Please manually unset the environment variable(s) to unset \
            \"foo-unset-target\".",
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_no_config_fuchsia_device_addr_in_tree() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .env_var("FUCHSIA_DEVICE_ADDR", "bar-unset-target")
            .in_tree(test_build_dir.path())
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Cannot unset default target \
            $FUCHSIA_DEVICE_ADDR=\"bar-unset-target\".\n\
            Please run `fx unset-device` and/or manually unset the environment \
            variable(s) to unset \"bar-unset-target\".",
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_with_config_no_env() -> Result<()> {
        let env = test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("bar-unset-target".into())
            .await
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_ok());

        let user_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(user_default_target, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_with_config_both_envs_in_tree() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .env_var("FUCHSIA_NODENAME", "baz-unset-target1")
            .env_var("FUCHSIA_DEVICE_ADDR", "baz-unset-target2")
            .in_tree(test_build_dir.path())
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("foo-unset-target".into())
            .await
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "The current default target has been unset from ffx configuration, \
            but the default $FUCHSIA_NODENAME=\"baz-unset-target1\" and \
            $FUCHSIA_DEVICE_ADDR=\"baz-unset-target2\" cannot be unset by this \
            tool.\n\
            Please run `fx unset-device` and/or manually unset the environment \
            variable(s) to unset \"baz-unset-target2\".",
        );

        let user_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(user_default_target, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_with_config_both_envs_isolated() -> Result<()> {
        let env = test_env()
            .env_var("FUCHSIA_NODENAME", "baz-unset-target1")
            .env_var("FUCHSIA_DEVICE_ADDR", "baz-unset-target2")
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("foo-unset-target".into())
            .await
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "The current default target has been unset from ffx configuration, \
            but the default $FUCHSIA_NODENAME=\"baz-unset-target1\" and \
            $FUCHSIA_DEVICE_ADDR=\"baz-unset-target2\" cannot be unset by this \
            tool.\n\
            Please manually unset the environment variable(s) to unset \
            \"baz-unset-target2\".",
        );

        let user_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(user_default_target, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_with_config_all_levels() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env().in_tree(&test_build_dir.path()).build().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("foo-unset-user-target".into())
            .await
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Build))
            .set("foo-unset-build-target".into())
            .await
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Global))
            .set("foo-unset-global-target".into())
            .await
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_ok());

        let user_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");
        let build_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Build))
            .get_optional::<Option<String>>()
            .expect("query default target");
        let global_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Global))
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(user_default_target, None);
        assert_eq!(build_default_target, None);
        assert_eq!(global_default_target, None);

        let effective_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(effective_default_target, None);

        Ok(())
    }
}
