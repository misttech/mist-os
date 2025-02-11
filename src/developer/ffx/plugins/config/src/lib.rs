// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use errors::{ffx_bail, ffx_bail_with_code};
use ffx_config::api::ConfigError;
use ffx_config::{
    disable_metrics, enable_basic_metrics, enable_enhanced_metrics, print_config,
    show_metrics_status, ConfigLevel, EnvironmentContext,
};
use ffx_config_plugin_args::{
    AddCommand, AnalyticsCommand, AnalyticsControlCommand, ConfigCommand, EnvAccessCommand,
    EnvCommand, EnvSetCommand, GetCommand, MappingMode, RemoveCommand, SetCommand, SshKeyCommand,
    SubCommand,
};
use ffx_ssh::{SshKeyErrorKind, SshKeyFiles};
use fho::{FfxMain, FfxTool};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::Write;

#[derive(FfxTool)]
pub struct ConfigTool {
    #[command]
    config: ConfigCommand,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(ConfigTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ConfigTool {
    type Writer = ffx_writer::SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        match &self.config.sub {
            SubCommand::CheckSshKeys(check_ssh_cmd) => {
                exec_check_ssh_keys(&self.ctx, check_ssh_cmd, writer).await
            }
            SubCommand::Env(env) => exec_env(&self.ctx, env, writer).await,
            SubCommand::Get(get_cmd) => exec_get(&self.ctx, get_cmd, writer).await,
            SubCommand::Set(set_cmd) => exec_set(&self.ctx, set_cmd).await,
            SubCommand::Remove(remove_cmd) => exec_remove(&self.ctx, remove_cmd).await,
            SubCommand::Add(add_cmd) => exec_add(&self.ctx, add_cmd).await,
            SubCommand::Analytics(analytics_cmd) => exec_analytics(analytics_cmd).await,
        }
        .map_err(fho::Error::from)
    }
}

fn output<W: Write>(mut writer: W, value: Option<Value>) -> Result<()> {
    match value {
        Some(v) => writeln!(writer, "{}", serde_json::to_string_pretty(&v).unwrap())
            .map_err(|e| anyhow!("{}", e)),
        // Use 2 error code so wrapper scripts don't need check for the string to differentiate
        // errors.
        None => ffx_bail_with_code!(2, "Value not found"),
    }
}

fn output_array<W: Write>(
    mut writer: W,
    values: std::result::Result<Vec<Value>, ConfigError>,
) -> Result<()> {
    match values {
        Ok(v) => {
            if v.len() == 1 {
                writeln!(writer, "{}", serde_json::to_string_pretty(&v[0]).unwrap())
                    .map_err(|e| anyhow!("{}", e))
            } else {
                writeln!(writer, "{}", serde_json::to_string_pretty(&Value::Array(v)).unwrap())
                    .map_err(|e| anyhow!("{}", e))
            }
        }
        // Use 2 error code so wrapper scripts don't need check for the string to differentiate
        // errors.
        Err(_) => ffx_bail_with_code!(2, "Value not found"),
    }
}

fn output_first_element<W: Write>(mut writer: W, value: Option<Value>) -> Result<()> {
    match value {
        Some(Value::Array(vals)) => {
            if !vals.is_empty() {
                writeln!(writer, "{}", serde_json::to_string_pretty(&vals[0]).unwrap())
                    .map_err(|e| anyhow!("{}", e))
            } else {
                ffx_bail_with_code!(2, "Value not found")
            }
        }
        Some(v) => writeln!(writer, "{}", serde_json::to_string_pretty(&v).unwrap())
            .map_err(|e| anyhow!("{}", e)),
        // Use 2 error code so wrapper scripts don't need check for the string to differentiate
        // errors.
        None => ffx_bail_with_code!(2, "Value not found"),
    }
}

async fn exec_get<W: Write>(
    ctx: &EnvironmentContext,
    get_cmd: &GetCommand,
    writer: W,
) -> Result<()> {
    match get_cmd.name.as_ref() {
        Some(_) => match get_cmd.process {
            MappingMode::Raw => {
                let value: Option<Value> = get_cmd.query(ctx).get_raw()?;
                output(writer, value)
            }
            MappingMode::Substitute => {
                let value: std::result::Result<Vec<Value>, _> = get_cmd.query(ctx).get();
                output_array(writer, value)
            }
            MappingMode::File => {
                let value = get_cmd.query(ctx).get_file().await?;
                output_first_element(writer, value)
            }
        },
        None => {
            print_config(ctx, writer /*, get_cmd.query().get_build_dir().await.as_deref()*/).await
        }
    }
}

async fn exec_set(ctx: &EnvironmentContext, set_cmd: &SetCommand) -> Result<()> {
    tracing::debug!("Set command running...");
    set_cmd.query(ctx).set(set_cmd.value.clone()).await
}

async fn exec_remove(ctx: &EnvironmentContext, remove_cmd: &RemoveCommand) -> Result<()> {
    let entry = remove_cmd.query(ctx);
    // Check that there is a value before removing it.
    if let Ok(Some(_val)) = entry.get_raw::<Option<Value>>() {
        entry.remove().await
    } else {
        ffx_bail_with_code!(2, "Configuration key not found")
    }
}

async fn exec_add(ctx: &EnvironmentContext, add_cmd: &AddCommand) -> Result<()> {
    add_cmd.query(ctx).add(Value::String(format!("{}", add_cmd.value))).await
}

async fn exec_env_set<W: Write>(
    env_context: &EnvironmentContext,
    mut writer: W,
    s: &EnvSetCommand,
) -> Result<()> {
    let env_file = env_context.env_file_path().context("Getting ffx environment file path")?;

    if !env_file.exists() {
        writeln!(writer, "\"{}\" does not exist, creating empty json file", env_file.display())?;
        let mut file = File::create(&env_file).context("opening write buffer")?;
        file.write_all(b"{}").context("writing configuration file")?;
        if !env_context.env_kind().is_isolated() {
            file.sync_all().context("syncing configuration file to filesystem")?;
        }
    }

    // Double check read/write permissions and create the file if it doesn't exist.
    let _ = OpenOptions::new().read(true).write(true).create(true).open(&s.file)?;

    let mut env = env_context.load().context("Loading environment file")?;

    match &s.level {
        ConfigLevel::User => env.set_user(Some(&s.file)),
        ConfigLevel::Build => env.set_build(&s.file)?,
        ConfigLevel::Global => env.set_global(Some(&s.file)),
        _ => ffx_bail!("This configuration is not stored in the environment."),
    }
    env.save().await
}

async fn exec_env<W: Write>(
    ctx: &EnvironmentContext,
    env_command: &EnvCommand,
    mut writer: W,
) -> Result<()> {
    match &env_command.access {
        Some(a) => match a {
            EnvAccessCommand::Set(s) => exec_env_set(ctx, writer, s).await,
            EnvAccessCommand::Get(g) => {
                writeln!(
                    writer,
                    "{}",
                    &ctx.load().context("Loading environment file")?.display(&g.level)
                )?;
                Ok(())
            }
        },
        None => {
            writeln!(
                writer,
                "{}",
                &ctx.load().context("Loading environment file")?.display(&None)
            )?;
            Ok(())
        }
    }
}

async fn exec_analytics(analytics_cmd: &AnalyticsCommand) -> Result<()> {
    let writer = Box::new(std::io::stdout());
    match &analytics_cmd.sub {
        AnalyticsControlCommand::EnableEnhanced(_) => {
            enable_enhanced_metrics().await.with_context(|| "Failed to enable metrics")?;
            show_metrics_status(writer).await.with_context(|| "Failed to read metrics state")?
        }
        AnalyticsControlCommand::Enable(_) => {
            enable_basic_metrics().await.with_context(|| "Failed to enable metrics")?;
            show_metrics_status(writer).await.with_context(|| "Failed to read metrics state")?
        }
        AnalyticsControlCommand::Disable(_) => {
            disable_metrics().await.with_context(|| "Failed to disable metrics")?;
            show_metrics_status(writer).await.with_context(|| "Failed to read metrics state")?
        }
        AnalyticsControlCommand::Show(_) => {
            show_metrics_status(writer).await.with_context(|| "Failed to read metrics state")?
        }
    }
    Ok(())
}

async fn exec_check_ssh_keys<W: Write>(
    ctx: &EnvironmentContext,
    _check_ssh_command: &SshKeyCommand,
    mut writer: W,
) -> Result<()> {
    match SshKeyFiles::load(Some(&ctx)).await {
        Ok(ssh_files) => {
            match ssh_files.check_keys(true) {
                Ok(message) => writeln!(writer, "{message}")?,
                Err(e) => match e.kind {
                    SshKeyErrorKind::BadKeyType => {
                        writeln!(writer, "SSH keys type not supported: {}", e.message)?
                    }
                    SshKeyErrorKind::BadConfiguration => {
                        writeln!(writer, "SSH keys configuration problem: {e}")?
                    }
                    _ => writeln!(writer, "SSH keys problem: {e}.")?,
                },
            };
        }
        Err(e) => {
            writeln!(writer, "Could not get SSH key paths {e}")?;
        }
    };
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use std::{env, fs};

    use super::*;
    use errors::{FfxError, IntoExitCode};
    use ffx_config::{test_init, SelectMode};
    use serde_json::json;

    #[fuchsia::test]
    async fn test_exec_env_set_set_values() -> Result<()> {
        let test_env = test_init().await?;
        let writer = Vec::<u8>::new();
        let cmd = EnvSetCommand { file: "test.json".into(), level: ConfigLevel::User };
        exec_env_set(&test_env.context, writer, &cmd).await?;
        assert_eq!(cmd.file, test_env.load().get_user().unwrap());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_gey_key() {
        let test_env = test_init().await.expect("test env initialized");
        test_env
            .context
            .query("some-key")
            .level(Some(ConfigLevel::User))
            .set("a value".into())
            .await
            .expect("setting value");

        let get_cmd = GetCommand {
            name: Some("some-key".into()),
            process: MappingMode::Substitute,
            select: ffx_config::SelectMode::First,
        };

        let mut writer = Vec::<u8>::new();
        exec_get(&test_env.context, &get_cmd, &mut writer).await.expect("getting value");
        assert_eq!(String::from_utf8(writer).unwrap(), "\"a value\"\n".to_string());
    }

    #[fuchsia::test]
    async fn test_remove_key() {
        let test_env = test_init().await.expect("test env initialized");
        test_env
            .context
            .query("some-key")
            .level(Some(ConfigLevel::User))
            .set("a value".into())
            .await
            .expect("setting value");

        let remove_cmd = RemoveCommand { name: "some-key".into() };

        let get_cmd = GetCommand {
            name: Some("some-key".into()),
            process: MappingMode::Substitute,
            select: ffx_config::SelectMode::First,
        };

        exec_remove(&test_env.context, &remove_cmd).await.expect("remove");

        let mut writer = Vec::<u8>::new();
        match exec_get(&test_env.context, &get_cmd, &mut writer).await {
            Ok(_) => panic!("Expected error getting removed key"),
            Err(e) => assert_eq!(e.to_string(), "Value not found"),
        };
    }

    #[fuchsia::test]
    async fn test_remove_nonexistant_key() {
        let test_env = test_init().await.expect("test env initialized");

        let remove_cmd = RemoveCommand { name: "some-key".into() };

        match exec_remove(&test_env.context, &remove_cmd).await {
            Ok(_) => panic!("Expected error getting removed key"),
            Err(e) => {
                if let Some(ffx_err) = e.downcast_ref::<FfxError>() {
                    assert_eq!(ffx_err.to_string(), "Configuration key not found");
                    assert!(ffx_err.exit_code() != 0, "Expected non-zero exit code");
                } else {
                }
            }
        };
    }

    #[fuchsia::test]
    async fn test_list_processed_by_raw() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let private_path1 = test_env.isolate_root.path().join("privatekey1");
        let private_path2 = test_env.isolate_root.path().join("privatekey2");
        fs::write(&private_path1, "path1").expect("key 1 written");
        fs::write(&private_path2, "path2").expect("key 2 written");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET_2",
                private_path1.to_string_lossy(),
                private_path2.to_string_lossy(),
            ]))
            .await
            .expect("set ssh.priv");

        exec_get(
            &test_env.context,
            &GetCommand {
                name: Some("ssh.priv".into()),
                process: MappingMode::Raw,
                select: SelectMode::First,
            },
            &mut writer,
        )
        .await
        .expect("exec_get");
        let got = String::from_utf8_lossy(&writer);
        let want = serde_json::to_string_pretty(&json!([
            "$ENV_PATH_THAT_IS_NOT_SET_2",
            private_path1,
            private_path2
        ]))
        .expect("json output");
        assert_eq!(got, format!("{}\n", want));
    }

    #[fuchsia::test]
    async fn test_list_processed_by_substitute() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let private_path1 = test_env.isolate_root.path().join("privatekey1");
        let private_path2 = test_env.isolate_root.path().join("privatekey2");
        fs::write(&private_path1, "path1").expect("key 1 written");
        fs::write(&private_path2, "path2").expect("key 2 written");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET_2",
                private_path1.to_string_lossy(),
                private_path2.to_string_lossy(),
            ]))
            .await
            .expect("set ssh.priv");

        exec_get(
            &test_env.context,
            &GetCommand {
                name: Some("ssh.priv".into()),
                process: MappingMode::Substitute,
                select: SelectMode::First,
            },
            &mut writer,
        )
        .await
        .expect("exec_get");
        let got = String::from_utf8_lossy(&writer);
        let want = serde_json::to_string_pretty(&json!([private_path1, private_path2]))
            .expect("json output");
        assert_eq!(got, format!("{}\n", want));
    }

    #[fuchsia::test]
    async fn test_list_processed_by_substitute_with_env() {
        // Set the env before the test env
        // assert the key is not already in the environment
        assert!(
            env::var("ENV_SSH_PATH_FOR_TESTING_").is_err(),
            "Expected weird testing env variable to be unset"
        );
        env::set_var("ENV_SSH_PATH_FOR_TESTING_", "private_path1");

        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let private_path1 = test_env.isolate_root.path().join("privatekey1");
        let private_path2 = test_env.isolate_root.path().join("privatekey2");
        fs::write(&private_path1, "path1").expect("key 1 written");
        fs::write(&private_path2, "path2").expect("key 2 written");

        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!(["$ENV_SSH_PATH_FOR_TESTING_", private_path2.to_string_lossy(),]))
            .await
            .expect("set ssh.priv");

        exec_get(
            &test_env.context,
            &GetCommand {
                name: Some("ssh.priv".into()),
                process: MappingMode::Substitute,
                select: SelectMode::First,
            },
            &mut writer,
        )
        .await
        .expect("exec_get");
        let got = String::from_utf8_lossy(&writer);
        let want = serde_json::to_string_pretty(&json!(["private_path1", private_path2]))
            .expect("json output");
        assert_eq!(got, format!("{}\n", want));
    }

    #[fuchsia::test]
    async fn test_list_single_by_file() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let private_path1 = test_env.isolate_root.path().join("privatekey1");
        fs::write(&private_path1, "path1").expect("key 1 written");

        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([private_path1.to_string_lossy(),]))
            .await
            .expect("set ssh.priv");

        exec_get(
            &test_env.context,
            &GetCommand {
                name: Some("ssh.priv".into()),
                process: MappingMode::File,
                select: SelectMode::First,
            },
            &mut writer,
        )
        .await
        .expect("exec_get");
        let got = String::from_utf8_lossy(&writer);
        assert_eq!(got, format!("{}\n", json!(private_path1)));
    }

    #[fuchsia::test]
    async fn test_list_processed_by_file() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let private_path1 = test_env.isolate_root.path().join("privatekey1");
        let private_path2 = test_env.isolate_root.path().join("privatekey2");
        fs::write(&private_path1, "path1").expect("key 1 written");
        fs::write(&private_path2, "path2").expect("key 2 written");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET_2",
                private_path1.to_string_lossy(),
                private_path2.to_string_lossy(),
            ]))
            .await
            .expect("set ssh.priv");

        exec_get(
            &test_env.context,
            &GetCommand {
                name: Some("ssh.priv".into()),
                process: MappingMode::File,
                select: SelectMode::First,
            },
            &mut writer,
        )
        .await
        .expect("exec_get");
        let got = String::from_utf8_lossy(&writer);
        assert_eq!(got, format!("{}\n", json!(private_path1)));
    }

    #[fuchsia::test]
    async fn test_list_processed_by_file_with_env() {
        let private_path1 = tempfile::NamedTempFile::new().expect("temp file 1");
        // Set the env before the test env
        // assert the key is not already in the environment
        assert!(
            env::var("ENV_SSH_PATH_FOR_TESTING_2").is_err(),
            "Expected weird testing env variable to be unset"
        );
        env::set_var("ENV_SSH_PATH_FOR_TESTING_2", private_path1.path());

        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let private_path2 = test_env.isolate_root.path().join("privatekey2");
        fs::write(&private_path2, "path2").expect("key 2 written");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!(["$ENV_SSH_PATH_FOR_TESTING_2", private_path2.to_string_lossy(),]))
            .await
            .expect("set ssh.priv");

        exec_get(
            &test_env.context,
            &GetCommand {
                name: Some("ssh.priv".into()),
                process: MappingMode::File,
                select: SelectMode::First,
            },
            &mut writer,
        )
        .await
        .expect("exec_get");
        let got = String::from_utf8_lossy(&writer);
        assert_eq!(got, format!("{}\n", json!(private_path1.path())));
    }

    #[fuchsia::test]
    async fn test_exec_check_mismatched_ssh_keys() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let auth_key_path1 = test_env.isolate_root.path().join("authorized_keys1");
        let private_path1 = test_env.isolate_root.path().join("privatekey1");

        let auth_key_path2 = test_env.isolate_root.path().join("authorized_keys2");
        let private_path2 = test_env.isolate_root.path().join("privatekey2");

        test_env
            .context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET",
                auth_key_path2.to_string_lossy(),
                "someother"
            ]))
            .await
            .expect("set ssh.pub");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET_2",
                private_path1.to_string_lossy(),
                "someother/place"
            ]))
            .await
            .expect("set ssh.priv");

        let keys = SshKeyFiles {
            authorized_keys: auth_key_path1.clone(),
            private_key: private_path1.clone(),
        };
        keys.create_keys_if_needed(false).expect("Initializing keys");

        let other_keys = SshKeyFiles {
            authorized_keys: auth_key_path2.clone(),
            private_key: private_path2.clone(),
        };
        other_keys.create_keys_if_needed(false).expect("Initializing other keys");

        exec_check_ssh_keys(&test_env.context, &SshKeyCommand {}, &mut writer)
            .await
            .expect("no error");
        assert_eq!(format!("Keys repaired: KeyMismatch:Could not find matching public key for the private key {}.\n", private_path1.to_string_lossy()), String::from_utf8_lossy(&writer));

        let mut post_repair_writer = Vec::<u8>::new();

        exec_check_ssh_keys(&test_env.context, &SshKeyCommand {}, &mut post_repair_writer)
            .await
            .expect("no error");
        assert_eq!("SSH Public/Private keys match\n", String::from_utf8_lossy(&post_repair_writer));
    }

    #[fuchsia::test]
    async fn test_exec_check_ok_ssh_keys() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let auth_key_path = test_env.isolate_root.path().join("authorized_keys");
        let private_path = test_env.isolate_root.path().join("privatekey");

        test_env
            .context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!(["$ENV_PATH_THAT_IS_NOT_SET", auth_key_path.to_string_lossy(), "someother"]))
            .await
            .expect("set ssh.pub");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET_2",
                private_path.to_string_lossy(),
                "someother/place"
            ]))
            .await
            .expect("set ssh.priv");

        let keys = SshKeyFiles::load(Some(&test_env.context)).await.expect("new ssh keys");

        keys.create_keys_if_needed(false).expect("Initializing keys");

        assert_eq!(
            keys.authorized_keys.display().to_string(),
            auth_key_path.to_string_lossy().to_string()
        );
        assert_eq!(
            keys.private_key.display().to_string(),
            private_path.to_string_lossy().to_string()
        );

        exec_check_ssh_keys(&test_env.context, &SshKeyCommand {}, &mut writer)
            .await
            .expect("no error");
        assert_eq!("SSH Public/Private keys match\n", String::from_utf8_lossy(&writer));
    }

    #[fuchsia::test]
    async fn test_exec_check_empty_ssh_keys() {
        let test_env = test_init().await.expect("test env");
        let mut writer = Vec::<u8>::new();

        let auth_key_path = test_env.isolate_root.path().join("authorized_keys");
        let private_path = test_env.isolate_root.path().join("privatekey");

        test_env
            .context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!(["$ENV_PATH_THAT_IS_NOT_SET", auth_key_path.to_string_lossy(), "someother"]))
            .await
            .expect("set ssh.pub");
        test_env
            .context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([
                "$ENV_PATH_THAT_IS_NOT_SET_2",
                private_path.to_string_lossy(),
                "someother/place"
            ]))
            .await
            .expect("set ssh.priv");

        let keys = SshKeyFiles::load(Some(&test_env.context)).await.expect("new ssh keys");

        assert_eq!(
            keys.authorized_keys.display().to_string(),
            auth_key_path.to_string_lossy().to_string()
        );
        assert_eq!(
            keys.private_key.display().to_string(),
            private_path.to_string_lossy().to_string()
        );

        exec_check_ssh_keys(&test_env.context, &SshKeyCommand {}, &mut writer)
            .await
            .expect("no error");
        assert_eq!(
            format!(
                "Keys repaired: FileNotFound:Private key {} does not exist.\n",
                private_path.to_string_lossy()
            ),
            String::from_utf8_lossy(&writer)
        );
    }
}
