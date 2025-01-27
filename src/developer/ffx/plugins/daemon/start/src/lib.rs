// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_daemon::DaemonConfig;
use ffx_daemon_start_args::StartCommand;
use fho::{user_error, FfxContext, FfxMain, FfxTool, FhoEnvironment, TryFromEnv};
use target_holders::DaemonProxyHolder;

// The field in this tuple is never read because getting a daemon proxy is sufficient enough to
// determine that a connection is valid (this requires a version negotiation handshake).
pub struct DefaultDaemonProvider(#[allow(dead_code)] DaemonProxyHolder);

#[async_trait(?Send)]
impl TryFromEnv for DefaultDaemonProvider {
    async fn try_from_env(env: &FhoEnvironment) -> fho::Result<Self> {
        Ok(Self(DaemonProxyHolder::try_from_env(env).await?))
    }
}

#[derive(FfxTool)]
pub struct DaemonStartTool<T: TryFromEnv + 'static> {
    #[command]
    cmd: StartCommand,
    context: EnvironmentContext,
    daemon_checker: fho::Deferred<T>,
}

fho::embedded_plugin!(DaemonStartTool<DefaultDaemonProvider>);
const CIRCUIT_REFRESH_RATE: std::time::Duration = std::time::Duration::from_millis(500);

#[async_trait::async_trait(?Send)]
impl<T: TryFromEnv + 'static> FfxMain for DaemonStartTool<T> {
    type Writer = fho::SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        if self.cmd.background {
            tracing::debug!("invoking daemon background start");
            drop(self.daemon_checker.await.user_message("Unable to connect to daemon proxy")?);
            return Ok(());
        }
        tracing::debug!("in daemon start main");
        let node = overnet_core::Router::new(Some(CIRCUIT_REFRESH_RATE))
            .user_message("Failed to initialize overnet")?;
        let ascendd_path = match self.cmd.path {
            Some(path) => path,
            None => self
                .context
                .get_ascendd_path()
                .await
                .user_message("Could not load daemon socket path")?,
        };
        let parent_dir =
            ascendd_path.parent().ok_or_else(|| user_error!("Daemon socket path had no parent"))?;
        tracing::debug!("creating daemon socket dir");
        std::fs::create_dir_all(parent_dir).with_user_message(|| {
            format!(
                "Could not create directory for the daemon socket ({path})",
                path = parent_dir.display()
            )
        })?;
        tracing::debug!("creating daemon");
        let mut daemon = ffx_daemon_server::Daemon::new(ascendd_path);
        daemon.start(node).await.bug()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    struct AutoSucceedFakeDaemonProvider;

    #[async_trait(?Send)]
    impl TryFromEnv for AutoSucceedFakeDaemonProvider {
        async fn try_from_env(_env: &FhoEnvironment) -> fho::Result<Self> {
            Ok(Self)
        }
    }

    struct AutoFailFakeDaemonProvider;

    #[async_trait(?Send)]
    impl TryFromEnv for AutoFailFakeDaemonProvider {
        async fn try_from_env(_env: &FhoEnvironment) -> fho::Result<Self> {
            fho::return_user_error!("Oh no it broke!");
        }
    }

    #[fuchsia::test]
    async fn test_background_succeeds_when_daemon_connection_established() {
        let config_env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { path: None, background: true };
        let tool_env = fho::testing::ToolEnv::new().make_environment(config_env.context.clone());
        let tool = DaemonStartTool {
            cmd,
            context: config_env.context.clone(),
            daemon_checker: fho::Deferred::<AutoSucceedFakeDaemonProvider>::try_from_env(&tool_env)
                .await
                .unwrap(),
        };
        let test_buffers = fho::TestBuffers::default();
        let writer = fho::SimpleWriter::new_test(&test_buffers);
        assert!(tool.main(writer).await.is_ok());
    }

    #[fuchsia::test]
    async fn test_background_fails_when_daemon_connection_fails() {
        let config_env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { path: None, background: true };
        let tool_env = fho::testing::ToolEnv::new().make_environment(config_env.context.clone());
        let tool = DaemonStartTool {
            cmd,
            context: config_env.context.clone(),
            daemon_checker: fho::Deferred::<AutoFailFakeDaemonProvider>::try_from_env(&tool_env)
                .await
                .unwrap(),
        };
        let test_buffers = fho::TestBuffers::default();
        let writer = fho::SimpleWriter::new_test(&test_buffers);
        assert!(tool.main(writer).await.is_err());
    }
}
