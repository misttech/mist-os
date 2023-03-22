// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use errors::ffx_error;
use ffx_core::ffx_plugin;
use ffx_daemon::DaemonConfig;
use ffx_daemon_start_args::StartCommand;

#[ffx_plugin()]
pub async fn daemon(cmd: StartCommand) -> Result<()> {
    // todo(fxb/108692) remove this use of the global hoist when we put the main one in the environment context
    // instead.
    let hoist = hoist::hoist();
    let context = ffx_config::global_env_context()
        .with_context(|| ffx_error!("No environment context loaded"))?;
    let ascendd_path = match cmd.path {
        Some(path) => path,
        None => context
            .get_ascendd_path()
            .await
            .with_context(|| ffx_error!("Could not load daemon socket path"))?,
    };
    let parent_dir =
        ascendd_path.parent().with_context(|| ffx_error!("Daemon socket path had no parent"))?;
    std::fs::create_dir_all(parent_dir).with_context(|| {
        ffx_error!(
            "Could not create directory for the daemon socket ({path})",
            path = parent_dir.display()
        )
    })?;
    let mut daemon = ffx_daemon::Daemon::new(ascendd_path);
    daemon.start(hoist).await
}
