// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_daemon::DaemonConfig;
use ffx_daemon_start_args::StartCommand;
use fho::{user_error, FfxContext, FfxMain, FfxTool, FhoEnvironment};
use target_holders::DaemonProxyHolder;

#[derive(FfxTool)]
pub struct DaemonStartTool {
    #[command]
    cmd: StartCommand,
    fho_env: FhoEnvironment,
}

fho::embedded_plugin!(DaemonStartTool);
const CIRCUIT_REFRESH_RATE: std::time::Duration = std::time::Duration::from_millis(500);

#[async_trait(?Send)]
impl FfxMain for DaemonStartTool {
    type Writer = ffx_writer::SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        if self.cmd.background {
            return self.start_in_background().await;
        }
        let node = overnet_core::Router::new(Some(CIRCUIT_REFRESH_RATE))
            .user_message("Failed to initialize overnet")?;
        let ascendd_path = match self.cmd.path {
            Some(path) => path,
            None => self
                .fho_env
                .environment_context()
                .get_ascendd_path()
                .await
                .user_message("Could not load daemon socket path")?,
        };
        let parent_dir =
            ascendd_path.parent().ok_or_else(|| user_error!("Daemon socket path had no parent"))?;
        log::debug!("creating daemon socket dir");
        std::fs::create_dir_all(parent_dir).with_user_message(|| {
            format!(
                "Could not create directory for the daemon socket ({path})",
                path = parent_dir.display()
            )
        })?;
        log::debug!("creating daemon");
        let mut daemon = ffx_daemon_server::Daemon::new(ascendd_path);
        daemon.start(node).await.bug()
    }
}

impl DaemonStartTool {
    async fn start_in_background(self) -> fho::Result<()> {
        log::debug!("invoking daemon background start");
        let target_env = ffx_target::fho::target_interface(&self.fho_env);
        if target_env.behavior().is_none() {
            let env_context = self.fho_env.environment_context();
            let b = target_holders::init_daemon_connection_behavior(env_context).await?;
            target_env.set_behavior(b)?;
        }
        let _ = target_env
            .injector::<DaemonProxyHolder>(&self.fho_env)
            .await?
            .daemon_factory_force_autostart()
            .await
            .user_message("Unable to connect to daemon proxy")?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::EnvironmentContext;
    use ffx_target::fho::{target_interface, FhoConnectionBehavior};
    use std::sync::Arc;
    use target_holders::{fake_proxy, FakeInjector};

    fn create_fake_injector_with_result(
        context: &EnvironmentContext,
        succeeds: bool,
    ) -> FhoEnvironment {
        let fake_injector = FakeInjector {
            daemon_factory_force_autostart_closure: Box::new(move || {
                Box::pin(async move {
                    if succeeds {
                        let fake_daemon_proxy = fake_proxy(|_req| unimplemented!());
                        Ok(fake_daemon_proxy)
                    } else {
                        anyhow::bail!("daemon failed")
                    }
                })
            }),
            ..Default::default()
        };

        let fho_env = FhoEnvironment::new_with_args(context, &["some", "test"]);
        let target_env = target_interface(&fho_env);
        target_env
            .set_behavior(FhoConnectionBehavior::DaemonConnector(Arc::new(fake_injector)))
            .expect("set_behavior");
        fho_env
    }

    #[fuchsia::test]
    async fn test_background_start_succeeds() {
        let config_env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { path: None, background: true };
        let fho_env = create_fake_injector_with_result(&config_env.context, true);
        let tool = DaemonStartTool { cmd, fho_env };
        let test_buffers = ffx_writer::TestBuffers::default();
        let writer = ffx_writer::SimpleWriter::new_test(&test_buffers);
        tool.main(writer).await.unwrap();
    }

    #[fuchsia::test]
    async fn test_background_start_fails() {
        let config_env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { path: None, background: true };
        let fho_env = create_fake_injector_with_result(&config_env.context, false);
        let tool = DaemonStartTool { cmd, fho_env };
        let test_buffers = ffx_writer::TestBuffers::default();
        let writer = ffx_writer::SimpleWriter::new_test(&test_buffers);
        tool.main(writer).await.unwrap_err();
    }
}
