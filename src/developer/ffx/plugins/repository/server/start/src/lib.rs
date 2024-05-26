// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_repository_server_start_args::StartCommand;
use fho::{
    daemon_protocol, return_bug, return_user_error, Error, FfxContext, FfxMain, FfxTool, Result,
    VerifiedMachineWriter,
};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_ffx_ext::RepositoryError;
use fidl_fuchsia_net_ext::SocketAddress;
use pkg::config as pkg_config;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// The output is untagged and OK is flattened to match
// the legacy output. One day, we'll update the schema and
// worry about migration then.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(untagged)]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok {
        #[serde(flatten)]
        address: ServerInfo,
    },
    /// Unexpected error with string.
    UnexpectedError { error_message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { error_message: String },
}

#[derive(FfxTool)]
pub struct ServerStartTool {
    #[command]
    cmd: StartCommand,
    #[with(daemon_protocol())]
    repos: ffx::RepositoryRegistryProxy,
}

fho::embedded_plugin!(ServerStartTool);

#[async_trait(?Send)]
impl FfxMain for ServerStartTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        match start_impl(self.cmd, self.repos).await {
            Ok(server_addr) => {
                writer.machine_or(
                    &CommandStatus::Ok { address: ServerInfo { address: server_addr } },
                    format!("Repository server is listening on {server_addr}"),
                )?;
                Ok(())
            }
            Err(e @ Error::User(_)) => {
                writer.machine(&CommandStatus::UserError { error_message: e.to_string() })?;
                Err(e)
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { error_message: e.to_string() })?;
                Err(e)
            }
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerInfo {
    address: std::net::SocketAddr,
}

async fn start_impl(
    cmd: StartCommand,
    repos: ffx::RepositoryRegistryProxy,
) -> Result<std::net::SocketAddr> {
    let listen_address = match {
        if let Some(addr_flag) = cmd.address {
            Ok(Some(addr_flag))
        } else {
            pkg_config::repository_listen_addr().await
        }
    } {
        Ok(Some(address)) => address,
        Ok(None) => {
            return_user_error!(
                "The server listening address is unspecified.\n\
                You can fix this by setting your ffx config.\n\
                \n\
                $ ffx config set repository.server.listen '[::]:8083'\n\
                $ ffx repository server start
                \n\
                Or alternatively specify at runtime:\n\
                $ ffx repository server start --address <IP4V_or_IP6V_addr>",
            )
        }
        Err(err) => {
            return_user_error!(
                "Failed to read repository server from ffx config or runtime flag: {:#?}",
                err
            )
        }
    };

    let runtime_address =
        if cmd.address.is_some() { Some(SocketAddress(listen_address).into()) } else { None };

    match repos
        .server_start(runtime_address.as_ref())
        .await
        .bug_context("communicating with daemon")?
        .map_err(RepositoryError::from)
    {
        Ok(address) => {
            let address = SocketAddress::from(address);

            // Error out if the server is listening on a different address. Either we raced some
            // other `start` command, or the server was already running, and someone changed the
            // `repository.server.listen` address without then stopping the server.
            if listen_address.port() != 0 && listen_address != address.0 {
                return_user_error!(
                    "The server is listening on {} but is configured to listen on {}.\n\
                    You will need to restart the server for it to listen on the\n\
                    new address. You can fix this with:\n\
                    \n\
                    $ ffx repository server stop\n\
                    $ ffx repository server start",
                    listen_address,
                    address
                )
            }

            Ok(address.0)
        }
        Err(err @ RepositoryError::ServerAddressAlreadyInUse) => {
            return_bug!("Failed to start repository server on {}: {}", listen_address, err)
        }
        Err(RepositoryError::ServerNotRunning) => {
            return_bug!(
                "Failed to start repository server on {}: {:#}",
                listen_address,
                pkg::config::determine_why_repository_server_is_not_running().await
            )
        }
        Err(err) => {
            return_bug!("Failed to start repository server on {}: {}", listen_address, err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fho::Format;
    use fho::TestBuffers;
    use fidl_fuchsia_developer_ffx::{RepositoryError, RepositoryRegistryRequest};
    use futures::channel::oneshot::channel;
    use std::net::Ipv4Addr;

    #[fuchsia::test]
    async fn test_start() {
        let test_env = ffx_config::test_init().await.expect("test initialization");

        let address = (Ipv4Addr::LOCALHOST, 1234).into();
        test_env
            .context
            .query("repository.server.listen")
            .level(Some(ffx_config::ConfigLevel::User))
            .set("127.0.0.1:1234".into())
            .await
            .unwrap();

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Ok(&SocketAddress(address).into())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = ServerStartTool { cmd: StartCommand { address: None }, repos };
        let buffers = TestBuffers::default();
        let writer = <ServerStartTool as FfxMain>::Writer::new_test(None, &buffers);

        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        assert_eq!(stderr, "");
        assert_eq!(receiver.await, Ok(()));
    }

    #[fuchsia::test]
    async fn test_start_runtime_port() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");

        let address = (Ipv4Addr::LOCALHOST, 8084).into();

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStart { responder, address: Some(_test) } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Ok(&SocketAddress(address).into())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = ServerStartTool {
            cmd: StartCommand { address: Some("127.0.0.1:8084".parse().unwrap()) },
            repos,
        };
        let buffers = TestBuffers::default();
        let writer = <ServerStartTool as FfxMain>::Writer::new_test(None, &buffers);

        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        assert_eq!(stderr, "");
        assert_eq!(receiver.await, Ok(()));
    }

    #[fuchsia::test]
    async fn test_start_machine() {
        let test_env = ffx_config::test_init().await.expect("test initialization");

        let address = (Ipv4Addr::LOCALHOST, 1234).into();
        test_env
            .context
            .query("repository.server.listen")
            .level(Some(ffx_config::ConfigLevel::User))
            .set("127.0.0.1:1234".into())
            .await
            .unwrap();

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Ok(&SocketAddress(address).into())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = ServerStartTool { cmd: StartCommand { address: None }, repos };
        let buffers = TestBuffers::default();
        let writer = <ServerStartTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <ServerStartTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        assert_eq!(stderr, "");
        assert_eq!(receiver.await, Ok(()));

        // Make sure the output for ok is backwards compatible with the old schema.
        assert_eq!(stdout, "{\"address\":\"127.0.0.1:1234\"}\n");
    }

    #[fuchsia::test]
    async fn test_start_failed() {
        let _test_env = ffx_config::test_init().await.expect("test initialization");

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });

        let tool = ServerStartTool { cmd: StartCommand { address: None }, repos };
        let buffers = TestBuffers::default();
        let writer = <ServerStartTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_err(), "expected err: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <ServerStartTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        assert_eq!(stderr, "");
        assert_eq!(receiver.await, Ok(()));
    }

    #[fuchsia::test]
    async fn test_start_wrong_port() {
        let test_env = ffx_config::test_init().await.expect("test initialization");

        let address = (Ipv4Addr::LOCALHOST, 1234).into();
        test_env
            .context
            .query("repository.server.listen")
            .level(Some(ffx_config::ConfigLevel::User))
            .set("127.0.0.1:4321".into())
            .await
            .unwrap();

        let (sender, receiver) = channel();
        let mut sender = Some(sender);
        let repos = fho::testing::fake_proxy(move |req| match req {
            RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Ok(&SocketAddress(address).into())).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        });
        let tool = ServerStartTool { cmd: StartCommand { address: None }, repos };
        let buffers = TestBuffers::default();
        let writer = <ServerStartTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert!(res.is_err(), "expected err: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <ServerStartTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        assert_eq!(stderr, "");
        assert_eq!(receiver.await, Ok(()));
    }
}
