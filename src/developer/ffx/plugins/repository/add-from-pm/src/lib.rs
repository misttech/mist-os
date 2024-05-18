// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use errors::{ffx_bail, ffx_error};
use ffx_core::ffx_plugin;
use ffx_repository_add_from_pm_args::AddFromPmCommand;
use fidl_fuchsia_developer_ffx::{RepositoryIteratorMarker, RepositoryRegistryProxy};
use fidl_fuchsia_developer_ffx_ext::{RepositoryError, RepositorySpec};
use fuchsia_url::RepositoryUrl;

enum RepoRegState {
    New,
    Duplicate,
}

#[ffx_plugin(RepositoryRegistryProxy = "daemon::protocol")]
pub async fn add_from_pm(cmd: AddFromPmCommand, repos: RepositoryRegistryProxy) -> Result<()> {
    // Validate that we can construct a valid repository url from the name.
    let repo_url = RepositoryUrl::parse_host(cmd.repository.to_string())
        .map_err(|err| ffx_error!("invalid repository name for {:?}: {}", cmd.repository, err))?;
    let repo_name = repo_url.host();

    let full_path = cmd
        .pm_repo_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {:?}", cmd.pm_repo_path))?;

    let repo_spec = RepositorySpec::Pm {
        path: full_path.try_into()?,
        aliases: cmd.aliases.into_iter().collect(),
    };

    match repo_registered_state(&repos, repo_name, &repo_spec).await? {
        RepoRegState::Duplicate => {
            tracing::info!("attempt to re-add {repo_name} using the same parameters, ignoring");
            Ok(())
        }
        RepoRegState::New => match repos.add_repository(repo_name, &repo_spec.into()).await? {
            Ok(()) => {
                println!("added repository {}", repo_name);
                Ok(())
            }
            Err(err) => {
                let err = RepositoryError::from(err);
                ffx_bail!("Adding repository {} failed: {}", repo_name, err);
            }
        },
    }
}

async fn repo_registered_state(
    repos: &RepositoryRegistryProxy,
    repo_name: &str,
    repo_spec: &RepositorySpec,
) -> Result<RepoRegState> {
    let (client, server) =
        ffx_core::macro_deps::fidl::endpoints::create_endpoints::<RepositoryIteratorMarker>();
    repos.list_repositories(server).context("listing repositories")?;
    let client = client.into_proxy().context("creating repository iterator proxy")?;

    loop {
        let batch = client.next().await.context("fetching next batch of repositories")?;
        if batch.is_empty() {
            break;
        }

        for repo in batch {
            if repo.name == repo_name {
                let spec = RepositorySpec::try_from(repo.spec)?;
                // If the repo specs match, we can continue with a warning.
                if spec == *repo_spec {
                    return Ok(RepoRegState::Duplicate);
                } else {
                    return Err(anyhow!("repository {repo_name} is registered using different settings, it could be removed by running `ffx repository remove {repo_name}"));
                }
            }
        }
    }

    Ok(RepoRegState::New)
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_developer_ffx::{
        PmRepositorySpec, RepositoryConfig, RepositoryIteratorRequest, RepositoryRegistryMarker,
        RepositoryRegistryRequest, RepositoryRegistryRequestStream, RepositorySpec,
    };
    use fuchsia_async as fasync;
    use futures::{SinkExt, StreamExt, TryStreamExt};

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
                            let mut iterator = iterator.into_stream().unwrap();
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

    #[fasync::run_singlethreaded(test)]
    async fn test_add_from_pm() {
        let tmp = tempfile::tempdir().unwrap();

        let (sender, mut receiver) = futures::channel::mpsc::channel::<_>(1);

        let (repos, stream) = ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<
            RepositoryRegistryMarker,
        >()
        .unwrap();

        let _ = FakeRepositoryRegistry::new(stream, sender);

        add_from_pm(
            AddFromPmCommand {
                repository: "my-repo".to_owned(),
                pm_repo_path: tmp.path().to_path_buf(),
                aliases: vec![],
            },
            repos,
        )
        .await
        .unwrap();

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
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_add_from_pm_rejects_invalid_names() {
        let tmp = tempfile::tempdir().unwrap();

        let repos =
            setup_fake_repos(move |req| panic!("should not receive any requests: {:?}", req));

        for name in ["", "my_repo", "MyRepo", "😀"] {
            assert_matches!(
                add_from_pm(
                    AddFromPmCommand {
                        repository: name.to_owned(),
                        pm_repo_path: tmp.path().to_path_buf(),
                        aliases: vec![],
                    },
                    repos.clone(),
                )
                .await,
                Err(_)
            );
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_add_from_pm_exits_when_existing_repo() {
        let tmp = tempfile::tempdir().unwrap();

        let (sender, mut receiver) = futures::channel::mpsc::channel::<_>(1);

        let (repos, stream) = ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<
            RepositoryRegistryMarker,
        >()
        .unwrap();

        let _ = FakeRepositoryRegistry::new(stream, sender);

        add_from_pm(
            AddFromPmCommand {
                repository: "my-repo".to_owned(),
                pm_repo_path: tmp.path().to_path_buf(),
                aliases: vec![],
            },
            repos.clone(),
        )
        .await
        .unwrap();

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
        assert_eq!(
            add_from_pm(
                AddFromPmCommand {
                    repository: "my-repo".to_owned(),
                    pm_repo_path: tmp.path().to_path_buf(),
                    aliases: vec![],
                },
                repos.clone(),
            )
            .await
            .is_err(),
            false
        );

        // Adding a repo under the same name with a different config
        // should fail, as it would overwrite the already added repo.
        assert_eq!(
            add_from_pm(
                AddFromPmCommand {
                    repository: "my-repo".to_owned(),
                    pm_repo_path: "/foo".into(),
                    aliases: vec![],
                },
                repos,
            )
            .await
            .is_err(),
            true
        );
    }
}
