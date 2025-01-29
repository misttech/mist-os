// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use async_trait::async_trait;
use camino::FromPathBufError;
use errors::ffx_error;
use ffx::RepositoryIteratorMarker;
use ffx_repository_add_from_pm_args::AddFromPmCommand;
use fho::{
    bug, return_user_error, user_error, Error, FfxContext, FfxMain, FfxTool, Result,
    VerifiedMachineWriter,
};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_ffx_ext::{RepositoryError, RepositorySpec};
use fuchsia_url::RepositoryUrl;
use pkg::config as pkg_config;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeSet;
use target_holders::daemon_protocol;

enum RepoRegState {
    New,
    Duplicate,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { repo_name: String },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}

#[derive(FfxTool)]
pub struct AddFromPmTool {
    #[command]
    cmd: AddFromPmCommand,
    #[with(daemon_protocol())]
    repos: ffx::RepositoryRegistryProxy,
}

fho::embedded_plugin!(AddFromPmTool);

#[async_trait(?Send)]
impl FfxMain for AddFromPmTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        match self.add_from_pm().await.map_err(Into::into) {
            Ok(repo_name) => {
                writer.machine_or(
                    &CommandStatus::Ok { repo_name: repo_name.clone() },
                    format!("added repository {repo_name}"),
                )?;
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

impl AddFromPmTool {
    pub async fn add_from_pm(&self) -> Result<String> {
        // Validate that we can construct a valid repository url from the name.
        let repo_url =
            RepositoryUrl::parse_host(self.cmd.repository.to_string()).map_err(|err| {
                user_error!("invalid repository name for {:?}: {}", self.cmd.repository, err)
            })?;
        let repo_name = repo_url.host();
        let full_path =
            self.cmd.pm_repo_path.canonicalize().with_bug_context(|| {
                format!("failed to canonicalize {:?}", self.cmd.pm_repo_path)
            })?;

        let repo_spec = RepositorySpec::Pm {
            path: full_path.try_into().map_err(|e: FromPathBufError| bug!(e))?,
            aliases: self.cmd.aliases.clone().into_iter().collect(),
        };

        match self.repo_registered_state(repo_name, &repo_spec).await? {
            RepoRegState::Duplicate => {
                tracing::info!("attempt to re-add {repo_name} using the same parameters, ignoring");
                Ok(repo_name.to_string())
            }
            RepoRegState::New => match self
                .repos
                .add_repository(repo_name, &repo_spec.clone().into())
                .await
                .map_err(|e| bug!(e))?
            {
                Ok(()) => {
                    // Save the filesystem configuration.
                    pkg_config::set_repository(repo_name, &repo_spec.into())
                        .await
                        .map_err(|err| ffx_error!("Failed to save repository: {:#?}", err))?;
                    Ok(repo_name.to_string())
                }
                Err(err) => {
                    let err = RepositoryError::from(err);
                    Err(user_error!("Adding repository {} failed: {}", repo_name, err))
                }
            },
        }
    }

    async fn repo_registered_state(
        &self,
        repo_name: &str,
        repo_spec: &RepositorySpec,
    ) -> Result<RepoRegState> {
        let (client, server) =
            ffx_core::macro_deps::fidl::endpoints::create_endpoints::<RepositoryIteratorMarker>();
        self.repos.list_repositories(server).bug_context("listing repositories")?;
        let client = client.into_proxy();
        let (repo_path, repo_aliases) = match repo_spec {
            RepositorySpec::Pm { path, aliases } => (Some(path.to_string()), aliases),
            _ => return_user_error!("only pm style repositories are supported"),
        };

        loop {
            let batch = client.next().await.bug_context("fetching next batch of repositories")?;
            if batch.is_empty() {
                break;
            }

            for repo in batch {
                if repo.name == repo_name {
                    // If the same path and aliases are specified, we can continue with a warning.
                    match repo.spec {
                        fidl_fuchsia_developer_ffx::RepositorySpec::Pm(spec) => {
                            let spec_aliases = if let Some(aliases) = spec.aliases {
                                BTreeSet::from_iter(aliases.into_iter())
                            } else {
                                BTreeSet::new()
                            };
                            if repo_path == spec.path && *repo_aliases == spec_aliases {
                                return Ok(RepoRegState::Duplicate);
                            }
                        }
                        _ => {
                            return_user_error!("repository {repo_name} is registered using different settings, it could be removed by running `ffx repository remove {repo_name}");
                        }
                    }
                }
            }
        }

        Ok(RepoRegState::New)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fho::Format;
    use fidl_fuchsia_developer_ffx::{
        PmRepositorySpec, RepositoryConfig, RepositoryIteratorRequest, RepositoryRegistryMarker,
        RepositoryRegistryRequest, RepositoryRegistryRequestStream, RepositorySpec,
    };
    use futures::{SinkExt, StreamExt, TryStreamExt};
    use serde_json::json;
    use target_holders::fake_proxy;

    struct FakeRepositoryRegistry;

    impl FakeRepositoryRegistry {
        fn new(
            mut stream: RepositoryRegistryRequestStream,
            mut sender: futures::channel::mpsc::Sender<(String, RepositorySpec)>,
        ) -> () {
            ffx_core::macro_deps::fuchsia_async::Task::local(async move {
                let mut repos = vec![];
                let mut sent = false;
                while let Ok(Some(req)) = stream.try_next().await {
                    match req {
                        RepositoryRegistryRequest::AddRepository {
                            name,
                            repository,
                            responder,
                        } => {
                            sender.send((name.clone(), repository.clone())).await.unwrap();
                            repos.push(RepositoryConfig { name: name, spec: repository });
                            responder.send(Ok(())).unwrap();
                        }
                        RepositoryRegistryRequest::ListRepositories { iterator, .. } => {
                            let mut iterator = iterator.into_stream();
                            while let Some(Ok(req)) = iterator.next().await {
                                match req {
                                    RepositoryIteratorRequest::Next { responder } => {
                                        if !sent {
                                            sent = true;
                                            responder.send(&repos).unwrap();
                                        } else {
                                            responder.send(&[]).unwrap()
                                        }
                                    }
                                }
                            }
                        }
                        other => panic!("Unexpected request: {:?}", other),
                    }
                }
            })
            .detach();
        }
    }

    #[fuchsia::test]
    async fn test_add_from_pm() {
        let _test_env = ffx_config::test_init().await.expect("test env init");
        let tmp = tempfile::tempdir().unwrap();

        let (sender, mut receiver) = futures::channel::mpsc::channel::<_>(1);

        let (repos, stream) = ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<
            RepositoryRegistryMarker,
        >();

        let _ = FakeRepositoryRegistry::new(stream, sender);

        let tool = AddFromPmTool {
            cmd: AddFromPmCommand {
                repository: "my-repo".to_owned(),
                pm_repo_path: tmp.path().to_path_buf(),
                aliases: vec![],
            },
            repos,
        };
        let buffers = fho::TestBuffers::default();
        let writer = <AddFromPmTool as FfxMain>::Writer::new_test(None, &buffers);

        tool.main(writer).await.expect("main ok");

        assert_eq!(
            receiver.next().await.unwrap(),
            (
                "my-repo".to_owned(),
                RepositorySpec::Pm(PmRepositorySpec {
                    path: Some(tmp.path().canonicalize().unwrap().to_str().unwrap().to_string()),
                    ..Default::default()
                })
            )
        );
        let got = buffers.into_stdout_str();
        assert_eq!(got, "added repository my-repo\n");
    }

    #[fuchsia::test]
    async fn test_add_from_pm_rejects_invalid_names() {
        let _test_env = ffx_config::test_init().await.expect("test env init");
        let tmp = tempfile::tempdir().unwrap();

        let repos: ffx::RepositoryRegistryProxy =
            fake_proxy(move |req| panic!("should not receive any requests: {:?}", req));

        for (name, want_msg) in [
            ("", "invalid repository name for \"\": url parse error"),
            ("my_repo", "invalid repository name for \"my_repo\": invalid host"),
            ("MyRepo", "invalid repository name for \"MyRepo\": invalid host"),
            ("😀", "invalid repository name for \"😀\": invalid host"),
        ] {
            let tool = AddFromPmTool {
                cmd: AddFromPmCommand {
                    repository: name.to_owned(),
                    pm_repo_path: tmp.path().to_path_buf(),
                    aliases: vec![],
                },
                repos: repos.clone(),
            };
            let buffers = fho::TestBuffers::default();
            let writer = <AddFromPmTool as FfxMain>::Writer::new_test(None, &buffers);
            let got_msg = tool.main(writer).await.expect_err("expected error").to_string();
            assert_eq!(got_msg, want_msg);
        }
    }

    #[fuchsia::test]
    async fn test_add_from_pm_machine() {
        let _test_env = ffx_config::test_init().await.expect("test env init");
        let tmp = tempfile::tempdir().unwrap();

        let (sender, mut receiver) = futures::channel::mpsc::channel::<_>(1);

        let (repos, stream) = ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<
            RepositoryRegistryMarker,
        >();

        let _ = FakeRepositoryRegistry::new(stream, sender);

        let tool = AddFromPmTool {
            cmd: AddFromPmCommand {
                repository: "my-new-repo".to_owned(),
                pm_repo_path: tmp.path().to_path_buf(),
                aliases: vec![],
            },
            repos,
        };
        let buffers = fho::TestBuffers::default();
        let writer = <AddFromPmTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;

        let got = receiver.next().await.unwrap();
        assert_eq!(
            got,
            (
                "my-new-repo".to_owned(),
                RepositorySpec::Pm(PmRepositorySpec {
                    path: Some(tmp.path().canonicalize().unwrap().to_str().unwrap().to_string()),
                    ..Default::default()
                })
            )
        );
        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <AddFromPmTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        assert_eq!(json, json!({"ok": { "repo_name": "my-new-repo"}}));
        assert_eq!(stderr, "");
    }

    #[fuchsia::test]
    async fn test_add_from_pm_rejects_invalid_names_machine() {
        let _test_env = ffx_config::test_init().await.expect("test env init");
        let tmp = tempfile::tempdir().unwrap();

        let repos: ffx::RepositoryRegistryProxy =
            fake_proxy(move |req| panic!("should not receive any requests: {:?}", req));

        for (name, want_msg) in [
            ("", "invalid repository name for \"\": url parse error"),
            ("my_repo", "invalid repository name for \"my_repo\": invalid host"),
            ("MyRepo", "invalid repository name for \"MyRepo\": invalid host"),
            ("😀", "invalid repository name for \"😀\": invalid host"),
        ] {
            let tool = AddFromPmTool {
                cmd: AddFromPmCommand {
                    repository: name.to_owned(),
                    pm_repo_path: tmp.path().to_path_buf(),
                    aliases: vec![],
                },
                repos: repos.clone(),
            };
            let buffers = fho::TestBuffers::default();
            let writer = <AddFromPmTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
            let res = tool.main(writer).await;

            let (stdout, stderr) = buffers.into_strings();
            assert!(res.is_err(), "expected err: {stdout} {stderr}");
            let err = format!("schema not valid {stdout}");
            let json = serde_json::from_str(&stdout).expect(&err);
            let err = format!("json must adhere to schema: {json}");
            <AddFromPmTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
            assert_eq!(json, json!({"user_error": { "message": want_msg}}));
        }
    }

    #[fuchsia::test]
    async fn test_add_from_pm_exits_when_existing_repo() {
        let _test_env = ffx_config::test_init().await.expect("test env init");
        let tmp = tempfile::tempdir().unwrap();

        let (sender, mut receiver) = futures::channel::mpsc::channel::<_>(1);

        let (repos, stream) = ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<
            RepositoryRegistryMarker,
        >();

        let _ = FakeRepositoryRegistry::new(stream, sender);

        let tool = AddFromPmTool {
            cmd: AddFromPmCommand {
                repository: "my-repo".to_owned(),
                pm_repo_path: tmp.path().to_path_buf(),
                aliases: vec![],
            },
            repos: repos.clone(),
        };

        tool.add_from_pm().await.unwrap();

        assert_eq!(
            receiver.next().await.unwrap(),
            (
                "my-repo".to_owned(),
                RepositorySpec::Pm(PmRepositorySpec {
                    path: Some(tmp.path().canonicalize().unwrap().to_str().unwrap().to_string()),
                    ..Default::default()
                })
            )
        );

        // Adding the same repo (same name, same config)
        // is expected to be successful, as add_from_pm
        // will ignore request in this case.

        let tool = AddFromPmTool {
            cmd: AddFromPmCommand {
                repository: "my-repo".to_owned(),
                pm_repo_path: tmp.path().to_path_buf(),
                aliases: vec![],
            },
            repos: repos.clone(),
        };

        assert_eq!(tool.add_from_pm().await.is_err(), false);

        // Adding a repo under the same name with a different config
        // should fail, as it would overwrite the already added repo.
        let tool = AddFromPmTool {
            cmd: AddFromPmCommand {
                repository: "my-repo".to_owned(),
                pm_repo_path: "/foo".into(),
                aliases: vec![],
            },
            repos,
        };

        assert_eq!(tool.add_from_pm().await.is_err(), true);
    }
}
