// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_target_repository_register_args::RegisterCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{RepositoryRegistryProxy, RepositoryTarget};
use fidl_fuchsia_developer_ffx_ext::RepositoryError;

#[derive(FfxTool)]
pub struct RegisterTool {
    #[command]
    cmd: RegisterCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
}

fho::embedded_plugin!(RegisterTool);

#[async_trait(?Send)]
impl FfxMain for RegisterTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        register_cmd(self.cmd, self.repos).await?;
        Ok(())
    }
}

pub async fn register_cmd(cmd: RegisterCommand, repos: RepositoryRegistryProxy) -> Result<()> {
    register(
        ffx_config::get(TARGET_DEFAULT_KEY).await.context("getting default target from config")?,
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
            ffx_bail!(
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
        .context("communicating with daemon")?
        .map_err(RepositoryError::from)
    {
        Ok(()) => Ok(()),
        Err(err @ RepositoryError::TargetCommunicationFailure) => {
            ffx_bail!(
                "Error while registering repository: {}\n\
                Ensure that a target is running and connected with:\n\
                $ ffx target list",
                err,
            )
        }
        Err(RepositoryError::ServerNotRunning) => {
            ffx_bail!(
                "Failed to register repository: {:#}",
                pkg::config::determine_why_repository_server_is_not_running().await
            )
        }
        Err(err @ RepositoryError::ConflictingRegistration) => {
            ffx_bail!(
                "Error while registering repository: {:#}\n\
                Repository '{repo_name}' has an alias conflict in its registration.\n\
                Locate and run de-registeration command specified in the Daemon log:\n\
                \n\
                $ ffx daemon log | grep \"Alias conflict found while registering '{repo_name}'\"",
                err,
            )
        }
        Err(err) => {
            ffx_bail!("Failed to register repository: {}", err)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::ConfigLevel;
    use fidl_fuchsia_developer_ffx::{
        RepositoryError, RepositoryRegistryRequest, RepositoryStorageType,
    };
    use fidl_fuchsia_developer_ffx_ext::RepositoryRegistrationAliasConflictMode;
    use fuchsia_async as fasync;
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

    #[fasync::run_singlethreaded(test)]
    async fn test_register() {
        let (repos, receiver) = setup_fake_server().await;

        let aliases = vec![String::from("my-alias")];
        register(
            Some(TARGET_NAME.to_string()),
            RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: aliases.clone(),
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
        )
        .await
        .unwrap();
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

    #[fasync::run_singlethreaded(test)]
    async fn test_register_default_repository() {
        let _env = ffx_config::test_init().await.unwrap();

        let default_repo_name = "default-repo";
        ffx_config::query("repository.default")
            .level(Some(ConfigLevel::User))
            .set(default_repo_name.into())
            .await
            .unwrap();

        let (repos, receiver) = setup_fake_server().await;

        register(
            None,
            RegisterCommand {
                repository: None,
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
        )
        .await
        .unwrap();
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

    #[fasync::run_singlethreaded(test)]
    async fn test_register_storage_type() {
        let (repos, receiver) = setup_fake_server().await;

        let aliases = vec![String::from("my-alias")];
        register(
            Some(TARGET_NAME.to_string()),
            RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: aliases.clone(),
                storage_type: Some(RepositoryStorageType::Persistent),
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
        )
        .await
        .unwrap();
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

    #[fasync::run_singlethreaded(test)]
    async fn test_register_empty_aliases() {
        let (repos, receiver) = setup_fake_server().await;

        register(
            Some(TARGET_NAME.to_string()),
            RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
        )
        .await
        .unwrap();
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

    #[fasync::run_singlethreaded(test)]
    async fn test_register_returns_error() {
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

        assert!(register(
            Some(TARGET_NAME.to_string()),
            RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace.into(),
            },
            repos,
        )
        .await
        .is_err());
    }
}
