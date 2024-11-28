// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_target::TargetProxy;
use ffx_target_repository_register_args::RegisterCommand;
use fho::{
    bug, daemon_protocol, moniker, return_bug, return_user_error, user_error, Error, FfxContext,
    FfxMain, FfxTool, Result, VerifiedMachineWriter,
};
use fidl_fuchsia_developer_ffx::{RepositoryRegistryProxy, RepositoryTarget, TargetInfo};
use fidl_fuchsia_developer_ffx_ext::{RepositoryError, RepositoryTarget as FfxRepositoryTarget};
use fidl_fuchsia_pkg::RepositoryManagerProxy;
use fidl_fuchsia_pkg_rewrite::EngineProxy;
use pkg::config::get_repository;
use pkg::repo::register_target_with_repo_instance;
use pkg::{PkgServerInfo, PkgServerInstanceInfo as _, PkgServerInstances, ServerMode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use timeout::timeout;

const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";

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
    target_proxy: TargetProxy,
    #[with(moniker(REPOSITORY_MANAGER_MONIKER))]
    repo_proxy: RepositoryManagerProxy,
    #[with(moniker(REPOSITORY_MANAGER_MONIKER))]
    engine_proxy: EngineProxy,
}

fho::embedded_plugin!(RegisterTool);

#[async_trait(?Send)]
impl FfxMain for RegisterTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match self.register_cmd().await {
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

impl RegisterTool {
    pub async fn register_cmd(&self) -> Result<()> {
        // Get the repository that should be registered.
        let instance_root = self
            .context
            .get("repository.process_dir")
            .map_err(|e: ffx_config::api::ConfigError| bug!(e))?;
        let mgr = PkgServerInstances::new(instance_root);

        let mut repo_name = if let Some(name) = &self.cmd.repository {
            Some(name.to_string())
        } else {
            pkg::config::get_default_repository().await?
        }
        .ok_or_else(|| {
            user_error!(
                "A repository must be specfied via the --repository flag or \
            configured using 'ffx repository default set'"
            )
        })?;

        // if none was found, check for a product bundle repo server which has the prefix of repo_name.
        let pkg_server_info = match mgr.get_instance(repo_name.clone())? {
            Some(instance) => Some(instance),
            None => {
                let instances = mgr.list_instances()?;
                instances
                    .iter()
                    .find(|s| s.name.starts_with(&format!("{repo_name}.")))
                    .and_then(|s| Some(s.clone()))
            }
        };

        let target_spec = ffx_target::get_target_specifier(&self.context)
            .await
            .user_message("getting target specifier from config")?;

        // update the repo name if we matched a product bundle repo.
        if let Some(info) = pkg_server_info.as_ref() {
            repo_name = info.name.clone();
        }

        let repository_target = RepositoryTarget {
            repo_name: Some(repo_name.clone()),
            target_identifier: target_spec.clone(),
            aliases: Some(self.cmd.alias.clone()),
            storage_type: self.cmd.storage_type,
            ..Default::default()
        };

        if let Some(server_info) = pkg_server_info {
            match server_info.server_mode {
                ServerMode::Daemon => self.register_daemon(&repo_name, repository_target).await,
                _ => self.register_standalone(&server_info, repository_target).await,
            }
        } else {
            // This means no running server matches the repo_name. Check if it is a daemon repo that is not running.
            // If so, treat it as a daemon based repo
            // with a warning. Once we migrate away from the daemon, it should go back to being an error.

            if get_repository(&repo_name).await?.is_some() {
                tracing::warn!("Repository server \"{repo_name}\" not running. Treating this as a daemon based server.");
                eprintln!("Repository server \"{repo_name}\" not running. Treating this as a daemon based server.");
                eprintln!("If \"{repo_name}\" is supposed to be a standalone server, please start it before running this command.");

                self.register_daemon(&repo_name, repository_target).await
            } else {
                return_user_error!(
                    "{repo_name} is not a running repository, nor a daemon based repository."
                )
            }
        }
    }

    async fn register_standalone(
        &self,
        info: &PkgServerInfo,
        mut repo_target_info: RepositoryTarget,
    ) -> Result<()> {
        repo_target_info.aliases = match repo_target_info.aliases {
            Some(aliases) if aliases.is_empty() => {
                Some(info.aliases().iter().map(ToString::to_string).collect())
            }
            None => Some(info.aliases().iter().map(ToString::to_string).collect()),
            Some(aliases) => Some(aliases),
        };

        let ffx_repo_target_info =
            FfxRepositoryTarget::try_from(repo_target_info).map_err(|e| bug!(e))?;

        let target_info: TargetInfo = timeout(Duration::from_secs(2), self.target_proxy.identity())
            .await
            .bug_context("Timed out getting target identity")?
            .bug_context("Failed to get target identity")?;

        let repo_server_listen_addr = info.address;

        register_target_with_repo_instance(
            self.repo_proxy.clone(),
            self.engine_proxy.clone(),
            &ffx_repo_target_info,
            &target_info,
            repo_server_listen_addr,
            &info,
            self.cmd.alias_conflict_mode.into(),
        )
        .await
        .map_err(|e| bug!("Failed to register repository: {:?}", e))
    }

    async fn register_daemon(
        &self,
        repo_name: &str,
        repo_target_info: RepositoryTarget,
    ) -> Result<()> {
        let target_spec = &repo_target_info.target_identifier;

        match self
            .repos
            .register_target(&repo_target_info, self.cmd.alias_conflict_mode)
            .await
            .bug_context("communicating with daemon")?
            .map_err(RepositoryError::from)
        {
            Ok(()) => Ok(()),
            Err(err @ RepositoryError::TargetCommunicationFailure) => {
                return_user_error!(
                    "Error while registering repository {repo_name} with {target}: {err}\n\
                                    Ensure that a target is running and connected with:\n\
                                    $ ffx target list",
                    target = target_spec.clone().unwrap_or_else(|| "None".to_string())
                )
            }
            Err(RepositoryError::ServerNotRunning) => {
                return_bug!(
                    "Failed to register repository {repo_name} with {target} {reason:#}",
                    reason = pkg::config::determine_why_repository_server_is_not_running().await,
                    target = target_spec.clone().unwrap_or_else(|| "None".to_string())
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
                return_bug!("Unexpected error. Failed to register repository {repo_name} with {target}: {err}",
                target=target_spec.clone().unwrap_or_else(|| "None".to_string()))
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use camino::Utf8PathBuf;
    use ffx_config::keys::TARGET_DEFAULT_KEY;
    use ffx_config::ConfigLevel;
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::{
        RepositoryError, RepositoryRegistryRequest, RepositoryStorageType, SshHostAddrInfo,
        TargetRequest,
    };
    use fidl_fuchsia_pkg::{MirrorConfig, RepositoryConfig, RepositoryManagerRequest};
    use fidl_fuchsia_pkg_ext::{RepositoryConfigBuilder, RepositoryRegistrationAliasConflictMode};
    use fidl_fuchsia_pkg_rewrite::{
        EditTransactionRequest, EngineRequest, LiteralRule, Rule, RuleIteratorRequest,
    };
    use fuchsia_repo::repository::RepositorySpec;
    use fuchsia_url::RepositoryUrl;
    use futures::channel::oneshot::{channel, Receiver};
    use futures::TryStreamExt;
    use std::collections::BTreeSet;
    use std::net::IpAddr;

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
                let mut target_info = target_info.clone();
                target_info.target_identifier = Some(TARGET_NAME.into());
                sender.take().unwrap().send(target_info).unwrap();
                responder.send(Ok(())).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    async fn setup_fake_repo_proxy(
        expected_config: Option<RepositoryConfig>,
    ) -> (RepositoryManagerProxy, Receiver<Result<(), i32>>) {
        let (sender, receiver) = channel();
        let mut _sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryManagerRequest::Add { repo, responder } => {
                if let Some(expected) = &expected_config {
                    if expected.repo_url != repo.repo_url {
                        tracing::error!("expected {:?} got {:?}", expected.repo_url, repo.repo_url);
                        responder.send(Err(-100)).unwrap();
                        return;
                    } else if expected.root_keys != repo.root_keys {
                        tracing::error!(
                            "expected {:?} got {:?}",
                            expected.root_keys,
                            repo.root_keys
                        );
                        responder.send(Err(-101)).unwrap();
                        return;
                    } else if expected.mirrors != repo.mirrors {
                        tracing::error!("expected {:?} got {:?}", expected.mirrors, repo.mirrors);
                        responder.send(Err(-102)).unwrap();
                        return;
                    } else if expected.root_version != repo.root_version {
                        tracing::error!(
                            "expected {:?} got {:?}",
                            expected.root_version,
                            repo.root_version
                        );
                        responder.send(Err(-103)).unwrap();
                        return;
                    } else if expected.root_threshold != repo.root_threshold {
                        tracing::error!(
                            "expected {:?} got {:?}",
                            expected.root_threshold,
                            repo.root_threshold
                        );
                        responder.send(Err(-104)).unwrap();
                        return;
                    } else if expected.use_local_mirror != repo.use_local_mirror {
                        tracing::error!(
                            "expected {:?} got {:?}",
                            expected.use_local_mirror,
                            repo.use_local_mirror
                        );
                        responder.send(Err(-105)).unwrap();
                        return;
                    } else if expected.storage_type != repo.storage_type {
                        tracing::error!(
                            "expected {:?} got {:?}",
                            expected.storage_type,
                            repo.storage_type
                        );
                        responder.send(Err(-106)).unwrap();
                        return;
                    }
                }

                responder.send(Ok(())).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    async fn setup_fake_engine_proxy(
        expected_rule: Option<Rule>,
    ) -> (EngineProxy, Receiver<Result<(), i32>>) {
        let (sender, receiver) = channel();
        let mut _sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            EngineRequest::StartEditTransaction { transaction, control_handle: _ } => {
                let expected_rule = expected_rule.clone();
                fuchsia_async::Task::local(async move {
                    let mut tx_stream = transaction.into_stream();

                    while let Some(req) = tx_stream.try_next().await.unwrap() {
                        match req {
                            EditTransactionRequest::ResetAll { control_handle: _ } => (),
                            EditTransactionRequest::ListDynamic { iterator, control_handle: _ } => {
                                let mut stream = iterator.into_stream();

                                while let Some(req) = stream.try_next().await.unwrap() {
                                    let RuleIteratorRequest::Next { responder } = req;
                                    responder.send(&[]).unwrap();
                                }
                            }
                            EditTransactionRequest::Add { rule, responder } => {
                                if let Some(Rule::Literal(ref expected)) = expected_rule {
                                    if let Rule::Literal(actual) = rule {
                                        if expected.host_match != actual.host_match {
                                            tracing::error!(
                                                "host_match expected {:?} got {:?}",
                                                expected.host_match,
                                                actual.host_match
                                            );
                                            responder.send(Err(-100)).unwrap();
                                            return;
                                        }
                                        if expected.host_replacement != actual.host_replacement {
                                            tracing::error!(
                                                "host_replacement expected {:?} got {:?}",
                                                expected.host_replacement,
                                                actual.host_replacement
                                            );
                                            responder.send(Err(-101)).unwrap();
                                            return;
                                        }
                                        if expected.path_prefix_match != actual.path_prefix_match {
                                            tracing::error!(
                                                "path_prefix_match expected {:?} got {:?}",
                                                expected.path_prefix_match,
                                                actual.path_prefix_match
                                            );
                                            responder.send(Err(-102)).unwrap();
                                            return;
                                        }
                                        if expected.path_prefix_replacement
                                            != actual.path_prefix_replacement
                                        {
                                            tracing::error!(
                                                "path_prefix_replacement expected {:?} got {:?}",
                                                expected.path_prefix_replacement,
                                                actual.path_prefix_replacement
                                            );
                                            responder.send(Err(-103)).unwrap();
                                            return;
                                        }
                                    }
                                }
                                responder.send(Ok(())).unwrap();
                            }
                            EditTransactionRequest::Commit { responder } => {
                                responder.send(Ok(())).unwrap();
                            }
                        }
                    }
                })
                .detach()
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    async fn setup_fake_target_proxy() -> (TargetProxy, Receiver<Result<(), i32>>) {
        let (sender, receiver) = channel();
        let mut _sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            TargetRequest::Identity { responder } => {
                let addr: TargetAddr = TargetAddr::new(
                    IpAddr::from([0xfe80, 0x0, 0x0, 0x0, 0xdead, 0xbeef, 0xbeef, 0xbeef]),
                    3,
                    0,
                );

                responder
                    .send(&TargetInfo {
                        nodename: Some("target-nodename".into()),
                        addresses: Some(vec![addr.into()]),
                        ssh_host_address: Some(SshHostAddrInfo { address: "127.7.7.1:22".into() }),
                        age_ms: Some(101),
                        ..Default::default()
                    })
                    .unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    async fn make_server_instance(
        root: &std::path::Path,
        context: &EnvironmentContext,
        server_mode: ServerMode,
        name: &str,
        aliases: BTreeSet<String>,
    ) -> Result<()> {
        let instance_root = root.join("repo_servers");
        context
            .query("repository.process_dir")
            .level(Some(ConfigLevel::User))
            .set(instance_root.to_string_lossy().into())
            .await?;

        let mgr = PkgServerInstances::new(instance_root);
        let repo_config = RepositoryConfigBuilder::new(
            RepositoryUrl::parse_host("name".into()).expect("repo url"),
        )
        .into();

        mgr.write_instance(&PkgServerInfo {
            name: name.into(),
            address: ([127, 0, 0, 1], 8888).into(),
            repo_spec: RepositorySpec::Pm { path: Utf8PathBuf::from("/some/repo/path"), aliases },
            registration_storage_type: fidl_fuchsia_pkg_ext::RepositoryStorageType::Ephemeral,
            registration_alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut
                .into(),
            server_mode,
            pid: std::process::id(),
            repo_config,
        })
        .map_err(Into::into)
    }

    #[fuchsia::test]
    async fn test_register_daemon() {
        let env = ffx_config::test_init().await.expect("test env");

        let (repos, receiver) = setup_fake_server().await;
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let aliases = vec![String::from("my-alias")];

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

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
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
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
    async fn test_register_standalone() {
        let env = ffx_config::test_init().await.expect("test env");

        let (repos, _) = setup_fake_server().await;
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let aliases = vec![String::from("my-alias")];

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Foreground,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

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
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("register ok");
    }

    #[fuchsia::test]
    async fn test_register_standalone_product_bundle() {
        let env = ffx_config::test_init().await.expect("test env");

        let expected_config = RepositoryConfig {
            repo_url: Some("fuchsia-pkg://test-repo.fuchsia.com".into()),
            root_keys: Some(vec![]),
            mirrors: Some(vec![MirrorConfig {
                mirror_url: Some("http://127.0.0.1:8888/test-repo.fuchsia.com".into()),
                subscribe: Some(false),
                blob_mirror_url: None,
                ..Default::default()
            }]),
            root_version: Some(1),
            root_threshold: Some(1),
            use_local_mirror: Some(false),
            storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
            ..Default::default()
        };

        let expected_rule = Rule::Literal(LiteralRule {
            host_match: "fuchsia.com".into(),
            host_replacement: "test-repo.fuchsia.com".into(),
            path_prefix_match: "/".into(),
            path_prefix_replacement: "/".into(),
        });

        let (repos, _) = setup_fake_server().await;
        let (repo_proxy, _) = setup_fake_repo_proxy(Some(expected_config)).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(Some(expected_rule)).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let mut aliases = BTreeSet::new();
        aliases.insert("fuchsia.com".into());
        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Foreground,
            "test-repo.fuchsia.com",
            aliases,
        )
        .await
        .expect("repo server instance");

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        env.context
            .query("repository.default")
            .level(Some(ConfigLevel::User))
            .set("test-repo".into())
            .await
            .expect("set default repo name");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: None,
                alias: vec![],
                storage_type: None,
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        let res = tool.main(writer).await;
        match res {
            Ok(_) => (),
            Err(e) => assert!(false, "Unexpected error {e:?}"),
        }
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

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            default_repo_name,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

        let (repos, receiver) = setup_fake_server().await;
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: None,
                alias: vec![],
                storage_type: None,
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("register ok");

        let got = receiver.await.unwrap();
        assert_eq!(
            got,
            RepositoryTarget {
                repo_name: Some(default_repo_name.to_string()),
                target_identifier: Some(TARGET_NAME.into()),
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
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let aliases = vec![String::from("my-alias")];

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: aliases.clone(),
                storage_type: Some(RepositoryStorageType::Persistent),
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
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
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NAME.into())
            .await
            .expect("set default target");

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
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
        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

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
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos,
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(None, &buffers);

        let err = tool.main(writer).await.expect_err("register error");
        let want = "Error while registering repository some-name with some-target: \
        error communicating with target device\n\
        Ensure that a target is running and connected with:\n$ ffx target list";
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

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

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
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        let tool = RegisterTool {
            cmd: RegisterCommand {
                repository: Some(REPO_NAME.to_string()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos,
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;
        let want = "Error while registering repository some-name with some-target: \
        error communicating with target device\nEnsure that a target is running and connected with:\n\
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
        let (repo_proxy, _) = setup_fake_repo_proxy(None).await;
        let (engine_proxy, _) = setup_fake_engine_proxy(None).await;
        let (target_proxy, _) = setup_fake_target_proxy().await;

        make_server_instance(
            env.isolate_root.path(),
            &env.context,
            ServerMode::Daemon,
            REPO_NAME,
            BTreeSet::<String>::new(),
        )
        .await
        .expect("repo server instance");

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
                alias_conflict_mode:
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            },
            repos: repos.clone(),
            context: env.context.clone(),
            repo_proxy,
            engine_proxy,
            target_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = <RegisterTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");

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

        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <RegisterTool as FfxMain>::Writer::verify_schema(&json).expect(&err);

        assert_eq!(json, serde_json::json!({"ok":{}}));
    }
}
