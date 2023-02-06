// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_error;
use ffx_core::ffx_plugin;

pub use ffx_repository_publish_args::RepoPublishCommand;

#[ffx_plugin()]
pub async fn cmd_repo_publish(cmd: RepoPublishCommand) -> Result<()> {
    package_tool::cmd_repo_publish(cmd).await.map_err(|err| ffx_error!(err))?;
    Ok(())
}
