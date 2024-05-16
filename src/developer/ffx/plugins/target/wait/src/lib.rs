// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::FfxError;
use ffx_target::KnockError;
use ffx_wait_args::WaitCommand;
use fho::{daemon_protocol, Error, FfxContext, FfxMain, FfxTool, Result, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx::{DaemonError, TargetCollectionProxy};
use fuchsia_async::WakeupTime;
use futures::future::Either;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DOWN_REPOLL_DELAY_MS: u64 = 500;
const OPEN_TARGET_TIMEOUT: Duration = Duration::from_millis(1000);

#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successfully waited for the target (either to come up or shut down).
    Ok {},
    /// Unexpected error with string denoting error message.
    UnexpectedError { message: String },
    /// A known error that can be reported to the user.
    UserError { message: String },
}

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
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match wait_for_device(self.target_collection_proxy, self.cmd).await {
            Ok(()) => {
                writer.machine(&CommandStatus::Ok {})?;
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

async fn wait_for_device(target_collection: TargetCollectionProxy, cmd: WaitCommand) -> Result<()> {
    let ffx: ffx_command::Ffx = argh::from_env();
    let default_target = ffx.target().await.bug()?;
    let knock_fut = async {
        loop {
            break match ffx_target::knock_target_by_name(
                &default_target,
                &target_collection,
                OPEN_TARGET_TIMEOUT,
                ffx_target::DEFAULT_RCS_KNOCK_TIMEOUT,
            )
            .await
            {
                Err(KnockError::CriticalError(e)) => Err(e),
                Err(KnockError::NonCriticalError(e)) => {
                    if cmd.down {
                        Ok(())
                    } else {
                        tracing::debug!("unable to knock target: {:?}", e);
                        continue;
                    }
                }
                Ok(()) => {
                    if cmd.down {
                        async_io::Timer::at(
                            Duration::from_millis(DOWN_REPOLL_DELAY_MS).into_time(),
                        )
                        .await;
                        continue;
                    } else {
                        Ok(())
                    }
                }
            };
        }
    };
    futures_lite::pin!(knock_fut);
    let timeout_fut = match cmd.timeout {
        0 => async_io::Timer::never(),
        _ => async_io::Timer::at(Duration::from_secs(cmd.timeout as u64).into_time()),
    };
    let timeout_err =
        FfxError::DaemonError { err: DaemonError::Timeout, target: ffx.target.clone() };
    match futures::future::select(knock_fut, timeout_fut).await {
        Either::Left((left, _)) => Ok(left.bug()?),
        Either::Right(_) => Err(Error::User(timeout_err.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fho::{Format, TestBuffers};
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

    fn setup_fake_target_collection_server(
        target_responsive: [bool; 2],
        rcs_responsive: bool,
    ) -> TargetCollectionProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<TargetCollectionMarker>().unwrap();
        let mut target_responsive = target_responsive.into_iter();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    TargetCollectionRequest::OpenTarget { responder, target_handle, .. } => {
                        if target_responsive.next().expect("Not enough target responses?") {
                            spawn_target_handler(
                                target_handle.into_stream().unwrap(),
                                rcs_responsive,
                            );
                            responder.send(Ok(())).unwrap();
                        } else {
                            std::future::pending::<()>().await;
                        }
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

    #[fuchsia::test]
    async fn peer_closed_to_target_collection_causes_top_level_error() {
        let _env = ffx_config::test_init().await.expect("test env");
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server_auto_close(),
            cmd: WaitCommand { timeout: 1000, down: false },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_err(), "expected error {stdout} {stderr}");
        assert!(
            matches!(res, Err(Error::Unexpected(_))),
            "expected 'unexpected error' {stdout} {stderr}"
        );
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn peer_closed_to_target_collection_causes_knock_error() {
        let _env = ffx_config::test_init().await.unwrap();
        let ffx: ffx_command::Ffx = argh::from_env();
        let tc_proxy = setup_fake_target_collection_server_auto_close();
        match ffx_target::knock_target_by_name(
            &ffx.target().await.unwrap(),
            &tc_proxy,
            OPEN_TARGET_TIMEOUT,
            ffx_target::DEFAULT_RCS_KNOCK_TIMEOUT,
        )
        .await
        .unwrap_err()
        {
            KnockError::CriticalError(_) => {}
            KnockError::NonCriticalError(e) => {
                panic!("should not have received non-critical error, but did: {:?}", e)
            }
        }
    }

    #[fuchsia::test]
    async fn able_to_connect_to_device() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server([true, true], true),
            cmd: WaitCommand { timeout: 5, down: false },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn unable_to_connect_to_device() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server([true, true], false),
            cmd: WaitCommand { timeout: 2, down: false },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_err(), "expected error: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn peer_closed_causes_down_error() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server_auto_close(),
            cmd: WaitCommand { timeout: 2, down: true },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_err(), "expected error: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn wait_for_down() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server([true, true], false),
            cmd: WaitCommand { timeout: 10, down: true },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn wait_for_down_when_never_up() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server([false, false], false),
            cmd: WaitCommand { timeout: 2, down: true },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn wait_for_down_when_after_up() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server([true, false], true),
            // We actually _have_ to wait for a few seconds, because
            // knock_rcs_impl will take 1 second before returning, and
            // we also wait for 500me in the wait_for_device() loop.
            // Any shorter a time and we're just going to hit the
            // timeout.
            cmd: WaitCommand { timeout: 10, down: true },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_ok(), "expected ok: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn wait_for_down_when_able_to_connect_to_device() {
        let _env = ffx_config::test_init().await.unwrap();
        let tool = WaitTool {
            target_collection_proxy: setup_fake_target_collection_server([true, true], true),
            cmd: WaitCommand { timeout: 3, down: true },
        };
        let test_buffers = TestBuffers::default();
        let writer =
            <WaitTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &test_buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_err(), "expected error: {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitTool as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }
}
