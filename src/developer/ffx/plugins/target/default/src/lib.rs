// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_config::api::ConfigError;
use ffx_config::keys::{STATELESS_DEFAULT_TARGET_CONFIGURATION, TARGET_DEFAULT_KEY};
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

const TARGET_GET_NO_TARGET_MSG: &str = "\
No default target.\n\
If exactly one target is connected, ffx will use that.\n";

const TARGET_SET_DEPRECATION_OOT_WARNING_MSG: &str = "\
WARN: `ffx target default set` will be deprecated soon. (https://fxbug.dev/394619603)\n\
WARN: Please set the `$FUCHSIA_NODENAME` environment variable in the future.\n";
const TARGET_SET_DEPRECATION_OOT_ERROR_MSG: &str = "\
ERROR: `ffx target default set` has been deprecated. (https://fxbug.dev/394619603)\n\
ERROR: Please set the `$FUCHSIA_NODENAME` environment variable instead.";
const TARGET_SET_DEPRECATION_IN_TREE_WARNING_MSG: &str = "\
WARN: `ffx target default set` will be deprecated soon. (https://fxbug.dev/394619603)\n\
WARN: Please use `fx set-device` and `fx unset-device` in the future.\n";
const TARGET_SET_DEPRECATION_IN_TREE_ERROR_MSG: &str = "\
ERROR: `ffx target default set` has been deprecated. (https://fxbug.dev/394619603)\n\
ERROR: Please use `fx set-device` and `fx unset-device` instead.";

// We should be a bit more gentle with `unset` warnings, since there's a
// legitimate use-case for `ffx target default unset` until we really start
// bypassing ConfigLevel::{User, Build, Global} for default targets.
// So for now, let's bias this warning towards deterring usages of
// `ffx target default set` since that's the real source of most users problems.
const TARGET_UNSET_DEPRECATION_OOT_WARNING_MSG: &str = "\
WARN: `ffx target default set/unset` will be deprecated soon. (https://fxbug.dev/394619603)\n\
WARN: Please set/unset the `$FUCHSIA_NODENAME` environment variable in the future.\n";
const TARGET_UNSET_DEPRECATION_OOT_ERROR_MSG: &str = "\
ERROR: `ffx target default unset` has been deprecated. (https://fxbug.dev/394619603)\n\
ERROR: Please unset the `$FUCHSIA_NODENAME` environment variable instead.";
const TARGET_UNSET_DEPRECATION_IN_TREE_WARNING_MSG: &str = "\
WARN: `ffx target default set/unset` will be deprecated soon. (https://fxbug.dev/394619603)\n\
WARN: Please use `fx set-device` and `fx unset-device` in the future.\n";
const TARGET_UNSET_DEPRECATION_IN_TREE_ERROR_MSG: &str = "\
ERROR: `ffx target default unset` has been deprecated. (https://fxbug.dev/394619603)\n\
ERROR: Please use `fx unset-device` and `fx set-device` instead.";

pub async fn exec_target_default_impl<W: std::io::Write + ToolIO>(
    context: &EnvironmentContext,
    cmd: TargetDefaultCommand,
    writer: &mut W,
) -> Result<()> {
    match &cmd.subcommand {
        SubCommand::Get(_) => {
            let target = if context.get(STATELESS_DEFAULT_TARGET_CONFIGURATION)? {
                // get_target_specifier can be overridden by `-t|--target` and
                // it seems more reasonable for `ffx target default get` to just
                // ignore that flag.
                context
                    .query(TARGET_DEFAULT_KEY)
                    .level(Some(ConfigLevel::Default))
                    .get_optional::<Option<String>>()?
            } else {
                ffx_target::get_target_specifier(&context).await?
            };
            match target {
                Some(target) if !target.is_empty() => writeln!(writer, "{}", target),
                _ => write!(writer.stderr(), "{}", TARGET_GET_NO_TARGET_MSG),
            }?
        }

        SubCommand::Set(args) => {
            check_target_set_deprecation(&context, writer)?;
            context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(ConfigLevel::User))
                .set(serde_json::Value::String(args.nodename.clone()))?;
        }

        SubCommand::Unset(_) => {
            check_target_unset_deprecation(&context, writer)?;
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

// Returns a user error with an appropriate error message if `ffx target set`
// has been disabled by `target.stateless_default_configuration=true`.
// Might issue a warning of impending deprecation if the flag is disabled.
fn check_target_set_deprecation<W: std::io::Write + ToolIO>(
    context: &EnvironmentContext,
    writer: &mut W,
) -> Result<()> {
    match (context.get(STATELESS_DEFAULT_TARGET_CONFIGURATION)?, context.is_in_tree()) {
        (false, false) => {
            write!(writer.stderr(), "{}", TARGET_SET_DEPRECATION_OOT_WARNING_MSG)?;
            Ok(())
        }
        (false, true) => {
            write!(writer.stderr(), "{}", TARGET_SET_DEPRECATION_IN_TREE_WARNING_MSG)?;
            Ok(())
        }
        (true, false) => Err(user_error!(TARGET_SET_DEPRECATION_OOT_ERROR_MSG))?,
        (true, true) => Err(user_error!(TARGET_SET_DEPRECATION_IN_TREE_ERROR_MSG))?,
    }
}

// Returns a user error with an appropriate error message if `ffx target unset`
// has been disabled by `target.stateless_default_configuration=true`.
// Might issue a warning of impending deprecation if the flag is disabled.
fn check_target_unset_deprecation<W: std::io::Write + ToolIO>(
    context: &EnvironmentContext,
    writer: &mut W,
) -> Result<()> {
    match (context.get(STATELESS_DEFAULT_TARGET_CONFIGURATION)?, context.is_in_tree()) {
        (false, false) => {
            write!(writer.stderr(), "{}", TARGET_UNSET_DEPRECATION_OOT_WARNING_MSG)?;
            Ok(())
        }
        (false, true) => {
            write!(writer.stderr(), "{}", TARGET_UNSET_DEPRECATION_IN_TREE_WARNING_MSG)?;
            Ok(())
        }
        (true, false) => Err(user_error!(TARGET_UNSET_DEPRECATION_OOT_ERROR_MSG))?,
        (true, true) => Err(user_error!(TARGET_UNSET_DEPRECATION_IN_TREE_ERROR_MSG))?,
    }
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
    match context.query(TARGET_DEFAULT_KEY).level(Some(level)).remove() {
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
                    log::warn!(
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

    #[fuchsia::test]
    async fn test_set_oot_deprecation_error() -> Result<()> {
        let env = test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand {
                subcommand: SubCommand::Set(TargetDefaultSetCommand {
                    nodename: "foo-set-device".into(),
                }),
            },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), TARGET_SET_DEPRECATION_OOT_ERROR_MSG);

        let config_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");

        assert_eq!(config_default_target, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_set_in_tree_deprecation_error() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env().in_tree(test_build_dir.path()).build().await.unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand {
                subcommand: SubCommand::Set(TargetDefaultSetCommand {
                    nodename: "foo-set-device".into(),
                }),
            },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), TARGET_SET_DEPRECATION_IN_TREE_ERROR_MSG);

        let config_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");

        assert_eq!(config_default_target, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_no_default_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
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
    async fn test_unset_oot_deprecation_error() -> Result<()> {
        let env = test_env()
            .env_var("FUCHSIA_NODENAME", "distraction-target1")
            .env_var("FUCHSIA_DEVICE_ADDR", "distraction-target2")
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("distraction-target3".into())
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
        assert_eq!(result.unwrap_err().to_string(), TARGET_UNSET_DEPRECATION_OOT_ERROR_MSG);

        let user_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(user_default_target, Some("distraction-target3".into()));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_in_tree_deprecation_error() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .env_var("FUCHSIA_NODENAME", "distraction-target1")
            .env_var("FUCHSIA_DEVICE_ADDR", "distraction-target2")
            .in_tree(test_build_dir.path())
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("distraction-target3".into())
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
        assert_eq!(result.unwrap_err().to_string(), TARGET_UNSET_DEPRECATION_IN_TREE_ERROR_MSG);

        let user_default_target = env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .get_optional::<Option<String>>()
            .expect("query default target");
        assert_eq!(user_default_target, Some("distraction-target3".into()));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_configuration_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("foo-target".into())
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
    async fn test_get_env_fuchsia_nodename_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .env_var("FUCHSIA_NODENAME", "bar-target")
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
        assert_eq!(stdout, "bar-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_env_fuchsia_device_addr_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .env_var("FUCHSIA_DEVICE_ADDR", "baz-target")
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
        assert_eq!(stdout, "baz-target\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_get_env_and_configuration_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
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
    async fn test_set_oot_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .build()
            .await
            .unwrap();
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
        assert_eq!(stderr, TARGET_SET_DEPRECATION_OOT_WARNING_MSG);

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
    async fn test_set_in_tree_stateful() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .in_tree(test_build_dir.path())
            .build()
            .await
            .unwrap();
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
        assert_eq!(stderr, TARGET_SET_DEPRECATION_IN_TREE_WARNING_MSG);

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
    async fn test_unset_no_config_no_env_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
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
        assert_eq!(
            stderr,
            format!("{}No default targets to unset.\n", TARGET_UNSET_DEPRECATION_OOT_WARNING_MSG)
        );
        assert!(result.is_ok());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_unset_no_config_fuchsia_nodename_isolated_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .env_var("FUCHSIA_NODENAME", "foo-unset-target")
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
        assert_eq!(stderr, TARGET_UNSET_DEPRECATION_OOT_WARNING_MSG);
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
    async fn test_unset_no_config_fuchsia_device_addr_in_tree_stateful() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
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
        assert_eq!(stderr, TARGET_UNSET_DEPRECATION_IN_TREE_WARNING_MSG);
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
    async fn test_unset_with_config_no_env_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("bar-unset-target".into())
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_UNSET_DEPRECATION_OOT_WARNING_MSG);
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
    async fn test_unset_with_config_both_envs_in_tree_stateful() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
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
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_UNSET_DEPRECATION_IN_TREE_WARNING_MSG);
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
    async fn test_unset_with_config_both_envs_isolated_stateful() -> Result<()> {
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
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
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_UNSET_DEPRECATION_OOT_WARNING_MSG);
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
    async fn test_unset_with_config_all_levels_stateful() -> Result<()> {
        let test_build_dir = tempdir().expect("output directory");
        let env = test_env()
            .runtime_config(STATELESS_DEFAULT_TARGET_CONFIGURATION, false)
            .in_tree(&test_build_dir.path())
            .build()
            .await
            .unwrap();
        let test_buffers = TestBuffers::default();
        let mut writer = SimpleWriter::new_test(&test_buffers);

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set("foo-unset-user-target".into())
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Build))
            .set("foo-unset-build-target".into())
            .expect("default target setting");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::Global))
            .set("foo-unset-global-target".into())
            .expect("default target setting");

        let result = exec_target_default_impl(
            &env.context,
            TargetDefaultCommand { subcommand: SubCommand::Unset(TargetDefaultUnsetCommand {}) },
            &mut writer,
        )
        .await;

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, TARGET_UNSET_DEPRECATION_IN_TREE_WARNING_MSG);
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
