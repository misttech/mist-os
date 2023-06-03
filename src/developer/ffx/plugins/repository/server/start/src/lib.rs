// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_repository_server_start_args::StartCommand;
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx::RepositoryRegistryProxy;
use fidl_fuchsia_developer_ffx_ext::RepositoryError;
use fidl_fuchsia_net_ext::SocketAddress;
use pkg::config as pkg_config;
use std::io::Write as _;

#[ffx_plugin(RepositoryRegistryProxy = "daemon::protocol")]
pub async fn start(
    cmd: StartCommand,
    repos: RepositoryRegistryProxy,
    #[ffx(machine = SocketAddress)] mut writer: Writer,
) -> Result<()> {
    start_impl(cmd, repos, &mut writer).await
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct ServerInfo {
    address: std::net::SocketAddr,
}

async fn start_impl(
    cmd: StartCommand,
    repos: RepositoryRegistryProxy,
    writer: &mut Writer,
) -> Result<()> {
    let listen_address = match {
        if let Some(addr_flag) = cmd.address {
            Ok(Some(addr_flag))
        } else {
            pkg_config::repository_listen_addr().await
        }
    } {
        Ok(Some(address)) => address,
        Ok(None) => {
            ffx_bail!(
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
            ffx_bail!(
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
        .context("communicating with daemon")?
        .map_err(RepositoryError::from)
    {
        Ok(address) => {
            let address = SocketAddress::from(address);

            // Error out if the server is listening on a different address. Either we raced some
            // other `start` command, or the server was already running, and someone changed the
            // `repository.server.listen` address without then stopping the server.
            if listen_address.port() != 0 && listen_address != address.0 {
                ffx_bail!(
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

            if writer.is_machine() {
                writer.machine(&ServerInfo { address: address.0 })?;
            } else {
                writeln!(writer, "Repository server is listening on {}", address)?;
            }

            Ok(())
        }
        Err(err @ RepositoryError::ServerAddressAlreadyInUse) => {
            ffx_bail!("Failed to start repository server on {}: {}", listen_address, err)
        }
        Err(RepositoryError::ServerNotRunning) => {
            ffx_bail!(
                "Failed to start repository server on {}: {:#}",
                listen_address,
                pkg::config::determine_why_repository_server_is_not_running().await
            )
        }
        Err(err) => {
            ffx_bail!("Failed to start repository server on {}: {}", listen_address, err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_developer_ffx::{RepositoryError, RepositoryRegistryRequest};
    use fidl_fuchsia_net as fidl;
    use futures::channel::oneshot::channel;
    use std::{future::Future, net::Ipv4Addr};

    // FIXME(http://fxbug.dev/80740): Unfortunately ffx_config is global, and so each of these tests
    // could step on each others ffx_config entries if run in parallel. To avoid this, we will:
    //
    // * use the `serial_test` crate to make sure each test runs sequentially
    // * clear out the config keys before we run each test to make sure state isn't leaked across
    //   tests.
    fn run_async_test<F: Future>(fut: F) -> F::Output {
        fuchsia_async::TestExecutor::new().run_singlethreaded(async move {
            let _env = ffx_config::test_init().await.unwrap();
            fut.await
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_start() {
        run_async_test(async {
            let mut writer = Writer::new_test(None);

            let address = (Ipv4Addr::LOCALHOST, 1234).into();
            ffx_config::query("repository.server.listen")
                .level(Some(ffx_config::ConfigLevel::User))
                .set("127.0.0.1:1234".into())
                .await
                .unwrap();

            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Ok(&SocketAddress(address).into())).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            start_impl(StartCommand { address: None }, repos, &mut writer).await.unwrap();
            assert_eq!(receiver.await, Ok(()));
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_start_runtime_port() {
        run_async_test(async {
            let mut writer = Writer::new_test(None);

            let address = (Ipv4Addr::LOCALHOST, 8084).into();

            let _test = fidl::SocketAddress::Ipv4(fidl::Ipv4SocketAddress {
                address: fidl::Ipv4Address { addr: [1, 2, 3, 4] },
                port: 5,
            });

            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStart { responder, address: Some(_test) } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Ok(&SocketAddress(address).into())).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            start_impl(
                StartCommand { address: Some("127.0.0.1:8084".parse().unwrap()) },
                repos,
                &mut writer,
            )
            .await
            .unwrap();
            assert_eq!(receiver.await, Ok(()));
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_start_machine() {
        run_async_test(async {
            let mut writer = Writer::new_test(Some(ffx_writer::Format::Json));

            let address = (Ipv4Addr::LOCALHOST, 1234).into();
            ffx_config::query("repository.server.listen")
                .level(Some(ffx_config::ConfigLevel::User))
                .set("127.0.0.1:1234".into())
                .await
                .unwrap();

            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Ok(&SocketAddress(address).into())).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            start_impl(StartCommand { address: None }, repos, &mut writer).await.unwrap();
            assert_eq!(receiver.await, Ok(()));

            let info: ServerInfo = serde_json::from_str(&writer.test_output().unwrap()).unwrap();
            assert_eq!(info, ServerInfo { address },);
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_start_failed() {
        run_async_test(async {
            let mut writer = Writer::new_test(None);

            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            assert!(start_impl(StartCommand { address: None }, repos, &mut writer).await.is_err());
            assert_eq!(receiver.await, Ok(()));
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_start_wrong_port() {
        run_async_test(async {
            let mut writer = Writer::new_test(None);

            let address = (Ipv4Addr::LOCALHOST, 1234).into();
            ffx_config::query("repository.server.listen")
                .level(Some(ffx_config::ConfigLevel::User))
                .set("127.0.0.1:4321".into())
                .await
                .unwrap();

            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Ok(&SocketAddress(address).into())).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            assert!(start_impl(StartCommand { address: None }, repos, &mut writer).await.is_err());
            assert_eq!(receiver.await, Ok(()));
        })
    }
}
