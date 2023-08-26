// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config::EnvironmentContext;
use ffx_daemon::DaemonConfig;
use ffx_daemon_start_args::StartCommand;
use fho::{user_error, FfxContext, FfxMain, FfxTool};

#[derive(FfxTool)]
pub struct DaemonStartTool {
    #[command]
    cmd: StartCommand,
    context: EnvironmentContext,
}

fho::embedded_plugin!(DaemonStartTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for DaemonStartTool {
    type Writer = fho::SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        tracing::debug!("in daemon start main");
        // todo(fxb/108692) remove this use of the global hoist when we put the main one in the environment context
        // instead.
        let hoist = hoist::hoist();
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
        daemon.start(hoist).await.bug()
    }
}
