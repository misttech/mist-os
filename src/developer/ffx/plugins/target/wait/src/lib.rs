// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use errors::FfxError;
use ffx_target::KnockError;
use ffx_wait_args::WaitCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, SimpleWriter};
use fidl::endpoints::create_proxy;
use fidl_fuchsia_developer_ffx::{DaemonError, TargetCollectionProxy, TargetMarker, TargetQuery};
use fuchsia_async::WakeupTime;
use futures::future::Either;
use std::time::Duration;

#[derive(FfxTool)]
pub struct WaitTool {
    #[command]
    cmd: WaitCommand,
    #[with(daemon_protocol())]
    target_collection_proxy: TargetCollectionProxy,
}

fho::embedded_plugin!(WaitTool);

#[async_trait(?Send)]
impl FfxMain for WaitTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        wait_for_device(self.target_collection_proxy, self.cmd).await?;
        Ok(())
    }
}

async fn wait_for_device(target_collection: TargetCollectionProxy, cmd: WaitCommand) -> Result<()> {
    let ffx: ffx_command::Ffx = argh::from_env();
    let knock_fut = async {
        loop {
            break match knock_target(&ffx, &target_collection).await {
                Err(KnockError::CriticalError(e)) => Err(e),
                Err(KnockError::NonCriticalError(e)) => {
                    tracing::debug!("unable to knock target: {:?}", e);
                    continue;
                }
                Ok(()) => Ok(()),
            };
        }
    };
    futures_lite::pin!(knock_fut);
    let timeout_fut = match cmd.timeout {
        0 => async_io::Timer::never(),
        _ => async_io::Timer::at(Duration::from_secs(cmd.timeout as u64).into_time()),
    };

    let is_default_target = ffx.target.is_none();
    let timeout_err = FfxError::DaemonError {
        err: DaemonError::Timeout,
        target: ffx.target.clone(),
        is_default_target,
    };
    match futures::future::select(knock_fut, timeout_fut).await {
        Either::Left((left, _)) => left,
        Either::Right(_) => Err(timeout_err.into()),
    }
}

async fn knock_target(
    ffx: &ffx_command::Ffx,
    target_collection_proxy: &TargetCollectionProxy,
) -> Result<(), KnockError> {
    let default_target = ffx.target().await?;
    let (target_proxy, target_remote) =
        create_proxy::<TargetMarker>().map_err(|e| KnockError::NonCriticalError(e.into()))?;
    // If you are reading this plugin for example code, this is an example of what you
    // should generally not be doing to connect to a daemon protocol. This is maintained
    // by the FFX team directly.
    target_collection_proxy
        .open_target(
            &TargetQuery { string_matcher: default_target.clone(), ..Default::default() },
            target_remote,
        )
        .await
        .map_err(|e| {
            KnockError::CriticalError(
                errors::ffx_error!("Lost connection to the Daemon. Full context:\n{}", e).into(),
            )
        })?
        .map_err(|e| {
            KnockError::CriticalError(errors::ffx_error!("Error opening target: {:?}", e).into())
        })?;
    ffx_target::knock_target(&target_proxy).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_developer_ffx::{
        TargetCollectionMarker, TargetCollectionRequest, TargetRequest, TargetRequestStream,
    };
    use fidl_fuchsia_developer_remotecontrol::{RemoteControlRequest, RemoteControlRequestStream};
    use futures::TryStreamExt;

    fn spawn_remote_control(mut rcs_stream: RemoteControlRequestStream) {
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = rcs_stream.try_next().await {
                match req {
                    RemoteControlRequest::OpenCapability { responder, server_channel, .. } => {
                        fuchsia_async::Task::local(async move {
                            // just hold the channel open to make the test succeed. No need to
                            // actually use it.
                            let _service_chan = server_channel;
                            std::future::pending::<()>().await;
                        })
                        .detach();
                        responder.send(Ok(())).unwrap();
                    }
                    e => panic!("unexpected request: {:?}", e),
                }
            }
        })
        .detach();
    }

    fn spawn_target_handler(mut target_stream: TargetRequestStream, responsive_rcs: bool) {
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = target_stream.try_next().await {
                match req {
                    TargetRequest::OpenRemoteControl { responder, remote_control, .. } => {
                        if responsive_rcs {
                            spawn_remote_control(remote_control.into_stream().unwrap());
                            responder.send(Ok(())).expect("responding to open rcs")
                        } else {
                            std::future::pending::<()>().await;
                        }
                    }
                    e => panic!("got unexpected req: {:?}", e),
                }
            }
        })
        .detach();
    }

    fn setup_fake_target_collection_server(responsive_rcs: bool) -> TargetCollectionProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<TargetCollectionMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    TargetCollectionRequest::OpenTarget { responder, target_handle, .. } => {
                        spawn_target_handler(target_handle.into_stream().unwrap(), responsive_rcs);
                        responder.send(Ok(())).unwrap();
                    }
                    e => panic!("unexpected request: {:?}", e),
                }
            }
        })
        .detach();
        proxy
    }

    /// Sets up a target collection that will automatically close out creating a PEER_CLOSED error.
    fn setup_fake_target_collection_server_auto_close() -> TargetCollectionProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<TargetCollectionMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    TargetCollectionRequest::OpenTarget { .. } => {
                        // Do nothing. This will immediately drop the responder struct causing a
                        // PEER_CLOSED error.
                    }
                    e => panic!("unexpected request: {:?}", e),
                }
            }
        })
        .detach();
        proxy
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn peer_closed_to_target_collection_causes_top_level_error() {
        let _env = ffx_config::test_init().await.unwrap();
        assert!(wait_for_device(
            setup_fake_target_collection_server_auto_close(),
            WaitCommand { timeout: 1000 }
        )
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn peer_closed_to_target_collection_causes_knock_error() {
        let _env = ffx_config::test_init().await.unwrap();
        let ffx: ffx_command::Ffx = argh::from_env();
        let tc_proxy = setup_fake_target_collection_server_auto_close();
        match knock_target(&ffx, &tc_proxy).await.unwrap_err() {
            KnockError::CriticalError(_) => {}
            KnockError::NonCriticalError(e) => {
                panic!("should not have received non-critical error, but did: {:?}", e)
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn able_to_connect_to_device() {
        let _env = ffx_config::test_init().await.unwrap();
        assert!(wait_for_device(
            setup_fake_target_collection_server(true),
            WaitCommand { timeout: 5 }
        )
        .await
        .is_ok());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn unable_to_connect_to_device() {
        let _env = ffx_config::test_init().await.unwrap();
        assert!(wait_for_device(
            setup_fake_target_collection_server(false),
            WaitCommand { timeout: 5 }
        )
        .await
        .is_err());
    }
}
