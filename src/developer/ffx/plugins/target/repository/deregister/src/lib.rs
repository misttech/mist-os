// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_target_repository_deregister_args::DeregisterCommand;
use ffx_writer::SimpleWriter;
use fho::{bug, return_bug, user_error, FfxMain, FfxTool, Result};
use fidl_fuchsia_pkg::RepositoryManagerProxy;
use fidl_fuchsia_pkg_rewrite::EngineProxy;
use fidl_fuchsia_pkg_rewrite_ext::{do_transaction, Rule};
use pkg::{PkgServerInstanceInfo as _, PkgServerInstances};
use target_holders::moniker;
use zx_status::Status;

const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
#[derive(FfxTool)]
pub struct DeregisterTool {
    #[command]
    cmd: DeregisterCommand,
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

        if let Some(server_info) = pkg_server_info {
            deregister_standalone(&server_info.name, self.repo_proxy, self.engine_proxy).await?
        } else {
            deregister_standalone(&repo_name, self.repo_proxy, self.engine_proxy).await?
        }
        Ok(())
    }
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
    use ffx_writer::TestBuffers;
    use fidl::endpoints::ServerEnd;
    use fidl_fuchsia_pkg_ext::{
        RepositoryConfigBuilder, RepositoryRegistrationAliasConflictMode, RepositoryStorageType,
    };
    use fidl_fuchsia_pkg_rewrite::{EditTransactionRequest, EngineRequest, RuleIteratorRequest};
    use futures::channel::oneshot::{channel, Receiver};
    use futures::StreamExt;
    use pkg::{PkgServerInfo, ServerMode};
    use std::collections::BTreeSet;
    use std::net::Ipv4Addr;
    use std::process;
    use target_holders::fake_proxy;

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

        let (repo_mgr, _) = setup_fake_repo_manager_server().await;

        make_server_instance(ServerMode::Foreground, default_repo_name.to_string(), &env.context)
            .expect("creating test server");

        let tool = DeregisterTool {
            cmd: DeregisterCommand { repository: None, port: None },
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
