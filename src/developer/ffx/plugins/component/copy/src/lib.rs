// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use component_debug::copy::copy_cmd;
use errors::ffx_bail;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_copy_args::CopyComponentCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;

#[derive(FfxTool)]
pub struct CopyTool {
    #[command]
    cmd: CopyComponentCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(CopyTool);

#[async_trait(?Send)]
impl FfxMain for CopyTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let query_proxy = connect_to_realm_query(&self.rcs).await?;
        let CopyComponentCommand { paths, verbose } = self.cmd;

        match copy_cmd(&query_proxy, paths, verbose, writer).await {
            Ok(_) => Ok(()),
            Err(e) => ffx_bail!("{}", e),
        }
    }
}
