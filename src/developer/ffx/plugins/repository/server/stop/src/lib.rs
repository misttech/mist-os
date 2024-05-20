// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_repository_server_stop_args::StopCommand;
use fho::{daemon_protocol, return_bug, Error, FfxMain, FfxTool, Result, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_ffx_ext::RepositoryError;
use pkg::config as pkg_config;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { message: String },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}
#[derive(FfxTool)]
pub struct RepoStopTool {
    #[command]
    _cmd: StopCommand,
    #[with(daemon_protocol())]
    repos: ffx::RepositoryRegistryProxy,
}

fho::embedded_plugin!(RepoStopTool);

#[async_trait(?Send)]
impl FfxMain for RepoStopTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match stop(self.repos).await {
            Ok(info) => {
                let message = info.unwrap_or("Stopped the repository server".into());
                writer.machine_or(&CommandStatus::Ok { message: message.clone() }, message)?;
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

pub async fn stop(repos: ffx::RepositoryRegistryProxy) -> Result<Option<String>> {
    match repos.server_stop().await {
        Ok(Ok(())) => Ok(None),
        Ok(Err(err)) => {
            let err = RepositoryError::from(err);
            match err {
                RepositoryError::ServerNotRunning => {
                    Ok(Some("No repository server is running".into()))
                }
                err => {
                    // If we failed to communicate with the daemon, disable the server so it doesn't start
                    // next time the daemon starts.
                    let _ = pkg_config::set_repository_server_enabled(false).await;

                    return_bug!("Failed to stop the server: {}", RepositoryError::from(err))
                }
            }
        }
        Err(err) => {
            // If we failed to communicate with the daemon, disable the server so it doesn't start
            // next time the daemon starts.
            let _ = pkg_config::set_repository_server_enabled(false).await;

            return_bug!("Failed to communicate with the daemon: {}", err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fho::Format;
    use fidl_fuchsia_developer_ffx::{RepositoryRegistryMarker, RepositoryRegistryRequest};
    use futures::channel::oneshot::channel;

    #[fuchsia::test]
    async fn test_stop() {
        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStop { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Ok(())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = RepoStopTool { _cmd: StopCommand {}, repos };
        let buffers = fho::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(None, &buffers);
        let res = tool.main(writer).await;

        assert!(res.is_ok());
        assert!(receiver.await.is_ok());
    }

    #[fuchsia::test]
    async fn test_stop_disables_server_on_error() {
        let _env = ffx_config::test_init().await.unwrap();
        pkg_config::set_repository_server_enabled(true).await.unwrap();

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStop { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Err(RepositoryError::InternalError.into())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = RepoStopTool { _cmd: StopCommand {}, repos };
        let buffers = fho::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(None, &buffers);
        let res = tool.main(writer).await;

        assert!(res.is_err());
        assert!(receiver.await.is_ok());

        assert!(!pkg_config::get_repository_server_enabled().await.unwrap());
    }

    #[fuchsia::test]
    async fn test_stop_disables_server_on_communication_error() {
        let _env = ffx_config::test_init().await.unwrap();
        pkg_config::set_repository_server_enabled(true).await.unwrap();

        let (repos, stream) =
            fidl::endpoints::create_proxy_and_stream::<RepositoryRegistryMarker>().unwrap();
        drop(stream);

        let tool = RepoStopTool { _cmd: StopCommand {}, repos };
        let buffers = fho::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(None, &buffers);
        let res = tool.main(writer).await;

        assert!(res.is_err());
        assert!(!pkg_config::get_repository_server_enabled().await.unwrap());
    }

    #[fuchsia::test]
    async fn test_stop_machine() {
        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStop { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Ok(())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = RepoStopTool { _cmd: StopCommand {}, repos };
        let buffers = fho::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
        let res = tool.main(writer).await;

        assert!(res.is_ok());
        assert!(receiver.await.is_ok());

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <RepoStopTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        assert_eq!(json, serde_json::json!({"ok": { "message": "Stopped the repository server"}}));
        assert_eq!(stderr, "");
    }

    #[fuchsia::test]
    async fn test_stop_error_machine() {
        let _env = ffx_config::test_init().await.unwrap();

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStop { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Err(RepositoryError::InternalError.into())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = RepoStopTool { _cmd: StopCommand {}, repos };
        let buffers = fho::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
        let res = tool.main(writer).await;

        assert!(receiver.await.is_ok());

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_err(), "expected error: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <RepoStopTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        assert_eq!(
            json,
            serde_json::json!({"unexpected_error": {
             "message": "BUG: An internal command error occurred.\nError: Failed to stop the server: some unspecified internal error"}})
        );
        assert_eq!(stderr, "");
    }
}
