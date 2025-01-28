// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use component_debug::cli::show_cmd_print;
use ffx_session_show_args::SessionShowCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use target_holders::RemoteControlProxyHolder;

const DETAILS_FAILURE: &str = "Could not get session information from the target. This may be
because there are no running sessions, or because the target is using a product configuration
that does not include `session_manager`.";

#[derive(FfxTool)]
pub struct ShowTool {
    #[command]
    cmd: SessionShowCommand,
    rcs: RemoteControlProxyHolder,
}

fho::embedded_plugin!(ShowTool);

#[async_trait(?Send)]
impl FfxMain for ShowTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        show_impl(self.rcs, self.cmd, &mut writer).await?;
        Ok(())
    }
}

async fn show_impl<W: std::io::Write>(
    rcs_proxy: RemoteControlProxyHolder,
    _cmd: SessionShowCommand,
    writer: &mut W,
) -> Result<()> {
    let query_proxy = rcs::root_realm_query(&rcs_proxy, std::time::Duration::from_secs(15))
        .await
        .context("opening realm query")?;
    let with_style = termion::is_tty(&std::io::stdout());
    show_cmd_print(
        "core/session-manager/session:session".to_string(),
        query_proxy,
        writer,
        with_style,
    )
    .await
    .context(DETAILS_FAILURE)?;

    Ok(())
}
