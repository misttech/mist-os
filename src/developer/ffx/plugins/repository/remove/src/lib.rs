// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_repository_remove_args::RemoveCommand;
use fho::{
    bug, daemon_protocol, return_user_error, user_error, FfxMain, FfxTool, Result, SimpleWriter,
};
use fidl_fuchsia_developer_ffx::{
    RepositoryConfig, RepositoryIteratorMarker, RepositoryRegistryProxy,
};

#[derive(FfxTool)]
pub struct RepoRemoveTool {
    #[command]
    pub cmd: RemoveCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
}

fho::embedded_plugin!(RepoRemoveTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for RepoRemoveTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> Result<()> {
        remove(self.cmd, self.repos).await
    }
}

pub async fn remove(cmd: RemoveCommand, repos_proxy: RepositoryRegistryProxy) -> Result<()> {
    let names = if cmd.all {
        let repos = list_repositories(repos_proxy.clone()).await?;
        repos.iter().map(|r| r.name.to_string()).collect()
    } else if let Some(name) = cmd.name {
        vec![name]
    } else {
        return_user_error!(
            "No repository name specified. Use `--all` or specify the repository name to remove."
        )
    };

    let mut errors: Vec<fho::Error> = vec![];
    for repo_name in names {
        if !repos_proxy.remove_repository(&repo_name).await.map_err(|e| bug!(e))? {
            errors.push(user_error!("No repository named \"{}\".", repo_name));
        }
    }

    match errors.len() {
        0 => Ok(()),
        1 => Err(user_error!("{}", errors.get(0).unwrap())),
        _ => Err(user_error!(
            "Multiple repositories could not be removed:\n\t{}",
            errors.iter().map(ToString::to_string).collect::<Vec<String>>().join("\n\t")
        )),
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

#[cfg(test)]
mod test {
    use super::*;
    use fho::TestBuffers;
    use fidl_fuchsia_developer_ffx::{
        FileSystemRepositorySpec, PmRepositorySpec, RepositoryIteratorRequest,
        RepositoryRegistryRequest, RepositorySpec,
    };
    use futures::channel::oneshot::channel;
    use futures::StreamExt;

    #[fuchsia::test]
    async fn test_remove() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RemoveRepository { name, responder } => {
                sender.take().unwrap().send(name).unwrap();
                responder.send(true).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        remove(RemoveCommand { all: false, name: Some("MyRepo".to_owned()) }, repos).await.unwrap();
        assert_eq!(receiver.await.unwrap(), "MyRepo".to_owned());
    }

    #[fuchsia::test]
    async fn test_remove_fail() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RemoveRepository { responder, .. } => {
                responder.send(false).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        assert!(remove(RemoveCommand { all: false, name: Some("NotMyRepo".to_owned()) }, repos)
            .await
            .is_err());
    }

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

    #[fuchsia::test]
    async fn test_remove_all() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RemoveRepository { responder, .. } => {
                responder.send(true).unwrap()
            }
            RepositoryRegistryRequest::ListRepositories { iterator, .. } => {
                fake_repo_list_handler(iterator);
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);
        let tool = RepoRemoveTool { cmd: RemoveCommand { all: true, name: None }, repos };
        let result = tool.main(writer).await;
        let (stdout, stderr) = buffers.into_strings();
        assert_eq!(stderr, "");
        assert_eq!(stdout, "");
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
    }

    #[fuchsia::test]
    async fn test_remove_all_some_fail() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::RemoveRepository { responder, name } => {
                responder.send(name == "Test1").unwrap()
            }
            RepositoryRegistryRequest::ListRepositories { iterator, .. } => {
                fake_repo_list_handler(iterator)
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);
        let tool = RepoRemoveTool { cmd: RemoveCommand { all: true, name: None }, repos };
        let result = tool.main(writer).await;
        let (stdout, stderr) = buffers.into_strings();
        assert_eq!(stderr, "");
        assert_eq!(stdout, "");
        assert!(result.is_err(), "Expected Err, got {result:?}");
        assert_eq!(result.err().expect("an error").to_string(), "No repository named \"Test2\".");
    }
}
