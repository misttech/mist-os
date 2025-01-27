// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_repository_server_status_args::StatusCommand;
use fho::{bug, FfxMain, FfxTool, MachineWriter, Result, ToolIO as _};
use fidl_fuchsia_developer_ffx::RepositoryRegistryProxy;
use fidl_fuchsia_developer_ffx_ext::ServerStatus;
use std::io::Write as _;
use target_holders::daemon_protocol;

#[derive(FfxTool)]
pub struct RepoStatusTool {
    #[command]
    pub cmd: StatusCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
}

fho::embedded_plugin!(RepoStatusTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for RepoStatusTool {
    type Writer = MachineWriter<ServerStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        status_impl(self.repos, &mut writer).await
    }
}

async fn status_impl(
    repos: RepositoryRegistryProxy,
    writer: &mut <RepoStatusTool as FfxMain>::Writer,
) -> Result<()> {
    let status = repos.server_status().await.map_err(|e| bug!(e))?;
    let status = ServerStatus::try_from(status).map_err(|e| bug!(e))?.into();

    if writer.is_machine() {
        writer.machine(&status)?;
    } else {
        match status {
            ServerStatus::Disabled => {
                let err = pkg::config::determine_why_repository_server_is_not_running().await;
                writeln!(writer, "Server is disabled: {:?}", err).map_err(|e| bug!(e))?;
            }
            ServerStatus::Stopped => writeln!(writer, "Server is stopped").map_err(|e| bug!(e))?,
            ServerStatus::Running { address } => {
                writeln!(writer, "Server is running on {}", address).map_err(|e| bug!(e))?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::RepositoryRegistryRequest;
    use futures::channel::oneshot::channel;
    use std::net::Ipv4Addr;

    #[fuchsia::test]
    async fn test_status() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStatus { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder
                    .send(
                        &ServerStatus::Running { address: (Ipv4Addr::LOCALHOST, 0).into() }.into(),
                    )
                    .unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let buffers = TestBuffers::default();
        let mut out = MachineWriter::new_test(None, &buffers);
        assert!(status_impl(repos, &mut out).await.is_ok());
        let () = receiver.await.unwrap();

        assert_eq!(buffers.into_stdout_str(), "Server is running on 127.0.0.1:0\n",);
    }

    #[fuchsia::test]
    async fn test_status_disabled() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");
        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStatus { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(&ServerStatus::Disabled.into()).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let buffers = TestBuffers::default();
        let mut out = MachineWriter::new_test(None, &buffers);
        assert!(status_impl(repos, &mut out).await.is_ok());
        let () = receiver.await.unwrap();

        assert_eq!(
            buffers.into_stdout_str(),
            "Server is disabled: Server is disabled. It can be started with:\n\
                $ ffx repository server start\n",
        );
    }

    #[fuchsia::test]
    async fn test_status_machine() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");
        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStatus { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder
                    .send(
                        &ServerStatus::Running { address: (Ipv4Addr::LOCALHOST, 0).into() }.into(),
                    )
                    .unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let buffers = TestBuffers::default();
        let mut out = MachineWriter::new_test(Some(Format::Json), &buffers);
        assert!(status_impl(repos, &mut out).await.is_ok());
        let () = receiver.await.unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&buffers.into_stdout_str()).unwrap(),
            serde_json::json!({
                "state": "running",
                "address": "127.0.0.1:0",
            }),
        );
    }
}
