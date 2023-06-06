// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use chrono::Local;
use ffx_version_args::VersionCommand;
use ffx_writer::{MachineWriter, ToolIO};
use fho::FfxContext;
use fho::{Deferred, FfxMain, FfxTool, Result};
use fidl_fuchsia_developer_ffx::{self as ffx};
use std::time::Duration;
use timeout::timeout;

mod serialization;

use serialization::*;

const DEFAULT_DAEMON_TIMEOUT_MS: u64 = 1500;

#[derive(FfxTool)]
pub struct VersionTool {
    #[command]
    cmd: VersionCommand,
    build_info: ffx::VersionInfo,
    daemon_proxy: Deferred<ffx::DaemonProxy>,
}

fho::embedded_plugin!(VersionTool);

#[async_trait(?Send)]
impl FfxMain for VersionTool {
    type Writer = MachineWriter<Versions>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        let tool_version = self.build_info.into();
        let daemon_version = if self.cmd.verbose {
            Some(get_daemon_version(self.daemon_proxy.await?).await?.into())
        } else {
            None
        };
        let versions = Versions { tool_version, daemon_version };

        if writer.is_machine() {
            Ok(writer.machine(&versions)?)
        } else {
            format_versions(&versions, self.cmd.verbose, &mut writer, Local)
        }
    }
}

async fn get_daemon_version(proxy: ffx::DaemonProxy) -> Result<VersionInfo> {
    timeout(Duration::from_millis(DEFAULT_DAEMON_TIMEOUT_MS), proxy.get_version_info())
        .await
        .user_message("Timed out trying to get daemon version info")?
        .user_message("Failed to get daemon version info")
        .map(VersionInfo::from)
}

#[cfg(test)]
pub(crate) mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::DaemonRequest;
    use futures::{
        channel::oneshot::{self, Receiver},
        future::Shared,
        FutureExt, TryStreamExt,
    };

    pub const FAKE_DAEMON_HASH: &str = "fake daemon fake";
    pub const FAKE_FRONTEND_HASH: &str = "fake frontend fake";
    pub const FAKE_DAEMON_BUILD_VERSION: &str = "fake daemon build";
    pub const FAKE_FRONTEND_BUILD_VERSION: &str = "fake frontend build";
    pub const TIMESTAMP: u64 = 1604080617;
    pub const TIMESTAMP_STR: &str = "Fri, 30 Oct 2020 17:56:57 +0000";
    pub const FAKE_ABI_REVISION: u64 = 17063755220075245312;
    pub const ABI_REVISION_STR: &str = "0xECCEA2F70ACD6F00";
    pub const FAKE_API_LEVEL: u64 = 7;

    pub fn daemon_info() -> VersionInfo {
        VersionInfo {
            commit_hash: Some(FAKE_DAEMON_HASH.to_string()),
            commit_timestamp: Some(TIMESTAMP),
            build_version: Some(FAKE_DAEMON_BUILD_VERSION.to_string()),
            abi_revision: Some(FAKE_ABI_REVISION),
            api_level: Some(FAKE_API_LEVEL),
            ..Default::default()
        }
    }

    pub fn frontend_info() -> VersionInfo {
        VersionInfo {
            commit_hash: Some(FAKE_FRONTEND_HASH.to_string()),
            commit_timestamp: Some(TIMESTAMP),
            build_version: Some(FAKE_FRONTEND_BUILD_VERSION.to_string()),
            abi_revision: Some(FAKE_ABI_REVISION),
            api_level: Some(FAKE_API_LEVEL),
            ..Default::default()
        }
    }

    fn setup_fake_daemon_server(succeed: bool, info: VersionInfo) -> ffx::DaemonProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<ffx::DaemonMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let info = info.into();
            #[allow(clippy::never_loop)]
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    DaemonRequest::GetVersionInfo { responder } => {
                        if succeed {
                            responder.send(&info).unwrap();
                        } else {
                            return;
                        }
                    }
                    _ => assert!(false),
                }
                // We should only get one request per stream. We want subsequent calls to fail if more are
                // made.
                break;
            }
        })
        .detach();

        proxy
    }

    fn setup_hanging_daemon_server(waiter: Shared<Receiver<()>>) -> ffx::DaemonProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<ffx::DaemonMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            #[allow(clippy::never_loop)]
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    DaemonRequest::GetVersionInfo { responder: _ } => {
                        waiter.await.unwrap();
                    }
                    _ => assert!(false),
                }
                // We should only get one request per stream. We want subsequent calls to fail if more are
                // made.
                break;
            }
        })
        .detach();

        proxy
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_daemon_succeeds() {
        let proxy = setup_fake_daemon_server(true, daemon_info());
        assert_eq!(daemon_info(), get_daemon_version(proxy).await.unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_daemon_fails() {
        let expected_output = "Failed to get daemon version info";
        let proxy = setup_fake_daemon_server(false, daemon_info());
        match get_daemon_version(proxy).await.unwrap_err() {
            fho::Error::User(err) => assert_eq!(&err.to_string(), expected_output),
            other => panic!("Expected '{expected_output}' user error, got: {other}"),
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_daemon_hangs() {
        let expected_output = "Timed out trying to get daemon version info";
        let (tx, rx) = oneshot::channel::<()>();
        let proxy = setup_hanging_daemon_server(rx.shared());
        let daemon_version = get_daemon_version(proxy).await;
        tx.send(()).unwrap();

        match daemon_version.unwrap_err() {
            fho::Error::User(err) => assert_eq!(&err.to_string(), expected_output),
            other => panic!("Expected '{expected_output}' user error, got: {other}"),
        }
    }
}
