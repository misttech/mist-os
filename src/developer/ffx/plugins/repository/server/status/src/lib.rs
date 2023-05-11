// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_core::ffx_plugin,
    ffx_repository_server_status_args::StatusCommand,
    ffx_writer::Writer,
    fidl_fuchsia_developer_ffx::RepositoryRegistryProxy,
    fidl_fuchsia_developer_ffx_ext::ServerStatus,
    std::{convert::TryFrom as _, io::Write},
};

#[ffx_plugin("ffx_repository", RepositoryRegistryProxy = "daemon::protocol")]
pub async fn status(
    _cmd: StatusCommand,
    repos: RepositoryRegistryProxy,
    #[ffx(machine = ServerStatus)] mut writer: Writer,
) -> Result<()> {
    status_impl(repos, &mut writer).await
}

async fn status_impl(repos: RepositoryRegistryProxy, writer: &mut Writer) -> Result<()> {
    let status = repos.server_status().await?;
    let status = ServerStatus::try_from(status)?.into();

    if writer.is_machine() {
        writer.machine(&status)?;
    } else {
        match status {
            ServerStatus::Disabled => {
                let err = pkg::config::determine_why_repository_server_is_not_running().await;
                writeln!(writer, "Server is disabled: {:?}", err)?
            }
            ServerStatus::Stopped => writeln!(writer, "Server is stopped")?,
            ServerStatus::Running { address } => {
                writeln!(writer, "Server is running on {}", address)?
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl_fuchsia_developer_ffx::RepositoryRegistryRequest,
        futures::channel::oneshot::channel,
        std::{future::Future, net::Ipv4Addr},
    };

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
    fn test_status() {
        run_async_test(async {
            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStatus { responder } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder
                        .send(
                            &ServerStatus::Running { address: (Ipv4Addr::LOCALHOST, 0).into() }
                                .into(),
                        )
                        .unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            let mut out = Writer::new_test(None);
            assert!(status_impl(repos, &mut out).await.is_ok());
            let () = receiver.await.unwrap();

            assert_eq!(&out.test_output().unwrap(), "Server is running on 127.0.0.1:0\n",);
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_status_disabled() {
        run_async_test(async {
            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStatus { responder } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(&ServerStatus::Disabled.into()).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            let mut out = Writer::new_test(None);
            assert!(status_impl(repos, &mut out).await.is_ok());
            let () = receiver.await.unwrap();

            assert_eq!(
                &out.test_output().unwrap(),
                "Server is disabled: Server is disabled. It can be started with:\n\
                $ ffx repository server start\n",
            );
        })
    }

    #[serial_test::serial]
    #[test]
    fn test_status_machine() {
        run_async_test(async {
            let (sender, receiver) = channel();
            let mut sender = Some(sender);
            let repos = setup_fake_repos(move |req| match req {
                RepositoryRegistryRequest::ServerStatus { responder } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder
                        .send(
                            &ServerStatus::Running { address: (Ipv4Addr::LOCALHOST, 0).into() }
                                .into(),
                        )
                        .unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            });

            let mut out = Writer::new_test(Some(ffx_writer::Format::Json));
            assert!(status_impl(repos, &mut out).await.is_ok());
            let () = receiver.await.unwrap();

            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&out.test_output().unwrap()).unwrap(),
                serde_json::json!({
                    "state": "running",
                    "address": "127.0.0.1:0",
                }),
            );
        })
    }
}
