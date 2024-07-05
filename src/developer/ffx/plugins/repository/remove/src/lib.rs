// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_repository_remove_args::RemoveCommand;
use fho::{bug, daemon_protocol, return_user_error, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_ffx::RepositoryRegistryProxy;

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

pub async fn remove(cmd: RemoveCommand, repos: RepositoryRegistryProxy) -> Result<()> {
    if !repos.remove_repository(&cmd.name).await.map_err(|e| bug!(e))? {
        return_user_error!("No repository named \"{}\".", cmd.name);
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::RepositoryRegistryRequest;
    use futures::channel::oneshot::channel;

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
        remove(RemoveCommand { name: "MyRepo".to_owned() }, repos).await.unwrap();
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
        assert!(remove(RemoveCommand { name: "NotMyRepo".to_owned() }, repos).await.is_err());
    }
}
