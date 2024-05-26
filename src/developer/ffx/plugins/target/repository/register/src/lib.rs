// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_target_repository_register_args::RegisterCommand;
use fho::{
    daemon_protocol, return_bug, return_user_error, Error, FfxContext, FfxMain, FfxTool, Result,
    VerifiedMachineWriter,
};
use fidl_fuchsia_developer_ffx::{RepositoryRegistryProxy, RepositoryTarget};
use fidl_fuchsia_developer_ffx_ext::RepositoryError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successfully waited for the target (either to come up or shut down).
    Ok {},
    /// Unexpected error with string denoting error message.
    UnexpectedError { message: String },
    /// A known error that can be reported to the user.
    UserError { message: String },
}

#[derive(FfxTool)]
pub struct RegisterTool {
    #[command]
    cmd: RegisterCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
    context: EnvironmentContext,
}

fho::embedded_plugin!(RegisterTool);

#[async_trait(?Send)]
impl FfxMain for RegisterTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match register_cmd(self.cmd, self.repos, self.context).await {
            Ok(()) => {
                writer.machine(&CommandStatus::Ok {})?;
                Ok(())
            }
            Err(e @ Error::User(_)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(e)
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                Err(e)
            }
        }
    }
}

pub async fn register_cmd(
    cmd: RegisterCommand,
    repos: RepositoryRegistryProxy,
    context: EnvironmentContext,
) -> Result<()> {
    register(
        ffx_target::get_target_specifier(&context)
            .await
            .user_message("getting target specifier from config")?,
        cmd,
        repos,
    )
    .await
}

async fn register(
    target_str: Option<String>,
    cmd: RegisterCommand,
    repos: RepositoryRegistryProxy,
) -> Result<()> {
    let repo_name = if let Some(repo_name) = cmd.repository {
        repo_name
    } else {
        if let Some(repo_name) = pkg::config::get_default_repository().await? {
            repo_name
        } else {
            return_user_error!(
                "Either a default repository must be set, or the --repository flag must be provided.\n\
                You can set a default repository using:\n\
                $ ffx repository default set <name>"
            )
        }
    };

    match repos
        .register_target(
            &RepositoryTarget {
                repo_name: Some(repo_name.clone()),
                target_identifier: target_str,
                aliases: Some(cmd.alias),
                storage_type: cmd.storage_type,
                ..Default::default()
            },
            cmd.alias_conflict_mode,
        )
        .await
        .bug_context("communicating with daemon")?
        .map_err(RepositoryError::from)
    {
        Ok(()) => Ok(()),
        Err(err @ RepositoryError::TargetCommunicationFailure) => {
            return_user_error!(
                "Error while registering repository: {err}\n\
                Ensure that a target is running and connected with:\n\
                $ ffx target list",
            )
        }
        Err(RepositoryError::ServerNotRunning) => {
            return_bug!(
                "Failed to register repository: {:#}",
                pkg::config::determine_why_repository_server_is_not_running().await
            )
        }
        Err(err @ RepositoryError::ConflictingRegistration) => {
            return_user_error!(
                "Error while registering repository: {err:#}\n\
                Repository '{repo_name}' has an alias conflict in its registration.\n\
                Locate and run de-registeration command specified in the Daemon log:\n\
                \n\
                $ ffx daemon log | grep \"Alias conflict found while registering '{repo_name}'\""
            )
        }
        Err(err) => {
            return_bug!("Failed to register repository: {err}")
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::{keys::TARGET_DEFAULT_KEY, ConfigLevel};
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::{
        RepositoryError, RepositoryRegistryRequest, RepositoryStorageType,
    };
    use fidl_fuchsia_developer_ffx_ext::RepositoryRegistrationAliasConflictMode;
    use futures::channel::oneshot::{channel, Receiver};

    const REPO_NAME: &str = "some-name";
    const TARGET_NAME: &str = "some-target";

    async fn setup_fake_server() -> (RepositoryRegistryProxy, Receiver<RepositoryTarget>) {
        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RegisterTarget {
                target_info,
                responder,
                alias_conflict_mode: _,
            } => {
                sender.take().unwrap().send(target_info).unwrap();
                responder.send(Ok(())).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    #[fuchsia::test]
    async fn test_register() {
        let env = ffx_config::test_init().await.expect("test env");

        let (repos, receiver) = setup_fake_server().await;

        let aliases = vec![String::from("my-alias")];

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: aliases.clone(),
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos: repos.clone(),
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("register ok");

        let got = receiver.await.unwrap();
        assert_eq!(
            got,
            RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NAME.to_string()),
                aliases: Some(aliases),
                storage_type: None,
                ..Default::default()
            }
        );
    }

    #[fuchsia::test]
    async fn test_register_default_repository() {
        let env = ffx_config::test_init().await.unwrap();

        let default_repo_name = "default-repo";
        env.context
            .query("repository.default")
            .level(Some(ConfigLevel::User))
            .set(default_repo_name.into())
            .await
            .unwrap();

        let (repos, receiver) = setup_fake_server().await;

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: None,
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos: repos.clone(),
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("register ok");

        let got = receiver.await.unwrap();
        assert_eq!(
            got,
            RepositoryTarget {
                repo_name: Some(default_repo_name.to_string()),
                target_identifier: None,
                aliases: Some(vec![]),
                storage_type: None,
                ..Default::default()
            }
        );
    }

    #[fuchsia::test]
    async fn test_register_storage_type() {
        let env = ffx_config::test_init().await.expect("test env");

        let (repos, receiver) = setup_fake_server().await;

        let aliases = vec![String::from("my-alias")];

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: aliases.clone(),
                storage_type: Some(RepositoryStorageType::Persistent),
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos: repos.clone(),
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("register ok");

        let got = receiver.await.unwrap();
        assert_eq!(
            got,
            RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NAME.to_string()),
                aliases: Some(aliases),
                storage_type: Some(RepositoryStorageType::Persistent),
                ..Default::default()
            }
        );
    }

    #[fuchsia::test]
    async fn test_register_empty_aliases() {
        let env = ffx_config::test_init().await.expect("test env");

        let (repos, receiver) = setup_fake_server().await;

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos: repos.clone(),
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("register ok");

        let got = receiver.await.unwrap();
        assert_eq!(
            got,
            RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NAME.to_string()),
                aliases: Some(vec![]),
                storage_type: None,
                ..Default::default()
            }
        );
    }

    #[fuchsia::test]
    async fn test_register_returns_error() {
        let env = ffx_config::test_init().await.expect("test env");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RegisterTarget {
                target_info: _,
                responder,
                alias_conflict_mode: _,
            } => {
                responder.send(Err(RepositoryError::TargetCommunicationFailure)).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        let err = tool.main(writer).await.expect_err("register error");
        let want = "Error while registering repository: error communicating with target device\n\
        Ensure that a target is running and connected with:\n\
        $ ffx target list";
        assert_eq!(err.to_string(), want)
    }

    #[fuchsia::test]
    async fn test_register_returns_error_machine() {
        let env = ffx_config::test_init().await.expect("test env");
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RegisterTarget {
                target_info: _,
                responder,
                alias_conflict_mode: _,
            } => {
                responder.send(Err(RepositoryError::TargetCommunicationFailure)).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;
        let want = "Error while registering repository: error communicating with target device\n\
        Ensure that a target is running and connected with:\n\
        $ ffx target list";

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_err(), "expected error: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <RegisterTool as FfxMain>::Writer::verify_schema(&json).expect(&err);

        assert_eq!(json, serde_json::json!({"user_error":{"message": want}}));
    }

    #[fuchsia::test]
    async fn test_register_machine() {
        let env = ffx_config::test_init().await.expect("test env");

        let (repos, receiver) = setup_fake_server().await;

        let aliases = vec![String::from("my-alias")];

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: aliases.clone(),
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos: repos.clone(),
            context: env.context.clone(),
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;

        let got = receiver.await.unwrap();
        assert_eq!(
            got,
            RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NAME.to_string()),
                aliases: Some(aliases),
                storage_type: None,
                ..Default::default()
            }
        );

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <RegisterTool as FfxMain>::Writer::verify_schema(&json).expect(&err);

        assert_eq!(json, serde_json::json!({"ok":{}}));
    }
}
