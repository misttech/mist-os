// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_target_repository_deregister_args::DeregisterCommand;
use fho::{
    bug, return_bug, return_user_error, user_error, FfxContext, FfxMain, FfxTool, Result,
    SimpleWriter,
};
use fidl_fuchsia_developer_ffx::{
    RepositoryConfig, RepositoryIteratorMarker, RepositoryRegistryProxy,
};
use fidl_fuchsia_developer_ffx_ext::RepositoryError;
use fidl_fuchsia_pkg::RepositoryManagerProxy;
use fidl_fuchsia_pkg_rewrite::EngineProxy;
use fidl_fuchsia_pkg_rewrite_ext::{do_transaction, Rule};
use pkg::{PkgServerInstanceInfo as _, PkgServerInstances, ServerMode};
use target_holders::{daemon_protocol, moniker};
use zx_status::Status;

const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
#[derive(FfxTool)]
pub struct DeregisterTool {
    #[command]
    cmd: DeregisterCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
    context: EnvironmentContext,
    #[with(moniker(REPOSITORY_MANAGER_MONIKER))]
    repo_proxy: RepositoryManagerProxy,
    #[with(moniker(REPOSITORY_MANAGER_MONIKER))]
    engine_proxy: EngineProxy,
}

fho::embedded_plugin!(DeregisterTool);

#[async_trait(?Send)]
impl FfxMain for DeregisterTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        // Get the repository that should be registered.
        let instance_root = self
            .context
            .get("repository.process_dir")
            .map_err(|e: ffx_config::api::ConfigError| bug!(e))?;
        let mgr = PkgServerInstances::new(instance_root);

        let repo_name = if let Some(name) = &self.cmd.repository {
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
        let repo_port = self.cmd.port;

        let pkg_server_info = mgr.get_instance(repo_name.clone(), repo_port)?;

        let target_spec = ffx_target::get_target_specifier(&self.context)
            .await
            .user_message("getting target specifier from config")?;

        if let Some(server_info) = pkg_server_info {
            if server_info.server_mode == ServerMode::Daemon {
                deregister_daemon(
                    &server_info.name,
                    target_spec,
                    self.repos.clone(),
                    self.engine_proxy,
                )
                .await?
            } else {
                deregister_standalone(&server_info.name, self.repo_proxy, self.engine_proxy).await?
            }
        } else {
            let deamon_repos = list_repositories(self.repos.clone()).await?;
            if deamon_repos.iter().any(|repo| repo.name == repo_name) {
                deregister_daemon(&repo_name, target_spec, self.repos.clone(), self.engine_proxy)
                    .await?
            } else {
                deregister_standalone(&repo_name, self.repo_proxy, self.engine_proxy).await?
            }
        }
        Ok(())
    }
}

async fn list_repositories(repos_proxy: RepositoryRegistryProxy) -> Result<Vec<RepositoryConfig>> {
    let (client, server) = fidl::endpoints::create_endpoints::<RepositoryIteratorMarker>();
    repos_proxy.list_repositories(server).map_err(|e| bug!("error listing repositories: {e}"))?;
    let client = client.into_proxy();

    let mut repos = vec![];
    loop {
        let batch = client
            .next()
            .await
            .map_err(|e| bug!("error fetching next batch of repositories: {e}"))?;
        if batch.is_empty() {
            break;
        }

        for repo in batch {
            repos
                .push(repo.try_into().map_err(|e| bug!("error converting repository config {e}"))?);
        }
    }
    Ok(repos)
}

async fn deregister_standalone(
    repo_name: &str,
    repo_proxy: RepositoryManagerProxy,
    rewrite_proxy: EngineProxy,
) -> Result<()> {
    let repo_url = format!("fuchsia-pkg://{repo_name}");
    match repo_proxy.remove(&repo_url).await {
        Ok(Ok(())) => (),
        Ok(Err(err)) => {
            let status = Status::from_raw(err);
            if status != Status::NOT_FOUND {
                let message = format!(
                    "failed to remove registration for {repo_url}: {:#?}",
                    Status::from_raw(err)
                );
                tracing::error!("{message}");
                return_bug!("{message}");
            } else {
                tracing::info!("registration for {repo_url} was not found. Ignoring.")
            }
        }
        Err(err) => {
            let message =
                format!("failed to remove registrtation  due to communication error: {:#?}", err);
            tracing::error!("{message}");
            return_bug!("{message}");
        }
    };
    // Remove any alias rules.
    remove_aliases(repo_name, rewrite_proxy).await
}

async fn deregister_daemon(
    repo_name: &str,
    target_str: Option<String>,
    repos: RepositoryRegistryProxy,
    rewrite_proxy: EngineProxy,
) -> Result<()> {
    match repos
        .deregister_target(repo_name, target_str.as_deref())
        .await
        .map_err(|e| bug!("{e}"))?
        .map_err(RepositoryError::from)
    {
        Ok(()) => (),
        Err(err @ RepositoryError::TargetCommunicationFailure) => {
            ffx_bail!(
                "Error while deregistering repository: {}\n\
                Ensure that a target is running and connected with:\n\
                $ ffx target list",
                err,
            )
        }
        Err(RepositoryError::ServerNotRunning) => {
            return_user_error!(
                "Failed to deregister repository: {:#}",
                pkg::config::determine_why_repository_server_is_not_running().await
            )
        }
        Err(err) => {
            return_user_error!("Failed to deregister repository: {}", err)
        }
    };
    // Remove any alias rules.
    let repo_url = format!("fuchsia-pkg://{repo_name}");
    remove_aliases(&repo_url, rewrite_proxy).await
}

async fn remove_aliases(repo_url: &str, rewrite_proxy: EngineProxy) -> Result<()> {
    // Check flag here for "overwrite" style
    do_transaction(&rewrite_proxy, |transaction| async {
        // Prepend the alias rules to the front so they take priority.
        let mut rules: Vec<Rule> = vec![];

        // These are rules to re-evaluate...
        let repo_rules_state = transaction.list_dynamic().await?;
        rules.extend(repo_rules_state);

        // Clear the list, since we'll be adding it back later.
        transaction.reset_all()?;

        // Keep rules that do not match the repo being removed.
        rules.retain(|r| r.host_replacement() != repo_url);

        // Add the rules back into the transaction. We do it in reverse, because `.add()`
        // always inserts rules into the front of the list.
        for rule in rules.into_iter().rev() {
            transaction.add(rule).await?
        }

        Ok(transaction)
    })
    .await
    .map_err(|err| {
        tracing::warn!("failed to create transactions: {:#?}", err);
        bug!("Failed to create transaction for aliases: {err}")
    })?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use camino::Utf8PathBuf;
    use ffx_config::ConfigLevel;
    use fho::TestBuffers;
    use fidl::endpoints::ServerEnd;
    use fidl_fuchsia_developer_ffx::{
        FileSystemRepositorySpec, PmRepositorySpec, RepositoryError, RepositoryIteratorRequest,
        RepositoryRegistryRequest, RepositorySpec,
    };
    use fidl_fuchsia_pkg_ext::{
        RepositoryConfigBuilder, RepositoryRegistrationAliasConflictMode, RepositoryStorageType,
    };
    use fidl_fuchsia_pkg_rewrite::{EditTransactionRequest, EngineRequest, RuleIteratorRequest};
    use futures::channel::oneshot::{channel, Receiver};
    use futures::StreamExt;
    use pkg::PkgServerInfo;
    use std::collections::BTreeSet;
    use std::net::Ipv4Addr;
    use std::process;
    use target_holders::fake_proxy;

    const REPO_NAME: &str = "some-name";
    const TARGET_NAME: &str = "some-target";

    fn fake_repo_list_handler(iterator: fidl::endpoints::ServerEnd<RepositoryIteratorMarker>) {
        fuchsia_async::Task::spawn(async move {
            let mut sent = false;
            let mut iterator = iterator.into_stream();
            while let Some(Ok(req)) = iterator.next().await {
                match req {
                    RepositoryIteratorRequest::Next { responder } => {
                        if !sent {
                            sent = true;
                            responder
                                .send(&[
                                    RepositoryConfig {
                                        name: "Test1".to_owned(),
                                        spec: RepositorySpec::FileSystem(
                                            FileSystemRepositorySpec {
                                                metadata_repo_path: Some("a/b/meta".to_owned()),
                                                blob_repo_path: Some("a/b/blobs".to_owned()),
                                                ..Default::default()
                                            },
                                        ),
                                    },
                                    RepositoryConfig {
                                        name: "Test2".to_owned(),
                                        spec: RepositorySpec::Pm(PmRepositorySpec {
                                            path: Some("c/d".to_owned()),
                                            aliases: Some(vec![
                                                "example.com".into(),
                                                "fuchsia.com".into(),
                                            ]),
                                            ..Default::default()
                                        }),
                                    },
                                ])
                                .unwrap()
                        } else {
                            responder.send(&[]).unwrap()
                        }
                    }
                }
            }
        })
        .detach();
    }

    async fn setup_fake_server() -> (RepositoryRegistryProxy, Receiver<(String, Option<String>)>) {
        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fake_proxy(move |req| match req {
            RepositoryRegistryRequest::DeregisterTarget {
                repository_name,
                target_identifier,
                responder,
            } => {
                sender.take().unwrap().send((repository_name, target_identifier)).unwrap();
                responder.send(Ok(())).unwrap();
            }
            RepositoryRegistryRequest::ListRepositories { iterator, .. } => {
                fake_repo_list_handler(iterator);
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    async fn setup_fake_repo_manager_server(
    ) -> (RepositoryManagerProxy, Receiver<(String, Option<String>)>) {
        let (_sender, receiver) = channel();
        let repos = fake_proxy(move |req| match req {
            fidl_fuchsia_pkg::RepositoryManagerRequest::Remove { responder, .. } => {
                responder.send(Ok(())).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        (repos, receiver)
    }

    fn make_server_instance(
        server_mode: ServerMode,
        name: String,
        context: &EnvironmentContext,
    ) -> Result<()> {
        let instance_root = context.get("repository.process_dir").expect("instance dir");
        let mgr = PkgServerInstances::new(instance_root);

        let address = (Ipv4Addr::LOCALHOST, 1234).into();

        let repo_config =
            RepositoryConfigBuilder::new(format!("fuchsia-pkg://{name}").parse().unwrap()).build();

        mgr.write_instance(&PkgServerInfo {
            name,
            address,
            repo_spec: fuchsia_repo::repository::RepositorySpec::Pm {
                path: Utf8PathBuf::from("/somewhere"),
                aliases: BTreeSet::new(),
            }
            .into(),
            registration_storage_type: RepositoryStorageType::Ephemeral,
            registration_alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
            server_mode,
            pid: process::id(),
            repo_config,
        })
        .map_err(Into::into)
    }

    macro_rules! rule {
        ($host_match:expr => $host_replacement:expr,
         $path_prefix_match:expr => $path_prefix_replacement:expr) => {
            Rule::new($host_match, $host_replacement, $path_prefix_match, $path_prefix_replacement)
                .unwrap()
        };
    }

    fn handle_edit_transaction(
        transaction: ServerEnd<fidl_fuchsia_pkg_rewrite::EditTransactionMarker>,
    ) {
        fuchsia_async::Task::spawn(async move {
            let mut tx_stream = transaction.into_stream();

            while let Some(Ok(req)) = tx_stream.next().await {
                match req {
                    EditTransactionRequest::ResetAll { control_handle: _ } => {}
                    EditTransactionRequest::ListDynamic { iterator, control_handle: _ } => {
                        let mut stream = iterator.into_stream();

                        let mut rules = vec![
                            rule!("fuchsia.com" => "example.com", "/" => "/"),
                            rule!("fuchsia.com" => "mycorp.com", "/" => "/"),
                        ]
                        .into_iter();

                        while let Some(Ok(req)) = stream.next().await {
                            let RuleIteratorRequest::Next { responder } = req;

                            if let Some(rule) = rules.next() {
                                responder.send(&[rule.into()]).unwrap();
                            } else {
                                responder.send(&[]).unwrap();
                            }
                        }
                    }
                    EditTransactionRequest::Add { responder, .. } => {
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

    fn setup_fake_engine_server() -> EngineProxy {
        let engine = fake_proxy(move |req| match req {
            EngineRequest::StartEditTransaction { transaction, control_handle: _ } => {
                handle_edit_transaction(transaction);
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        engine
    }

    #[fuchsia::test]
    async fn test_deregister() {
        let (repos, receiver) = setup_fake_server().await;
        let engine = setup_fake_engine_server();
        deregister_daemon(REPO_NAME, Some(TARGET_NAME.to_string()), repos, engine).await.unwrap();
        let got = receiver.await.unwrap();
        assert_eq!(got, (REPO_NAME.to_string(), Some(TARGET_NAME.to_string()),));
    }

    #[fuchsia::test]
    async fn test_deregister_default_repository_standalone() {
        let env = ffx_config::test_init().await.unwrap();

        let default_repo_name = "default-repo";
        pkg::config::set_default_repository(default_repo_name).await.unwrap();
        env.context
            .query("target.default")
            .level(Some(ConfigLevel::User))
            .set("some-target".into())
            .await
            .expect("Setting default target");

        let repos = fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let (repo_mgr, _) = setup_fake_repo_manager_server().await;

        make_server_instance(ServerMode::Foreground, default_repo_name.to_string(), &env.context)
            .expect("creating test server");

        let tool = DeregisterTool {
            cmd: DeregisterCommand { repository: None, port: None },
            repos,
            context: env.context.clone(),
            repo_proxy: repo_mgr,
            engine_proxy: setup_fake_engine_server(),
        };

        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);

        let res = tool.main(writer).await;
        assert!(res.is_ok(), "Expected result to be OK. Got: {res:?}");
    }

    #[fuchsia::test]
    async fn test_deregister_default_repository_deamon() {
        let env = ffx_config::test_init().await.unwrap();

        let default_repo_name = "default-repo";
        pkg::config::set_default_repository(default_repo_name).await.unwrap();
        env.context
            .query("target.default")
            .level(Some(ConfigLevel::User))
            .set("some-target".into())
            .await
            .expect("Setting default target");

        let (repos, receiver) = setup_fake_server().await;
        let (repo_mgr, _) = setup_fake_repo_manager_server().await;

        make_server_instance(ServerMode::Daemon, default_repo_name.to_string(), &env.context)
            .expect("creating test server");

        let tool = DeregisterTool {
            cmd: DeregisterCommand { repository: None, port: None },
            repos,
            context: env.context.clone(),
            repo_proxy: repo_mgr,
            engine_proxy: setup_fake_engine_server(),
        };

        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);

        let res = tool.main(writer).await;
        assert!(res.is_ok(), "Expected result to be OK. Got: {res:?}");

        let got = receiver.await;
        match got {
            Ok((got_repo_name, got_target_name)) => assert_eq!(
                (got_repo_name, got_target_name),
                (default_repo_name.to_string(), Some(TARGET_NAME.to_string()),)
            ),
            Err(_e) => assert!(got.is_ok(), "Expected OK result, got {got:?}"),
        };
    }

    #[fuchsia::test]
    async fn test_deregister_returns_error() {
        let repos = fake_proxy(move |req| match req {
            RepositoryRegistryRequest::DeregisterTarget {
                repository_name: _,
                target_identifier: _,
                responder,
            } => {
                responder.send(Err(RepositoryError::TargetCommunicationFailure)).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let engine = setup_fake_engine_server();

        assert!(deregister_daemon(REPO_NAME, Some(TARGET_NAME.to_string()), repos, engine)
            .await
            .is_err());
    }

    #[fuchsia::test]
    async fn test_deregister_standalone_not_found() {
        // command should still succeed if the repo_proxy returns NOT_FOUND.
        let env = ffx_config::test_init().await.unwrap();

        let default_repo_name = "default-repo";
        pkg::config::set_default_repository(default_repo_name).await.unwrap();
        env.context
            .query("target.default")
            .level(Some(ConfigLevel::User))
            .set("some-target".into())
            .await
            .expect("Setting default target");

        let repos = fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let repo_mgr = fake_proxy(move |req| match req {
            fidl_fuchsia_pkg::RepositoryManagerRequest::Remove { responder, .. } => {
                responder.send(Err(Status::NOT_FOUND.into_raw())).unwrap();
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        make_server_instance(ServerMode::Foreground, default_repo_name.to_string(), &env.context)
            .expect("creating test server");

        let tool = DeregisterTool {
            cmd: DeregisterCommand { repository: None, port: None },
            repos,
            context: env.context.clone(),
            repo_proxy: repo_mgr,
            engine_proxy: setup_fake_engine_server(),
        };

        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);

        let res = tool.main(writer).await;
        assert!(res.is_ok(), "Expected result to be OK. Got: {res:?}");
    }
}
